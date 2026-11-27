//! Git through the `git` command-line tool (next phases, Phase 5).
//!
//! Every call is blocking; run it on a background thread. Reads run with
//! `GIT_OPTIONAL_LOCKS=0` so `git status` never rewrites the index (which
//! would wake the file watcher and refresh again, forever). Commands that
//! change the repository take the repository lock (`process::Cmd::locked`).
//! Every command runs with `GIT_TERMINAL_PROMPT=0`, so git never waits for
//! a password.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};

use crate::claude_env::EnvMap;
use crate::process::{self, CancelToken, Cmd, Output};

/// How network commands may run before they are stopped.
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(600);

/// How to run git.
#[derive(Clone, Debug)]
pub struct Git {
    pub program: PathBuf,
    pub env: Option<EnvMap>,
    /// Answers git's https password prompt for github.com with the
    /// signed-in GitHub token.
    pub askpass: Option<Askpass>,
}

/// `GIT_ASKPASS`: a small script (see `github_auth::write_askpass`) that
/// prints the token from `ANVEG_GIT_TOKEN`, only for github.com prompts.
/// The token travels in the environment, never on a command line.
#[derive(Clone, Debug)]
pub struct Askpass {
    pub script: PathBuf,
    pub token: crate::github_api::Secret,
}

impl Default for Git {
    fn default() -> Self {
        Self {
            program: PathBuf::from("git"),
            env: None,
            askpass: None,
        }
    }
}

impl Git {
    pub fn new(program: PathBuf, env: Option<EnvMap>) -> Self {
        Self {
            program,
            env,
            askpass: None,
        }
    }

    pub fn with_askpass(mut self, askpass: Option<Askpass>) -> Self {
        self.askpass = askpass;
        self
    }

    /// A git command in `repo`.
    pub fn cmd(&self, repo: &Path) -> Cmd {
        let mut cmd = Cmd::new(&self.program)
            .env(self.env.as_ref())
            .cwd(repo)
            .var("GIT_TERMINAL_PROMPT", "0")
            .var("GIT_OPTIONAL_LOCKS", "0")
            .args(["-c", "core.quotepath=off", "-c", "color.ui=false"]);
        if let Some(a) = &self.askpass {
            cmd = cmd
                .var("GIT_ASKPASS", a.script.to_string_lossy().into_owned())
                .var("ANVEG_GIT_TOKEN", a.token.expose());
        }
        cmd
    }

    /// A command that changes the repository: runs under its lock and may
    /// take optional locks.
    fn write_cmd(&self, repo: &Path) -> Cmd {
        let mut cmd = self.cmd(repo).locked(repo);
        cmd.extra_env.retain(|(k, _)| k != "GIT_OPTIONAL_LOCKS");
        cmd
    }

    fn read(&self, repo: &Path, args: &[&str]) -> Result<Output> {
        process::run(&self.cmd(repo).args(args.iter().copied()))
    }

    fn read_ok(&self, repo: &Path, args: &[&str]) -> Result<String> {
        Ok(self.read(repo, args)?.check()?.stdout_str())
    }

    fn write(&self, repo: &Path, args: &[&str]) -> Result<Output> {
        process::run(&self.write_cmd(repo).args(args.iter().copied()))?.check()
    }

    // -----------------------------------------------------------------
    // Repositories (5.2)
    // -----------------------------------------------------------------

    /// The top of the repository containing `dir`, if any.
    pub fn find_repo(&self, dir: &Path) -> Option<PathBuf> {
        let out = self.read(dir, &["rev-parse", "--show-toplevel"]).ok()?;
        out.success()
            .then(|| PathBuf::from(out.stdout_str().trim()))
            .filter(|p| !p.as_os_str().is_empty())
    }

    /// The repositories for a project's roots, then the repositories
    /// inside them (a folder with its own `.git`), without duplicates.
    pub fn find_repos_with_inner(&self, roots: &[PathBuf]) -> Vec<PathBuf> {
        let mut out = self.find_repos(roots);
        let mut i = 0;
        // Breadth first, three levels deep at most.
        let mut depth = vec![0usize; out.len()];
        while i < out.len() {
            if depth[i] < 3 {
                for inner in self.inner_repos(&out[i]) {
                    if !out.contains(&inner) {
                        out.push(inner);
                        depth.push(depth[i] + 1);
                    }
                }
            }
            i += 1;
        }
        out
    }

    /// Repositories inside `repo`: untracked folders with their own `.git`,
    /// and ones already recorded as a link (submodules, embedded repos).
    pub fn inner_repos(&self, repo: &Path) -> Vec<PathBuf> {
        let mut rel: Vec<String> = self
            .status(repo)
            .map(|s| s.inner_repos().map(str::to_owned).collect())
            .unwrap_or_default();
        rel.extend(self.gitlinks(repo));
        rel.into_iter()
            .map(|r| repo.join(r))
            .filter(|p| p.join(".git").exists())
            .collect()
    }

    /// Paths recorded in the index as links to other repositories.
    fn gitlinks(&self, repo: &Path) -> Vec<String> {
        let Ok(out) = self.read_ok(repo, &["ls-files", "--stage", "-z"]) else {
            return Vec::new();
        };
        out.split('\0')
            .filter(|l| l.starts_with("160000 "))
            .filter_map(|l| l.split_once('\t').map(|(_, p)| p.to_owned()))
            .collect()
    }

    /// Make the inner repository at `rel` an ordinary folder of `repo`:
    /// forget a recorded link to it, then `remove` its `.git` (the app
    /// moves it to the Trash). Its files then show as new in `repo`.
    pub fn absorb_inner_repo(
        &self,
        repo: &Path,
        rel: &str,
        remove: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<()> {
        let rel = rel.trim_end_matches('/');
        if self.gitlinks(repo).iter().any(|g| g == rel) {
            // --cached: only the index entry goes; -f: even when staged.
            self.write(repo, &["rm", "--cached", "-f", "-q", "--", rel])?;
        }
        let dot_git = repo.join(rel).join(".git");
        if !dot_git.exists() {
            anyhow::bail!("{} has no .git", repo.join(rel).display());
        }
        remove(&dot_git)
    }

    /// The repositories for a project's roots, without duplicates.
    pub fn find_repos(&self, roots: &[PathBuf]) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        for root in roots {
            if let Some(repo) = self.find_repo(root)
                && !out.contains(&repo)
            {
                out.push(repo);
            }
        }
        out
    }

    /// Linked worktrees: (path, branch).
    pub fn worktrees(&self, repo: &Path) -> Result<Vec<Worktree>> {
        Ok(parse_worktrees(&self.read_ok(
            repo,
            &["worktree", "list", "--porcelain"],
        )?))
    }

    /// The git directory (for watching `index`, `HEAD`, `refs`).
    pub fn git_dir(&self, repo: &Path) -> Result<PathBuf> {
        let out = self.read_ok(repo, &["rev-parse", "--absolute-git-dir"])?;
        Ok(PathBuf::from(out.trim()))
    }

    // -----------------------------------------------------------------
    // Status (5.3)
    // -----------------------------------------------------------------

    pub fn status(&self, repo: &Path) -> Result<Status> {
        let out = self
            .read(
                repo,
                &[
                    "status",
                    "--porcelain=v2",
                    "-z",
                    "--branch",
                    "--untracked-files=all",
                ],
            )?
            .check()?;
        Ok(parse_status(&out.stdout))
    }

