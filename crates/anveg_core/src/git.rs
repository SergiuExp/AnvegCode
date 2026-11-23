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
}

impl Default for Git {
    fn default() -> Self {
        Self {
            program: PathBuf::from("git"),
            env: None,
        }
    }
}

impl Git {
    pub fn new(program: PathBuf, env: Option<EnvMap>) -> Self {
        Self { program, env }
    }

    /// A git command in `repo`.
    pub fn cmd(&self, repo: &Path) -> Cmd {
        Cmd::new(&self.program)
            .env(self.env.as_ref())
            .cwd(repo)
            .var("GIT_TERMINAL_PROMPT", "0")
            .var("GIT_OPTIONAL_LOCKS", "0")
            .args(["-c", "core.quotepath=off", "-c", "color.ui=false"])
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

    pub fn stage_all(&self, repo: &Path) -> Result<()> {
        self.write(repo, &["add", "-A"]).map(drop)
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
        let args: Vec<String> = match op {
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
        };
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    pub branch: BranchStatus,
    pub entries: Vec<StatusEntry>,
}

impl Status {
    pub fn section(&self, section: Section) -> impl Iterator<Item = &StatusEntry> {
        self.entries.iter().filter(move |e| e.section == section)
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
    /// The first changed line on the new side (1-based), or the old side
    /// for a pure deletion: where the hunk's buttons go.
    pub fn anchor(&self) -> (bool, u32) {
        let mut old = self.old_start;
        let mut new = self.new_start;
        for l in &self.lines {
            match l.as_bytes().first() {
                Some(b'+') => return (true, new),
                Some(b'-') => return (false, old),
                Some(b' ') => {
                    old += 1;
                    new += 1;
                }
                _ => {}
            }
        }
        (true, self.new_start)
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
        assert_eq!(h.anchor(), (false, 12));
    }
}