    // -----------------------------------------------------------------
    // Diffs (5.4)
    // -----------------------------------------------------------------

    /// The diff of one file: staged (`--cached`) or unstaged. Untracked
    /// files come back as all-new.
    pub fn file_diff(&self, repo: &Path, path: &str, kind: DiffKind) -> Result<String> {
        let out = match kind {
            DiffKind::Unstaged => self.read(
                repo,
                &["diff", "--no-ext-diff", "--no-color", "--", path],
            )?,
            DiffKind::Staged => self.read(
                repo,
                &["diff", "--cached", "--no-ext-diff", "--no-color", "--", path],
            )?,
            DiffKind::Untracked => {
                // Exit code 1 means "differences", which is the point.
                let out = self.read(
                    repo,
                    &[
                        "diff",
                        "--no-index",
                        "--no-ext-diff",
                        "--no-color",
                        "--",
                        "/dev/null",
                        path,
                    ],
                )?;
                if out.code == Some(0) || out.code == Some(1) {
                    out
                } else {
                    out.check()?
                }
            }
        };
        if kind != DiffKind::Untracked {
            out.clone().check()?;
        }
        Ok(out.stdout_str())
    }

    /// The patch a commit introduced.
    pub fn commit_diff(&self, repo: &Path, sha: &str) -> Result<String> {
        self.read_ok(
            repo,
            &[
                "show",
                "--format=",
                "--no-ext-diff",
                "--no-color",
                "-m",
                "--first-parent",
                sha,
            ],
        )
    }

    // -----------------------------------------------------------------
    // Staging and discarding (5.5)
    // -----------------------------------------------------------------

    pub fn stage(&self, repo: &Path, paths: &[&str]) -> Result<()> {
        let mut args = vec!["add", "--"];
        args.extend_from_slice(paths);
        self.write(repo, &args).map(drop)
    }

    /// Stage `paths` and return how many files git added or updated
    /// (`git add -v`); 0 means there was nothing new or changed there.
    /// Ignored files are skipped, as `git add` does.
    pub fn add_counted(&self, repo: &Path, paths: &[&str]) -> Result<usize> {
        let mut args = vec!["add", "-v", "--"];
        args.extend_from_slice(paths);
        let out = self.write(repo, &args)?;
        Ok(out.stdout_str().lines().filter(|l| l.starts_with("add ")).count())
    }

    /// Stage `paths` even though `.gitignore` ignores them (`git add -f`).
    pub fn stage_force(&self, repo: &Path, paths: &[&str]) -> Result<()> {
        let mut args = vec!["add", "-f", "--"];
        args.extend_from_slice(paths);
        self.write(repo, &args).map(drop)
    }

    /// The rule that makes git ignore `path`, in words (e.g. "“build/”
    /// (line 3 of .gitignore)"), or None when it isn't ignored.
    pub fn ignore_rule(&self, repo: &Path, path: &str) -> Result<Option<String>> {
        let out = self.read(repo, &["check-ignore", "-v", "--", path])?;
        // Exit 1: not ignored.
        if out.code == Some(1) {
            return Ok(None);
        }
        let text = out.check()?.stdout_str();
        // "source:line:pattern<TAB>path"
        let rule = text.lines().next().and_then(|l| l.split('\t').next()).unwrap_or("");
        let mut parts = rule.splitn(3, ':');
        Ok(match (parts.next(), parts.next(), parts.next()) {
            (Some(file), Some(line), Some(pattern)) if !pattern.is_empty() => {
                Some(format!("“{pattern}” (line {line} of {file})"))
            }
            _ if !rule.is_empty() => Some(rule.to_owned()),
            _ => None,
        })
    }

    /// Files and folders git ignores that are not tracked (folders end in
    /// `/` and stand for everything inside): at most `max`, and the total.
    pub fn ignored(&self, repo: &Path, max: usize) -> Result<(Vec<String>, usize)> {
        let out = self
            .read(
                repo,
                &[
                    "status",
                    "--porcelain=v2",
                    "-z",
                    "--ignored=traditional",
                    "--untracked-files=normal",
                ],
            )?
            .check()?;
        Ok(parse_ignored(&out.stdout, max))
    }

    /// Stage every change and new file, except inner repositories (git
    /// would refuse one without commits, or record only a link to it).
    pub fn stage_all(&self, repo: &Path) -> Result<()> {
        let inner: Vec<String> = self
            .status(repo)
            .map(|s| s.inner_repos().map(|p| format!(":(exclude){p}")).collect())
            .unwrap_or_default();
        let mut args = vec!["add", "-A"];
        if !inner.is_empty() {
            args.extend(["--", "."]);
            args.extend(inner.iter().map(String::as_str));
        }
        self.write(repo, &args).map(drop)
    }

    pub fn unstage(&self, repo: &Path, paths: &[&str]) -> Result<()> {
        let mut args = vec!["restore", "--staged", "--"];
        args.extend_from_slice(paths);
        match self.write(repo, &args) {
            Ok(_) => Ok(()),
            // No commit yet: there is nothing to restore from.
            Err(_) if !self.has_head(repo) => {
                let mut args = vec!["rm", "--cached", "-r", "-q", "--"];
                args.extend_from_slice(paths);
                self.write(repo, &args).map(drop)
            }
            Err(e) => Err(e),
        }
    }

    pub fn has_head(&self, repo: &Path) -> bool {
        self.read(repo, &["rev-parse", "--verify", "-q", "HEAD"])
            .is_ok_and(|o| o.success())
    }

    /// Apply a patch to the index (`cached`) or the working tree, forwards
    /// or in `reverse`.
    pub fn apply(&self, repo: &Path, patch: &str, cached: bool, reverse: bool) -> Result<()> {
        let mut cmd = self
            .write_cmd(repo)
            .args(["apply", "--whitespace=nowarn", "--recount"]);
        if cached {
            cmd = cmd.arg("--cached");
        }
        if reverse {
            cmd = cmd.arg("-R");
        }
        process::run(&cmd.arg("-").stdin(patch.as_bytes().to_vec()))?
            .check()
            .map(drop)
    }

    /// The content of a blob as stored: `":path"` for the staged version,
    /// `"HEAD:path"` for the last commit. `None` when it does not exist
    /// there (a new file, or no commit yet).
    pub fn blob(&self, repo: &Path, spec: &str) -> Result<Option<Vec<u8>>> {
        let out = self.read(repo, &["cat-file", "-p", spec])?;
        Ok(out.success().then_some(out.stdout))
    }

    /// Put `content` in the index as the staged version of `path` (stage or
    /// unstage part of a file). Keeps the file's mode; a file not in the
    /// index yet gets 100755 when executable on disk, else 100644.
    pub fn set_index_content(&self, repo: &Path, path: &str, content: &[u8]) -> Result<()> {
        let listed = self.read_ok(repo, &["ls-files", "-s", "--", path])?;
        let mode = match listed.split_whitespace().next() {
            Some(mode) => mode.to_owned(),
            None => {
                if is_executable(&repo.join(path)) {
                    "100755".to_owned()
                } else {
                    "100644".to_owned()
                }
            }
        };
        let hashed = process::run(
            &self
                .cmd(repo)
                .args(["hash-object", "-w", "--stdin"])
                .stdin(content.to_vec()),
        )?
        .check()?
        .stdout_str();
        let sha = hashed.trim();
        if sha.is_empty() {
            anyhow::bail!("git hash-object printed no hash");
        }
        let info = format!("{mode},{sha},{path}");
        self.write(repo, &["update-index", "--add", "--cacheinfo", &info])
            .map(drop)
    }

    /// Throw away unstaged changes to tracked files.
    pub fn restore_worktree(&self, repo: &Path, paths: &[&str]) -> Result<()> {
        let mut args = vec!["restore", "--"];
        args.extend_from_slice(paths);
        self.write(repo, &args).map(drop)
    }

    // -----------------------------------------------------------------
    // Commit (5.6)
    // -----------------------------------------------------------------

    /// Commit the index with `message`. Hooks run as usual; when one fails
    /// its output is the error.
    pub fn commit(&self, repo: &Path, message: &str, amend: bool) -> Result<String> {
        let mut cmd = self
            .write_cmd(repo)
            .args(["commit", "-F", "-"])
            .stdin(message.as_bytes().to_vec())
            // Hooks (tests, linters) can be slow.
            .timeout(Duration::from_secs(600));
        if amend {
            cmd = cmd.arg("--amend");
        }
        Ok(process::run(&cmd)?.check()?.stdout_str())
    }

    pub fn last_commit_message(&self, repo: &Path) -> Result<String> {
        Ok(self
            .read_ok(repo, &["log", "-1", "--format=%B"])?
            .trim_end()
            .to_owned())
    }

    // -----------------------------------------------------------------
    // Branches and remotes (5.7)
    // -----------------------------------------------------------------

    pub fn branches(&self, repo: &Path) -> Result<Vec<Branch>> {
        let out = self.read_ok(
            repo,
            &[
                "for-each-ref",
                "--format=%(refname)%00%(refname:short)%00%(HEAD)%00%(upstream:short)%00%(objectname:short)%00%(committerdate:unix)%00%(subject)",
                "refs/heads",
                "refs/remotes",
            ],
        )?;
        Ok(parse_branches(&out))
    }

    /// The checked-out branch, `None` when detached.
    pub fn current_branch(&self, repo: &Path) -> Option<String> {
        self.read_lines(repo, &["symbolic-ref", "--short", "-q", "HEAD"])
            .ok()
            .and_then(|l| l.into_iter().next())
    }

    /// A remote's URL as configured (before any `url.*.insteadOf`
    /// rewriting, so a github.com remote still reads as one).
    pub fn remote_url(&self, repo: &Path, remote: &str) -> Result<String> {
        let key = format!("remote.{remote}.url");
        let url = self.read_ok(repo, &["config", "--get", &key])?.trim().to_owned();
        if url.is_empty() {
            anyhow::bail!("remote {remote} has no URL");
        }
        Ok(url)
    }

    /// The remote whose URL is github.com/`owner`/`name`.
    pub fn github_remote_name(&self, repo: &Path, owner: &str, name: &str) -> Option<String> {
        self.remotes(repo).ok()?.into_iter().find(|r| {
            self.remote_url(repo, r)
                .ok()
                .and_then(|u| crate::github_api::parse_remote(&u))
                .is_some_and(|(o, n)| o.eq_ignore_ascii_case(owner) && n.eq_ignore_ascii_case(name))
        })
    }

    /// `git fetch <remote> <refspec>` (network, with the token).
    pub fn fetch_refspec(&self, repo: &Path, remote: &str, refspec: &str) -> Result<()> {
        let cmd = self
            .write_cmd(repo)
            .args(["fetch", remote, refspec])
            .timeout(NETWORK_TIMEOUT);
        process::run(&cmd)?.check().map(drop)
    }

    /// Switch to `branch`; with `track`, create it from `<track>/<branch>`
    /// when it does not exist locally.
    pub fn switch_branch(&self, repo: &Path, branch: &str, track: Option<&str>) -> Result<()> {
        let exists = self
            .read(repo, &["rev-parse", "--verify", "-q", &format!("refs/heads/{branch}")])
            .is_ok_and(|o| o.success());
        match (exists, track) {
            (false, Some(remote)) => self
                .write(repo, &["switch", "-c", branch, "--track", &format!("{remote}/{branch}")])
                .map(drop),
            _ => self.write(repo, &["switch", branch]).map(drop),
        }
    }

    pub fn init(&self, dir: &Path) -> Result<()> {
        self.write(dir, &["init", "-q"]).map(drop)
    }

    pub fn add_remote(&self, repo: &Path, name: &str, url: &str) -> Result<()> {
        self.write(repo, &["remote", "add", name, url]).map(drop)
    }

    /// Stage everything and make the first commit.
    pub fn initial_commit(&self, repo: &Path, message: &str) -> Result<()> {
        self.stage_all(repo)?;
        self.commit(repo, message, false).map(drop)
    }

    /// `git clone <url> <dest>`: cancellable, with progress lines.
    pub fn clone_repo(
        &self,
        url: &str,
        dest: &Path,
        cancel: &CancelToken,
        progress: Option<process::Progress>,
    ) -> Result<()> {
        let parent = dest.parent().ok_or_else(|| anyhow::anyhow!("no parent folder"))?;
        let cmd = self
            .cmd(parent)
            .args(["clone", "--progress", url])
            .arg(dest.to_string_lossy().into_owned())
            .timeout(NETWORK_TIMEOUT);
        let out = process::run_with(&cmd, Some(cancel), progress)?;
        if out.success() {
            Ok(())
        } else {
            bail!("{}", out.message())
        }
    }

    pub fn switch(&self, repo: &Path, branch: &Branch) -> Result<()> {
        if branch.remote {
            // "origin/feature" → a local "feature" tracking it (or the
            // existing local one).
            let local = branch
                .name
                .split_once('/')
                .map(|(_, rest)| rest)
                .unwrap_or(&branch.name);
            self.write(repo, &["switch", local]).map(drop)
        } else {
            self.write(repo, &["switch", &branch.name]).map(drop)
        }
    }

    pub fn create_branch(&self, repo: &Path, name: &str, start: Option<&str>) -> Result<()> {
        let mut args = vec!["switch", "-c", name];
        if let Some(start) = start {
            args.push(start);
        }
        self.write(repo, &args).map(drop)
    }

    pub fn rename_branch(&self, repo: &Path, from: &str, to: &str) -> Result<()> {
        self.write(repo, &["branch", "-m", from, to]).map(drop)
    }

    /// Delete a local branch. Without `force`, git refuses when the branch
    /// has unmerged commits; that error is [`DeleteError::Unmerged`].
    pub fn delete_branch(&self, repo: &Path, name: &str, force: bool) -> Result<()> {
        let flag = if force { "-D" } else { "-d" };
        match self.write(repo, &["branch", flag, name]) {
            Ok(_) => Ok(()),
            Err(e) if !force && e.to_string().contains("not fully merged") => {
                Err(DeleteError::Unmerged(e.to_string()).into())
            }
            Err(e) => Err(e),
        }
    }

    pub fn check_branch_name(&self, repo: &Path, name: &str) -> bool {
        self.read(repo, &["check-ref-format", "--branch", name])
            .is_ok_and(|o| o.success())
    }

    /// A read-only command's non-empty output lines (fails on a non-zero
    /// exit).
    pub fn read_lines(&self, repo: &Path, args: &[&str]) -> Result<Vec<String>> {
        Ok(self
            .read_ok(repo, args)?
            .lines()
            .map(str::to_owned)
            .filter(|l| !l.is_empty())
            .collect())
    }

    /// The patch between two commits (`from` → `to`).
    pub fn range_diff(&self, repo: &Path, from: &str, to: &str) -> Result<String> {
        self.read_ok(
            repo,
            &["diff", "--no-ext-diff", "--no-color", "-M", from, to],
        )
    }

    pub fn remotes(&self, repo: &Path) -> Result<Vec<String>> {
        Ok(self
            .read_ok(repo, &["remote"])?
            .lines()
            .map(str::to_owned)
            .filter(|l| !l.is_empty())
            .collect())
    }

    /// Run a network command (fetch, pull, push): cancellable, with stderr
    /// progress lines.
    pub fn network(
        &self,
        repo: &Path,
        op: &NetworkOp,
        cancel: &CancelToken,
        progress: Option<process::Progress>,
    ) -> Result<String> {
        let args = network_args(op);
        let cmd = self
            .write_cmd(repo)
            .args(args)
            .arg("--progress")
            .timeout(NETWORK_TIMEOUT);
        let out = process::run_with(&cmd, Some(cancel), progress)?;
        if out.success() {
            Ok(out.message())
        } else {
            let message = out.message();
            if is_auth_failure(&message) {
                bail!(
                    "{message}\n\nGit could not sign in. Set up a credential helper \
                     (for example `gh auth setup-git`), or run the command in the \
                     Terminal tab."
                );
            }
            bail!("{message}")
        }
    }

    // -----------------------------------------------------------------
    // History and stash (5.8–5.9)
    // -----------------------------------------------------------------

    pub fn log(&self, repo: &Path, limit: usize) -> Result<Vec<Commit>> {
        if !self.has_head(repo) {
            return Ok(Vec::new());
        }
        let n = limit.to_string();
        let out = self.read_ok(
            repo,
            &[
                "log",
                "-n",
                &n,
                "--format=%H%x00%h%x00%an%x00%at%x00%s%x1e",
            ],
        )?;
        Ok(parse_log(&out))
    }

    pub fn stashes(&self, repo: &Path) -> Result<Vec<Stash>> {
        let out = self.read_ok(repo, &["stash", "list", "--format=%gd%x00%gs%x00%ct"])?;
        Ok(parse_stashes(&out))
    }

    pub fn stash_push(&self, repo: &Path, message: Option<&str>) -> Result<()> {
        let mut args = vec!["stash", "push", "--include-untracked"];
        if let Some(m) = message.filter(|m| !m.trim().is_empty()) {
            args.extend(["-m", m]);
        }
        self.write(repo, &args).map(drop)
    }

    pub fn stash_op(&self, repo: &Path, op: StashOp, name: &str) -> Result<()> {
        let verb = match op {
            StashOp::Apply => "apply",
            StashOp::Pop => "pop",
            StashOp::Drop => "drop",
        };
        self.write(repo, &["stash", verb, name]).map(drop)
    }

    /// The commits on the current branch that are not on `base`, oldest
    /// first: (subject, body). For pre-filling a pull request.
    pub fn commits_since(&self, repo: &Path, base: &str) -> Result<Vec<(String, String)>> {
        let range = format!("{base}..HEAD");
        let out = self.read_ok(
            repo,
            &["log", "--reverse", "--format=%s%x00%b%x1e", &range],
        )?;
        Ok(out
            .split('\x1e')
            .filter_map(|r| {
                let r = r.trim_start_matches('\n');
                let (s, b) = r.split_once('\0')?;
                Some((s.to_owned(), b.trim().to_owned()))
            })
            .collect())
    }
}

/// Which diff of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffKind {
    Unstaged,
    Staged,
    Untracked,
}

/// How `git pull` integrates (a setting; fast-forward only by default).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PullMode {
    #[default]
    FastForwardOnly,
    Merge,
    Rebase,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NetworkOp {
    Fetch,
    Pull { mode: PullMode },
    /// `set_upstream`: the remote to publish the branch to.
    Push { set_upstream: Option<String> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StashOp {
    Apply,
    Pop,
    Drop,
}

/// A branch could not be deleted.
#[derive(Debug, Clone)]
pub enum DeleteError {
    Unmerged(String),
}

impl std::fmt::Display for DeleteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unmerged(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for DeleteError {}

/// Whether git's message says it could not authenticate.
pub fn is_auth_failure(message: &str) -> bool {
    let m = message.to_lowercase();
    [
        "authentication failed",
        "could not read username",
        "could not read password",
        "terminal prompts disabled",
        "permission denied (publickey",
        "invalid username or password",
    ]
    .iter()
    .any(|s| m.contains(s))
}

// ---------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
}

pub fn parse_worktrees(text: &str) -> Vec<Worktree> {
    let mut out = Vec::new();
    for block in text.split("\n\n") {
        let mut path = None;
        let mut branch = None;
        for line in block.lines() {
            if let Some(p) = line.strip_prefix("worktree ") {
                path = Some(PathBuf::from(p));
            } else if let Some(b) = line.strip_prefix("branch ") {
                branch = Some(b.trim_start_matches("refs/heads/").to_owned());
            }
        }
        if let Some(path) = path {
            out.push(Worktree { path, branch });
        }
    }
    out
}

/// The branch line of `git status --porcelain=v2 --branch`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchStatus {
    /// `None` when detached.
    pub head: Option<String>,
    pub oid: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
}

/// Which section of the Git panel an entry is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Section {
    Conflicts,
    Staged,
    Changes,
    Untracked,
}

/// What happened to a file, for its letter in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Conflicted,
}

impl Change {
    fn from_code(c: char) -> Option<Self> {
        Some(match c {
            'A' => Self::Added,
            'M' => Self::Modified,
            'D' => Self::Deleted,
            'R' => Self::Renamed,
            'C' => Self::Copied,
            'T' => Self::TypeChanged,
            _ => return None,
        })
    }

    pub fn letter(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::TypeChanged => "T",
            Self::Untracked => "U",
            Self::Conflicted => "!",
        }
    }
}

/// One file in one section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusEntry {
    pub path: String,
    pub orig_path: Option<String>,
    pub section: Section,
    pub change: Change,
}

impl StatusEntry {
    /// An untracked folder with its own `.git` (with every untracked file
    /// listed, only such a folder ends in `/`).
    pub fn is_inner_repo(&self) -> bool {
        self.section == Section::Untracked && self.path.ends_with('/')
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: BranchStatus,
    pub entries: Vec<StatusEntry>,
}

impl Status {
    pub fn section(&self, section: Section) -> impl Iterator<Item = &StatusEntry> {
        self.entries.iter().filter(move |e| e.section == section)
    }

    /// Untracked folders that are repositories of their own (git lists
    /// such a folder as one entry ending in `/`, never its files), without
    /// the `/`.
    pub fn inner_repos(&self) -> impl Iterator<Item = &str> {
        self.section(Section::Untracked)
            .filter(|e| e.is_inner_repo())
            .map(|e| e.path.trim_end_matches('/'))
    }

    pub fn has_staged(&self) -> bool {
        self.section(Section::Staged).next().is_some()
    }

    pub fn is_clean(&self) -> bool {
        self.entries.is_empty()
    }

    /// The change of `path` as the file tree colours it: conflicts win,
    /// then unstaged/untracked, then staged.
    pub fn file_change(&self, path: &str) -> Option<Change> {
        let mut best: Option<(Section, Change)> = None;
        for e in self.entries.iter().filter(|e| e.path == path) {
            if best.is_none_or(|(s, _)| e.section < s) {
                best = Some((e.section, e.change));
            }
        }
        best.map(|(_, c)| c)
    }
}

/// Parse `git status --porcelain=v2 -z --branch`.
/// The `! path` records of `git status --porcelain=v2 -z --ignored`: the
/// first `max` paths, and how many there are.
pub fn parse_ignored(bytes: &[u8], max: usize) -> (Vec<String>, usize) {
    let text = String::from_utf8_lossy(bytes);
    let all: Vec<&str> = text.split('\0').filter_map(|r| r.strip_prefix("! ")).collect();
    let total = all.len();
    (all.into_iter().take(max).map(str::to_owned).collect(), total)
}

pub fn parse_status(bytes: &[u8]) -> Status {
    let text = String::from_utf8_lossy(bytes);
    let mut fields = text.split('\0').peekable();
    let mut status = Status::default();
    while let Some(record) = fields.next() {
        if record.is_empty() {
            continue;
        }
        if let Some(header) = record.strip_prefix("# ") {
            let (key, value) = header.split_once(' ').unwrap_or((header, ""));
            match key {
                "branch.oid" => status.branch.oid = Some(value.to_owned()),
                "branch.head" => {
                    status.branch.head = (value != "(detached)").then(|| value.to_owned())
                }
                "branch.upstream" => status.branch.upstream = Some(value.to_owned()),
                "branch.ab" => {
                    for part in value.split(' ') {
                        if let Some(n) = part.strip_prefix('+') {
                            status.branch.ahead = n.parse().unwrap_or(0);
                        } else if let Some(n) = part.strip_prefix('-') {
                            status.branch.behind = n.parse().unwrap_or(0);
                        }
                    }
                }
                _ => {}
            }
            continue;
        }
        let kind = record.chars().next().unwrap_or(' ');
        match kind {
            '1' | '2' => {
                // 1 XY sub mH mI mW hH hI path
                // 2 XY sub mH mI mW hH hI Xscore path<NUL>origPath
                let n = if kind == '1' { 9 } else { 10 };
                let parts: Vec<&str> = record.splitn(n, ' ').collect();
                let Some(path) = parts.get(n - 1) else {
                    continue;
                };
                let orig = if kind == '2' {
                    fields.next().map(str::to_owned)
                } else {
                    None
                };
                let xy: Vec<char> = parts.get(1).map(|s| s.chars().collect()).unwrap_or_default();
                let (x, y) = (
                    xy.first().copied().unwrap_or('.'),
                    xy.get(1).copied().unwrap_or('.'),
                );
                if let Some(change) = Change::from_code(x) {
                    status.entries.push(StatusEntry {
                        path: (*path).to_owned(),
                        orig_path: orig.clone(),
                        section: Section::Staged,
                        change,
                    });
                }
                if let Some(change) = Change::from_code(y) {
                    status.entries.push(StatusEntry {
                        path: (*path).to_owned(),
                        orig_path: None,
                        section: Section::Changes,
                        change,
                    });
                }
            }
            'u' => {
                // u XY sub m1 m2 m3 mW h1 h2 h3 path
                let parts: Vec<&str> = record.splitn(11, ' ').collect();
                if let Some(path) = parts.get(10) {
                    status.entries.push(StatusEntry {
                        path: (*path).to_owned(),
                        orig_path: None,
                        section: Section::Conflicts,
                        change: Change::Conflicted,
                    });
                }
            }
            '?' => {
                if let Some(path) = record.get(2..) {
                    status.entries.push(StatusEntry {
                        path: path.to_owned(),
                        orig_path: None,
                        section: Section::Untracked,
                        change: Change::Untracked,
                    });
                }
            }
            _ => {}
        }
    }
    status
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    /// Short name: `main`, `origin/main`.
    pub name: String,
    pub remote: bool,
    pub current: bool,
    pub upstream: Option<String>,
    pub sha: String,
    pub time: i64,
    pub subject: String,
}

pub fn parse_branches(text: &str) -> Vec<Branch> {
    text.lines()
        .filter_map(|line| {
            let f: Vec<&str> = line.split('\0').collect();
            if f.len() < 7 {
                return None;
            }
            let remote = f[0].starts_with("refs/remotes/");
            // Skip "origin/HEAD" symbolic refs.
            if remote && f[0].ends_with("/HEAD") {
                return None;
            }
            Some(Branch {
                name: f[1].to_owned(),
                remote,
                current: f[2] == "*",
                upstream: (!f[3].is_empty()).then(|| f[3].to_owned()),
                sha: f[4].to_owned(),
                time: f[5].parse().unwrap_or(0),
                subject: f[6].to_owned(),
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub sha: String,
    pub short: String,
    pub author: String,
    pub time: i64,
    pub subject: String,
}

pub fn parse_log(text: &str) -> Vec<Commit> {
    text.split('\x1e')
        .filter_map(|r| {
            let r = r.trim_start_matches('\n');
            let f: Vec<&str> = r.split('\0').collect();
            (f.len() >= 5).then(|| Commit {
                sha: f[0].to_owned(),
                short: f[1].to_owned(),
                author: f[2].to_owned(),
                time: f[3].parse().unwrap_or(0),
                subject: f[4].to_owned(),
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stash {
    /// `stash@{0}`.
    pub name: String,
    pub message: String,
    pub time: i64,
}

pub fn parse_stashes(text: &str) -> Vec<Stash> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\0').collect();
            (f.len() >= 3).then(|| Stash {
                name: f[0].to_owned(),
                message: f[1].to_owned(),
                time: f[2].parse().unwrap_or(0),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------
// Unified diffs and hunk patches
// ---------------------------------------------------------------------

/// One hunk of a file's diff, with its raw lines (each keeps its `+`, `-`,
/// ` ` or `\` prefix and any `\r`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub header: String,
    pub old_start: u32,
    pub old_len: u32,
    pub new_start: u32,
    pub new_len: u32,
    pub lines: Vec<String>,
}

impl Hunk {
    /// The hunk's last shown line, as (new side?, 1-based line): the hunk's
    /// buttons go just below it, so they never split a change.
    pub fn anchor(&self) -> (bool, u32) {
        let mut old = self.old_start;
        let mut new = self.new_start;
        let mut last = (true, self.new_start);
        for l in &self.lines {
            match l.as_bytes().first() {
                Some(b'+') => {
                    last = (true, new);
                    new += 1;
                }
                Some(b'-') => {
                    last = (false, old);
                    old += 1;
                }
                Some(b' ') => {
                    last = (true, new);
                    old += 1;
                    new += 1;
                }
                _ => {}
            }
        }
        last
    }
}

/// One file of a unified diff.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilePatch {
    /// `diff --git …`, `index …`, `---`, `+++` and similar lines.
    pub header: Vec<String>,
    pub old_path: Option<String>,
    pub new_path: Option<String>,
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

impl FilePatch {
    /// A patch with only hunk `index`, for `git apply`.
    pub fn hunk_patch(&self, index: usize) -> Option<String> {
        let hunk = self.hunks.get(index)?;
        let mut out = String::new();
        for h in &self.header {
            out.push_str(h);
            out.push('\n');
        }
        out.push_str(&hunk.header);
        out.push('\n');
        for l in &hunk.lines {
            out.push_str(l);
            out.push('\n');
        }
        Some(out)
    }

    pub fn path(&self) -> &str {
        self.new_path
            .as_deref()
            .or(self.old_path.as_deref())
            .unwrap_or("")
    }

    pub fn line_count(&self) -> usize {
        self.hunks.iter().map(|h| h.lines.len()).sum()
    }
}

fn parse_range(s: &str) -> (u32, u32) {
    let s = &s[1..];
    match s.split_once(',') {
        Some((a, b)) => (a.parse().unwrap_or(0), b.parse().unwrap_or(0)),
        None => (s.parse().unwrap_or(0), 1),
    }
}

fn header_path(line: &str, prefix: &str) -> Option<String> {
    let p = line.strip_prefix(prefix)?.trim_end_matches('\r');
    let p = p.split('\t').next().unwrap_or(p);
    if p == "/dev/null" {
        return None;
    }
    Some(
        p.strip_prefix("a/")
            .or_else(|| p.strip_prefix("b/"))
            .unwrap_or(p)
            .to_owned(),
    )
}

/// Split a unified diff into files and hunks. Lines are split on `\n`
/// only, so `\r` stays part of a CRLF line.
pub fn parse_patch(text: &str) -> Vec<FilePatch> {
    let mut files: Vec<FilePatch> = Vec::new();
    let mut in_hunk = false;
    let mut lines: Vec<&str> = text.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    for line in lines {
        if line.starts_with("diff --git ") || (files.is_empty() && line.starts_with("--- ")) {
            files.push(FilePatch::default());
            in_hunk = false;
            if line.starts_with("--- ") {
                // A bare patch without a `diff --git` line.
                if let Some(f) = files.last_mut() {
                    f.old_path = header_path(line, "--- ");
                    f.header.push(line.to_owned());
                }
                continue;
            }
        }
        let Some(file) = files.last_mut() else {
            continue;
        };
        if let Some(rest) = line.strip_prefix("@@ ") {
            let mut parts = rest.split(' ');
            let old = parts.next().map(parse_range).unwrap_or((0, 0));
            let new = parts.next().map(parse_range).unwrap_or((0, 0));
            file.hunks.push(Hunk {
                header: line.to_owned(),
                old_start: old.0,
                old_len: old.1,
                new_start: new.0,
                new_len: new.1,
                lines: Vec::new(),
            });
            in_hunk = true;
            continue;
        }
        if in_hunk {
            if matches!(line.as_bytes().first(), Some(b'+' | b'-' | b' ' | b'\\')) {
                if let Some(h) = file.hunks.last_mut() {
                    h.lines.push(line.to_owned());
                }
                continue;
            }
            if line.is_empty() {
                // An empty context line some tools emit without the space.
                if let Some(h) = file.hunks.last_mut() {
                    h.lines.push(" ".to_owned());
                }
                continue;
            }
            in_hunk = false;
        }
        if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            file.binary = true;
        }
        if line.starts_with("--- ") {
            file.old_path = header_path(line, "--- ");
        } else if line.starts_with("+++ ") {
            file.new_path = header_path(line, "+++ ");
        } else if let Some(rest) = line.strip_prefix("diff --git ") {
            // Fallback paths for binary or mode-only changes.
            if let Some((a, b)) = rest.split_once(" b/") {
                file.old_path = Some(a.trim_start_matches("a/").to_owned());
                file.new_path = Some(b.to_owned());
            }
        } else if line.starts_with("new file mode") {
            file.old_path = None;
        } else if line.starts_with("deleted file mode") {
            file.new_path = None;
        }
        file.header.push(line.to_owned());
    }
    files
}

/// Copy `path` into the Trash (the original stays), so a discard can be
/// undone from Finder.
pub fn copy_to_trash(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let dir = tempfile::Builder::new()
        .prefix("anveg-discard-")
        .tempdir()
        .context("could not make a temporary folder")?;
    let name = path.file_name().context("no file name")?;
    let copy = dir.path().join(name);
    if path.is_dir() {
        bail!("{} is a folder", path.display());
    }
    std::fs::copy(path, &copy).with_context(|| format!("could not copy {}", path.display()))?;
    trash::delete(&copy).context("could not move the copy to the Trash")?;
    Ok(())
}

/// The arguments of a network command (`git` excluded; `--progress` is
/// added when it runs).
pub fn network_args(op: &NetworkOp) -> Vec<String> {
    match op {
        NetworkOp::Fetch => vec!["fetch".into(), "--all".into(), "--prune".into()],
        NetworkOp::Pull { mode } => {
            let mut a = vec!["pull".to_owned()];
            a.push(
                match mode {
                    PullMode::FastForwardOnly => "--ff-only",
                    PullMode::Merge => "--no-rebase",
                    PullMode::Rebase => "--rebase",
                }
                .into(),
            );
            a
        }
        NetworkOp::Push { set_upstream } => match set_upstream {
            Some(remote) => vec![
                "push".into(),
                "--set-upstream".into(),
                remote.clone(),
                "HEAD".into(),
            ],
            None => vec!["push".into()],
        },
    }
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(path).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(_: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, script: &str) -> Result<()> {
        let out = process::run(
            &Cmd::new("sh")
                .args(["-c", script])
                .cwd(dir)
                .var("GIT_AUTHOR_NAME", "T")
                .var("GIT_AUTHOR_EMAIL", "t@example.com")
                .var("GIT_COMMITTER_NAME", "T")
                .var("GIT_COMMITTER_EMAIL", "t@example.com"),
        )?;
        out.check().map(drop)
    }

    fn repo() -> Result<(tempfile::TempDir, PathBuf)> {
        let tmp = tempfile::tempdir()?;
        let dir = tmp.path().canonicalize()?;
        sh(
            &dir,
            "git init -q -b main && git config user.name T && git config user.email t@example.com \
             && git config commit.gpgsign false",
        )?;
        Ok((tmp, dir))
    }

    #[test]
    fn blobs_and_partial_staging() -> Result<()> {
        let (_t, dir) = repo()?;
        let git = Git::default();
        sh(
            &dir,
            "printf 'a\\nb\\nc\\n' > f.txt && git add f.txt && git commit -qm init \
             && printf 'A\\nb\\nC\\n' > f.txt",
        )?;
        assert_eq!(git.blob(&dir, "HEAD:f.txt")?.as_deref(), Some(&b"a\nb\nc\n"[..]));
        assert_eq!(git.blob(&dir, ":nope.txt")?, None);
        // Stage only the first change.
        git.set_index_content(&dir, "f.txt", b"A\nb\nc\n")?;
        assert_eq!(git.blob(&dir, ":f.txt")?.as_deref(), Some(&b"A\nb\nc\n"[..]));
        let staged = git.file_diff(&dir, "f.txt", DiffKind::Staged)?;
        assert!(staged.contains("-a\n+A\n") && !staged.contains("+C"));
        let unstaged = git.file_diff(&dir, "f.txt", DiffKind::Unstaged)?;
        assert!(unstaged.contains("-c\n+C\n") && !unstaged.contains("+A"));
        // The working file is untouched.
        assert_eq!(std::fs::read(dir.join("f.txt"))?, b"A\nb\nC\n");
        // A new file staged in part keeps a regular mode.
        std::fs::write(dir.join("n.txt"), "x\ny\n")?;
        git.set_index_content(&dir, "n.txt", b"x\n")?;
        let listed = git.read_ok(&dir, &["ls-files", "-s", "--", "n.txt"])?;
        assert!(listed.starts_with("100644 "), "{listed}");
        Ok(())
    }

    #[test]
    fn adding_new_and_ignored_files() -> Result<()> {
        let (_t, dir) = repo()?;
        let git = Git::default();
        sh(
            &dir,
            "printf 'build/\\n*.log\\n' > .gitignore && git add .gitignore && git commit -qm init \
             && mkdir -p build src && echo x > build/out.bin && echo y > debug.log \
             && echo z > src/new.rs && echo w > src/other.rs",
        )?;
        // New files are untracked; ignored ones aren't listed there.
        let status = git.status(&dir)?;
        let untracked: Vec<&str> = status.section(Section::Untracked).map(|e| e.path.as_str()).collect();
        assert_eq!(untracked, ["src/new.rs", "src/other.rs"]);
        // The ignored ones, folders collapsed.
        let (ignored, total) = git.ignored(&dir, 10)?;
        assert_eq!((ignored.clone(), total), (vec!["build/".to_owned(), "debug.log".to_owned()], 2));
        assert_eq!(git.ignored(&dir, 1)?, (vec!["build/".to_owned()], 2));
        // Which rule ignores a path.
        assert_eq!(git.ignore_rule(&dir, "debug.log")?.as_deref(), Some("“*.log” (line 2 of .gitignore)"));
        assert_eq!(git.ignore_rule(&dir, "src/new.rs")?, None);
        // Adding a folder stages its new files; again, nothing is left.
        assert_eq!(git.add_counted(&dir, &["src"])?, 2);
        assert_eq!(git.add_counted(&dir, &["src"])?, 0);
        // An ignored file is added only by force.
        // (git add refuses an ignored path outright.)
        assert!(git.add_counted(&dir, &["build"]).is_err());
        git.stage_force(&dir, &["debug.log"])?;
        let staged: Vec<String> = git.status(&dir)?.section(Section::Staged).map(|e| e.path.clone()).collect();
        assert_eq!(staged, ["debug.log", "src/new.rs", "src/other.rs"]);
        Ok(())
    }

    #[test]
    fn inner_repositories() -> Result<()> {
        let (_t, dir) = repo()?;
        let git = Git::default();
        sh(
            &dir,
            "echo a > a.txt && git add a.txt && git commit -qm init \
             && mkdir -p Plain Inner Done && echo x > Plain/f.swift \
             && (cd Inner && git init -q && echo y > f.swift) \
             && (cd Done && git init -q && echo z > g.swift && git add g.swift && git commit -qm d)",
        )?;
        let status = git.status(&dir)?;
        assert_eq!(status.inner_repos().collect::<Vec<_>>(), ["Done", "Inner"]);
        let found = git.find_repos_with_inner(std::slice::from_ref(&dir));
        assert_eq!(found, [dir.clone(), dir.join("Done"), dir.join("Inner")]);
        // Stage All leaves inner repositories out (no error, no link).
        git.stage_all(&dir)?;
        let staged: Vec<String> = git.status(&dir)?.section(Section::Staged).map(|e| e.path.clone()).collect();
        assert_eq!(staged, ["Plain/f.swift"]);
        // A link recorded anyway (plain git add) is still found...
        sh(&dir, "git add Done 2>/dev/null")?;
        assert_eq!(git.gitlinks(&dir), ["Done"]);
        assert!(git.inner_repos(&dir).contains(&dir.join("Done")));
        // ...and absorbing turns both into ordinary folders.
        let rm = |p: &Path| std::fs::remove_dir_all(p).map_err(anyhow::Error::from);
        git.absorb_inner_repo(&dir, "Done/", rm)?;
        git.absorb_inner_repo(&dir, "Inner", rm)?;
        assert!(git.gitlinks(&dir).is_empty() && git.inner_repos(&dir).is_empty());
        let untracked: Vec<String> = git.status(&dir)?.section(Section::Untracked).map(|e| e.path.clone()).collect();
        assert_eq!(untracked, ["Done/g.swift", "Inner/f.swift"]);
        assert!(git.absorb_inner_repo(&dir, "Plain", rm).is_err());
        Ok(())
    }

    #[test]
    fn clones_and_cancels() -> Result<()> {
        let (_t, src) = repo()?;
        sh(&src, "echo x > a && git add a && git commit -qm one")?;
        let tmp = tempfile::tempdir()?;
        let dest = tmp.path().join("copy");
        let git = Git::default();
        let lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let l2 = lines.clone();
        let progress: process::Progress = std::sync::Arc::new(move |line: &str| {
            if let Ok(mut v) = l2.lock() {
                v.push(line.to_owned());
            }
        });
        git.clone_repo(&src.to_string_lossy(), &dest, &CancelToken::new(), Some(progress))?;
        assert!(dest.join("a").is_file());
        assert_eq!(git.current_branch(&dest).as_deref(), Some("main"));
        // A cancelled clone stops with a cancellation error.
        let cancel = CancelToken::new();
        cancel.cancel();
        let e = git.clone_repo(&src.to_string_lossy(), &tmp.path().join("c2"), &cancel, None);
        assert!(e.is_err_and(|e| process::is_cancelled(&e)));
        // The askpass settings reach git's environment, not its arguments.
        let with = git.clone().with_askpass(Some(Askpass {
            script: PathBuf::from("/x/askpass"),
            token: crate::github_api::Secret::new("sekret"),
        }));
        let cmd = with.cmd(&src);
        assert!(!cmd.display().contains("sekret"));
        assert!(cmd.extra_env.iter().any(|(k, v)| k == "ANVEG_GIT_TOKEN" && v == "sekret"));
        assert!(format!("{with:?}").contains("Secret(…)") && !format!("{with:?}").contains("sekret"));
        Ok(())
    }

    #[test]
    fn status_sections() -> Result<()> {
        let (_t, dir) = repo()?;
        let git = Git::default();
        sh(
            &dir,
            "printf 'a\\n' > a.txt && printf 'b\\n' > b.txt && git add . && git commit -qm init \
             && printf 'a2\\n' > a.txt && git add a.txt && printf 'a3\\n' > a.txt \
             && rm b.txt && printf 'n\\n' > 'new file.txt' && git mv a.txt c.txt 2>/dev/null; true",
        )?;
        let s = git.status(&dir)?;
        assert_eq!(s.branch.head.as_deref(), Some("main"));
        assert!(s.has_staged());
        assert!(
            s.section(Section::Untracked)
                .any(|e| e.path == "new file.txt")
        );
        assert!(
            s.section(Section::Changes)
                .any(|e| e.path == "b.txt" && e.change == Change::Deleted)
        );
        let renamed = s.section(Section::Staged).find(|e| e.change == Change::Renamed);
        if let Some(r) = renamed {
            assert_eq!(r.path, "c.txt");
            assert_eq!(r.orig_path.as_deref(), Some("a.txt"));
        }
        Ok(())
    }

    #[test]
    fn parse_v2_records() {
        let raw = b"# branch.oid abc\0# branch.head feat\0# branch.upstream origin/feat\0# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 h1 h2 src/a b.rs\0\
2 R. N... 100644 100644 100644 h1 h2 R100 new.rs\0old.rs\0\
u UU N... 100644 100644 100644 100644 h1 h2 h3 conflict.rs\0\
? untracked.txt\0";
        let s = parse_status(raw);
        assert_eq!(s.branch.ahead, 2);
        assert_eq!(s.branch.behind, 1);
        assert_eq!(s.branch.upstream.as_deref(), Some("origin/feat"));
        assert_eq!(s.entries.len(), 4);
        assert_eq!(s.entries[0].path, "src/a b.rs");
        assert_eq!(s.entries[0].section, Section::Changes);
        assert_eq!(s.entries[1].orig_path.as_deref(), Some("old.rs"));
        assert_eq!(s.entries[2].section, Section::Conflicts);
        assert_eq!(s.entries[3].change, Change::Untracked);
        assert_eq!(s.file_change("conflict.rs"), Some(Change::Conflicted));
    }

    #[test]
    fn hunk_staging_with_crlf_and_no_trailing_newline() -> Result<()> {
        let (_t, dir) = repo()?;
        let git = Git::default();
        // CRLF file and a file without a final newline, 30 lines each so the
        // edits land in separate hunks.
        sh(
            &dir,
            "git config core.autocrlf false && \
             for i in $(seq 1 30); do printf 'line %s\\r\\n' $i; done > crlf.txt && \
             for i in $(seq 1 29); do printf 'n %s\\n' $i; done > nonl.txt && printf 'n 30' >> nonl.txt && \
             git add . && git commit -qm init && \
             sed -i 's/^line 2\\r$/line TWO\\r/; s/^line 28\\r$/line 28b\\r/' crlf.txt && \
             sed -i 's/^n 3$/n three/' nonl.txt && printf '\\nn 31' >> nonl.txt",
        )?;
        for path in ["crlf.txt", "nonl.txt"] {
            let diff = git.file_diff(&dir, path, DiffKind::Unstaged)?;
            let files = parse_patch(&diff);
            assert_eq!(files.len(), 1, "{path}");
            assert_eq!(files[0].hunks.len(), 2, "{path}: {diff}");
            // Stage only the second hunk.
            let patch = files[0].hunk_patch(1).unwrap_or_default();
            git.apply(&dir, &patch, true, false)?;
            let staged = parse_patch(&git.file_diff(&dir, path, DiffKind::Staged)?);
            assert_eq!(staged[0].hunks.len(), 1, "{path}");
            let unstaged = parse_patch(&git.file_diff(&dir, path, DiffKind::Unstaged)?);
            assert_eq!(unstaged[0].hunks.len(), 1, "{path}");
            // Unstage it again.
            let patch = staged[0].hunk_patch(0).unwrap_or_default();
            git.apply(&dir, &patch, true, true)?;
            assert!(git.file_diff(&dir, path, DiffKind::Staged)?.is_empty());
        }
        // CR bytes survive in the parsed lines.
        let diff = git.file_diff(&dir, "crlf.txt", DiffKind::Unstaged)?;
        assert!(parse_patch(&diff)[0].hunks[0].lines.iter().any(|l| l.ends_with('\r')));
        // Discarding a hunk in the working tree.
        let files = parse_patch(&diff);
        let patch = files[0].hunk_patch(0).unwrap_or_default();
        git.apply(&dir, &patch, false, true)?;
        let left = std::fs::read_to_string(dir.join("crlf.txt"))?;
        assert!(left.contains("line 2\r\n"));
        assert!(left.contains("line 28b\r\n"));
        Ok(())
    }

    #[test]
    fn untracked_diff_commit_branches_log_stash() -> Result<()> {
        let (_t, dir) = repo()?;
        let git = Git::default();
        std::fs::write(dir.join("n.txt"), "hello\n")?;
        let d = git.file_diff(&dir, "n.txt", DiffKind::Untracked)?;
        assert!(d.contains("+hello"));
        let f = parse_patch(&d);
        assert_eq!(f[0].new_path.as_deref(), Some("n.txt"));
        assert_eq!(f[0].old_path, None);

        assert!(git.log(&dir, 10)?.is_empty());
        git.stage_all(&dir)?;
        git.unstage(&dir, &["n.txt"])?; // no HEAD yet
        assert!(!git.status(&dir)?.has_staged());
        git.stage(&dir, &["n.txt"])?;
        git.commit(&dir, "first\n\nbody", false)?;
        assert_eq!(git.last_commit_message(&dir)?, "first\n\nbody");
        git.commit(&dir, "first (amended)", true)?;
        let log = git.log(&dir, 10)?;
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].subject, "first (amended)");
        assert!(git.commit_diff(&dir, &log[0].sha)?.contains("+hello"));

        git.create_branch(&dir, "feature", None)?;
        std::fs::write(dir.join("f.txt"), "f\n")?;
        git.stage_all(&dir)?;
        git.commit(&dir, "on feature", false)?;
        let branches = git.branches(&dir)?;
        assert!(branches.iter().any(|b| b.name == "feature" && b.current));
        let main = branches
            .iter()
            .find(|b| b.name == "main")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("no main"))?;
        git.switch(&dir, &main)?;
        let err = git.delete_branch(&dir, "feature", false).err();
        assert!(err.is_some_and(|e| e.downcast_ref::<DeleteError>().is_some()));
        git.rename_branch(&dir, "feature", "feature2")?;
        git.delete_branch(&dir, "feature2", true)?;
        assert!(!git.branches(&dir)?.iter().any(|b| b.name.starts_with("feature")));
        assert!(git.check_branch_name(&dir, "ok-name"));
        assert!(!git.check_branch_name(&dir, "bad..name"));

        std::fs::write(dir.join("n.txt"), "changed\n")?;
        git.stash_push(&dir, Some("wip"))?;
        let stashes = git.stashes(&dir)?;
        assert_eq!(stashes.len(), 1);
        assert!(stashes[0].message.contains("wip"));
        git.stash_op(&dir, StashOp::Pop, &stashes[0].name)?;
        assert_eq!(std::fs::read_to_string(dir.join("n.txt"))?, "changed\n");
        git.restore_worktree(&dir, &["n.txt"])?;
        assert_eq!(std::fs::read_to_string(dir.join("n.txt"))?, "hello\n");
        Ok(())
    }

    #[test]
    fn find_repos_and_worktrees() -> Result<()> {
        let (_t, dir) = repo()?;
        let git = Git::default();
        sh(&dir, "mkdir sub && touch x && git add . && git commit -qm i --allow-empty")?;
        assert_eq!(git.find_repo(&dir.join("sub")), Some(dir.clone()));
        assert_eq!(git.find_repos(&[dir.clone(), dir.join("sub")]), vec![dir.clone()]);
        let outside = tempfile::tempdir()?;
        assert_eq!(git.find_repo(outside.path()), None);
        assert!(git.git_dir(&dir)?.ends_with(".git"));
        let wt = git.worktrees(&dir)?;
        assert_eq!(wt[0].branch.as_deref(), Some("main"));
        Ok(())
    }

    #[test]
    fn auth_messages() {
        assert!(is_auth_failure(
            "fatal: could not read Username for 'https://github.com': terminal prompts disabled"
        ));
        assert!(!is_auth_failure("fatal: not a git repository"));
    }

    #[test]
    fn hunk_anchor() {
        let h = Hunk {
            header: "@@ -10,4 +10,4 @@".into(),
            old_start: 10,
            old_len: 4,
            new_start: 10,
            new_len: 4,
            lines: vec![" a".into(), " b".into(), "-c".into(), "+C".into()],
        };
        assert_eq!(h.anchor(), (true, 12));
        let deletion_at_end = Hunk {
            lines: vec![" a".into(), "-b".into()],
            ..h
        };
        assert_eq!(deletion_at_end.anchor(), (false, 11));
    }
}
