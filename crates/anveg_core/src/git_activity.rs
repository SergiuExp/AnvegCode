//! The Git activity log: what each push sent and what each fetch or pull
//! received, with the commits and the changed files.
//!
//! Around every network operation the app snapshots the repository's refs
//! (`refs/heads`, `refs/remotes` and `HEAD`). The difference tells what
//! moved: a push moves the remote-tracking refs it updated (sent); a fetch
//! moves remote-tracking refs (received); a pull also moves the current
//! branch (received into it). For each moved ref the log keeps its commits
//! (up to [`MAX_COMMITS`]) and changed files (up to [`MAX_FILES`]), so the
//! before → after diff can be opened later.
//!
//! Stored per project in `activity/<project-id>.json` (the newest
//! [`MAX_ENTRIES`]), in the app's data folder, never in the repository.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::git::{Git, NetworkOp};
use crate::process::{CancelToken, Progress};

pub const MAX_ENTRIES: usize = 100;
pub const MAX_COMMITS: usize = 50;
pub const MAX_FILES: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Fetch,
    Pull,
    Push,
}

impl Op {
    pub fn of(op: &NetworkOp) -> Self {
        match op {
            NetworkOp::Fetch => Self::Fetch,
            NetworkOp::Pull { .. } => Self::Pull,
            NetworkOp::Push { .. } => Self::Push,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Fetch => "Fetch",
            Self::Pull => "Pull",
            Self::Push => "Push",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitLine {
    pub sha: String,
    pub summary: String,
    pub author: String,
    /// Seconds since the epoch.
    pub time: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileLine {
    /// `A`, `M`, `D`, `R`…, as `git diff --name-status` prints it.
    pub status: String,
    pub path: String,
}

/// One ref that moved: `old` → `new` (`None`: created or deleted).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefChange {
    /// Short name, e.g. `origin/main` or `main`.
    pub name: String,
    pub old: Option<String>,
    pub new: Option<String>,
    pub commits: Vec<CommitLine>,
    /// Commits beyond the ones listed.
    pub more_commits: usize,
    pub files: Vec<FileLine>,
    pub more_files: usize,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    /// Milliseconds since the epoch.
    pub time: i64,
    pub repo: PathBuf,
    pub op: Op,
    /// The branch checked out at the time.
    pub branch: Option<String>,
    pub ok: bool,
    /// What git printed.
    pub message: String,
    /// The command that ran, e.g. `git push --set-upstream origin HEAD`.
    #[serde(default)]
    pub command: String,
    /// Push: the remote branches it updated.
    #[serde(default)]
    pub sent: Vec<RefChange>,
    /// Fetch and pull: the remote branches that moved.
    #[serde(default)]
    pub received: Vec<RefChange>,
    /// Pull: the current branch, as it moved.
    #[serde(default)]
    pub local: Option<RefChange>,
}

/// All of a project's entries, oldest first.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ActivityFile {
    pub entries: Vec<Entry>,
}

impl ActivityFile {
    pub fn load(path: &Path) -> Self {
        crate::atomic::read_json_or_default(path)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        crate::atomic::write_json(path, self)
    }

    pub fn push(&mut self, entry: Entry) {
        self.entries.push(entry);
        let extra = self.entries.len().saturating_sub(MAX_ENTRIES);
        self.entries.drain(..extra);
    }
}

/// `refname → sha` for local and remote branches, plus `HEAD`.
pub type Refs = BTreeMap<String, String>;

fn refs(git: &Git, repo: &Path) -> Refs {
    let mut out = Refs::new();
    let listed = git
        .read_lines(
            repo,
            &[
                "for-each-ref",
                "--format=%(refname) %(objectname)",
                "refs/heads",
                "refs/remotes",
            ],
        )
        .unwrap_or_default();
    for line in listed {
        if let Some((name, sha)) = line.split_once(' ')
            && !name.ends_with("/HEAD")
        {
            out.insert(name.to_owned(), sha.to_owned());
        }
    }
    if let Ok(head) = git.read_lines(repo, &["rev-parse", "-q", "--verify", "HEAD"])
        && let Some(sha) = head.into_iter().next()
    {
        out.insert("HEAD".to_owned(), sha);
    }
    out
}

fn short_name(refname: &str) -> String {
    refname
        .strip_prefix("refs/remotes/")
        .or_else(|| refname.strip_prefix("refs/heads/"))
        .unwrap_or(refname)
        .to_owned()
}

/// Parse `git log --format=%H%x1f%s%x1f%an%x1f%ct` output.
pub fn parse_commit_lines(lines: &[String]) -> Vec<CommitLine> {
    lines
        .iter()
        .filter_map(|l| {
            let mut f = l.split('\u{1f}');
            Some(CommitLine {
                sha: f.next()?.to_owned(),
                summary: f.next().unwrap_or_default().to_owned(),
                author: f.next().unwrap_or_default().to_owned(),
                time: f.next().and_then(|t| t.trim().parse().ok()).unwrap_or(0),
            })
        })
        .collect()
}

/// Parse `git diff --name-status` output.
pub fn parse_file_lines(lines: &[String]) -> Vec<FileLine> {
    lines
        .iter()
        .filter_map(|l| {
            let mut f = l.split('\t');
            let status = f.next()?.chars().next()?.to_string();
            // Renames and copies list the old and new path: keep the new.
            let path = f.next_back()?.to_owned();
            Some(FileLine { status, path })
        })
        .collect()
}

/// The commits and files between `old` and `new`. A new ref lists the
/// commits that were in none of the refs known before.
fn describe(git: &Git, repo: &Path, name: &str, old: Option<&str>, new: Option<&str>, before: &Refs) -> RefChange {
    let mut change = RefChange {
        name: short_name(name),
        old: old.map(str::to_owned),
        new: new.map(str::to_owned),
        ..Default::default()
    };
    let Some(new) = new else {
        return change;
    };
    let limit = format!("-n{}", MAX_COMMITS + 1);
    let mut args: Vec<String> = vec![
        "log".into(),
        "--format=%H%x1f%s%x1f%an%x1f%ct".into(),
        limit,
    ];
    match old {
        Some(old) => args.push(format!("{old}..{new}")),
        None => {
            args.push(new.to_owned());
            args.push("--not".into());
            args.extend(before.values().filter(|v| v.as_str() != new).cloned());
        }
    }
    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut commits = parse_commit_lines(&git.read_lines(repo, &arg_refs).unwrap_or_default());
    if commits.len() > MAX_COMMITS {
        commits.truncate(MAX_COMMITS);
        change.more_commits = 1; // "and more"; the exact count is not needed
    }
    change.commits = commits;
    if let Some(old) = old {
        let mut files = parse_file_lines(
            &git.read_lines(repo, &["diff", "--name-status", "-M", old, new])
                .unwrap_or_default(),
        );
        if files.len() > MAX_FILES {
            change.more_files = files.len() - MAX_FILES;
            files.truncate(MAX_FILES);
        }
        change.files = files;
    }
    change
}

/// What moved between two snapshots.
pub fn entry(
    git: &Git,
    repo: &Path,
    op: Op,
    branch: Option<String>,
    before: &Refs,
    after: &Refs,
    result: &Result<String>,
) -> Entry {
    let mut remote_changes = Vec::new();
    let names: std::collections::BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    for name in names {
        if !name.starts_with("refs/remotes/") {
            continue;
        }
        let (old, new) = (before.get(name), after.get(name));
        if old != new {
            remote_changes.push(describe(
                git,
                repo,
                name,
                old.map(String::as_str),
                new.map(String::as_str),
                before,
            ));
        }
    }
    let local = (op == Op::Pull)
        .then(|| {
            let (old, new) = (before.get("HEAD"), after.get("HEAD"));
            (old != new).then(|| {
                let mut c = describe(git, repo, "HEAD", old.map(String::as_str), new.map(String::as_str), before);
                c.name = branch.clone().unwrap_or_else(|| "HEAD".into());
                c
            })
        })
        .flatten();
    let (message, ok) = match result {
        Ok(m) => (m.clone(), true),
        Err(e) => (format!("{e:#}"), false),
    };
    let (sent, received) = if op == Op::Push {
        (remote_changes, Vec::new())
    } else {
        (Vec::new(), remote_changes)
    };
    Entry {
        id: uuid::Uuid::new_v4().to_string(),
        time: crate::time::now_millis(),
        repo: repo.to_path_buf(),
        op,
        branch,
        ok,
        message: message.trim().to_owned(),
        command: String::new(),
        sent,
        received,
        local,
    }
}

/// Run a network operation and describe what it sent or received.
pub fn run_logged(
    git: &Git,
    repo: &Path,
    op: &NetworkOp,
    cancel: &CancelToken,
    progress: Option<Progress>,
) -> (Result<String>, Entry) {
    let branch = git
        .read_lines(repo, &["symbolic-ref", "--short", "-q", "HEAD"])
        .ok()
        .and_then(|l| l.into_iter().next());
    let before = refs(git, repo);
    let result = git.network(repo, op, cancel, progress);
    let after = refs(git, repo);
    let mut entry = entry(git, repo, Op::of(op), branch, &before, &after, &result);
    entry.command = format!("git {}", crate::git::network_args(op).join(" "));
    (result, entry)
}

/// A failure in plain words: what happened and what to do about it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Explanation {
    pub title: String,
    pub hint: Option<String>,
}

/// Explain git's message for a failed push, pull or fetch. Known failures
/// get a sentence and a suggestion; anything else, git's own first error
/// line.
pub fn explain(message: &str, op: Op) -> Explanation {
    let m = message.to_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|n| m.contains(n));
    let e = |title: &str, hint: &str| Explanation {
        title: title.to_owned(),
        hint: Some(hint.to_owned()),
    };
    if has(&["cancelled"]) && !has(&["fatal:", "error:"]) {
        return Explanation {
            title: "Cancelled.".to_owned(),
            hint: None,
        };
    }
    if has(&["[rejected]", "non-fast-forward", "fetch first", "updates were rejected"]) {
        return e(
            "The remote has commits you don't have.",
            "Pull first (bringing those commits in), then push again.",
        );
    }
    if has(&["[remote rejected]", "pre-receive hook declined", "protected branch"]) {
        return e(
            "The remote refused the push.",
            "The branch may be protected, or a server-side check failed. See the details for the server's reason.",
        );
    }
    if has(&[
        "authentication failed",
        "could not read username",
        "could not read password",
        "permission denied (publickey)",
        "git could not sign in",
        "invalid username or password",
        "terminal prompts disabled",
    ]) {
        return e(
            "Git could not sign in to the remote.",
            "Set up a credential helper (for example `gh auth setup-git`) or an SSH key, or run the command once in the Terminal tab to sign in.",
        );
    }
    if has(&["repository not found", "does not appear to be a git repository", "not found"])
        && has(&["fatal:", "error:", "remote"])
    {
        return e(
            "The remote repository was not found.",
            "Check the remote's address (`git remote -v`) and that you have access to it.",
        );
    }
    if has(&[
        "could not resolve host",
        "unable to access",
        "connection refused",
        "connection timed out",
        "network is unreachable",
        "operation timed out",
        "failed to connect",
    ]) {
        return e(
            "Could not reach the remote.",
            "Check your network connection (or VPN) and try again.",
        );
    }
    if has(&["timed out after"]) {
        return e(
            "Git took too long and was stopped.",
            "Try again; for a large repository, run the command in the Terminal tab.",
        );
    }
    if has(&["not possible to fast-forward", "diverging branches", "have diverged"]) {
        return e(
            "Your branch and the remote branch have both changed.",
            "Pull with merge or rebase (Settings ▸ Git ▸ Pull), or combine them yourself in the Terminal tab.",
        );
    }
    if has(&["would be overwritten by merge", "would be overwritten by checkout", "your local changes to the following files"]) {
        return e(
            "Uncommitted changes are in the way.",
            "Commit or stash your changes to the files listed in the details, then pull again.",
        );
    }
    if has(&["conflict", "automatic merge failed", "could not apply"]) {
        return e(
            "The pull stopped with conflicts.",
            "Resolve the conflicted files (Git ▸ Changes), then commit; or abort with `git merge --abort` / `git rebase --abort` in the Terminal tab.",
        );
    }
    if has(&["has no upstream branch", "no tracking information", "there is no tracking information"]) {
        return e(
            "This branch has no remote branch to pull from.",
            "Push it first (that publishes it), or set its upstream in the Terminal tab.",
        );
    }
    if has(&["src refspec", "does not match any"]) {
        return e(
            "There is nothing to push on this branch yet.",
            "Make a commit first.",
        );
    }
    if has(&["no such remote", "no configured push destination", "no remote"]) {
        return e(
            "This repository has no remote to use.",
            "Add one with `git remote add origin <url>` in the Terminal tab.",
        );
    }
    // Git's own words: its first fatal/error line, else the first line
    // that is not a hint or progress.
    let line = message
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("fatal:") || l.starts_with("error:"))
        .or_else(|| {
            message
                .lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with("hint:") && !l.contains('\r'))
        })
        .unwrap_or("Git reported an error.");
    let line = line
        .trim_start_matches("fatal:")
        .trim_start_matches("error:")
        .trim();
    let mut title = format!("{} failed: {line}", op.label());
    if !title.ends_with('.') {
        title.push('.');
    }
    Explanation { title, hint: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process::{self, Cmd};

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

    /// A bare "remote", a clone that pushes, and a second clone that
    /// fetches and pulls.
    #[test]
    fn push_fetch_and_pull_are_described() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let d = tmp.path().canonicalize()?;
        sh(
            &d,
            "git init -q --bare -b main remote.git && git clone -q remote.git a 2>/dev/null \
             && cd a && git config commit.gpgsign false && git checkout -q -b main \
             && printf '1\\n' > f.txt && git add f.txt && git commit -qm one \
             && git push -q -u origin main 2>/dev/null && cd .. && git clone -q remote.git b 2>/dev/null",
        )?;
        let git = Git::default();
        let (a, b) = (d.join("a"), d.join("b"));
        sh(&a, "printf '2\\n' >> f.txt && printf 'n\\n' > new.txt && git add . && git commit -qm two && printf '3\\n' >> f.txt && git commit -qam three")?;

        // Push from a: sent two commits to origin/main, with their files.
        let (result, e) = run_logged(&git, &a, &NetworkOp::Push { set_upstream: None }, &CancelToken::new(), None);
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(e.op, Op::Push);
        assert_eq!(e.branch.as_deref(), Some("main"));
        assert_eq!(e.sent.len(), 1);
        let sent = &e.sent[0];
        assert_eq!(sent.name, "origin/main");
        let summaries: Vec<&str> = sent.commits.iter().map(|c| c.summary.as_str()).collect();
        assert_eq!(summaries, ["three", "two"]);
        let files: Vec<(&str, &str)> = sent.files.iter().map(|f| (f.status.as_str(), f.path.as_str())).collect();
        assert_eq!(files, [("M", "f.txt"), ("A", "new.txt")]);

        // Fetch in b: received the same.
        let (_, e) = run_logged(&git, &b, &NetworkOp::Fetch, &CancelToken::new(), None);
        assert_eq!(e.received.len(), 1);
        assert_eq!(e.received[0].commits.len(), 2);
        assert!(e.local.is_none());

        // Pull in b: the branch itself moves.
        let (result, e) = run_logged(
            &git,
            &b,
            &NetworkOp::Pull { mode: crate::git::PullMode::FastForwardOnly },
            &CancelToken::new(),
            None,
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(e.received.is_empty(), "already fetched");
        let local = e.local.as_ref().map(|l| (l.name.as_str(), l.commits.len(), l.files.len()));
        assert_eq!(local, Some(("main", 2, 2)));

        // A failed push is logged with git's message.
        let (result, e) = run_logged(&git, &b, &NetworkOp::Push { set_upstream: Some("nowhere".into()) }, &CancelToken::new(), None);
        assert!(result.is_err());
        assert!(!e.ok && !e.message.is_empty());
        Ok(())
    }

    #[test]
    fn keeps_the_newest_entries() {
        let mut f = ActivityFile::default();
        for i in 0..MAX_ENTRIES + 5 {
            f.push(Entry {
                id: i.to_string(),
                time: i as i64,
                repo: PathBuf::new(),
                op: Op::Fetch,
                branch: None,
                ok: true,
                message: String::new(),
                command: String::new(),
                sent: Vec::new(),
                received: Vec::new(),
                local: None,
            });
        }
        assert_eq!(f.entries.len(), MAX_ENTRIES);
        assert_eq!(f.entries[0].id, "5");
    }

    #[test]
    fn parses_name_status_with_renames() {
        let lines = vec!["M\tsrc/a.rs".to_owned(), "R087\told.rs\tnew.rs".to_owned()];
        let files = parse_file_lines(&lines);
        assert_eq!(files[1].status, "R");
        assert_eq!(files[1].path, "new.rs");
    }

    #[test]
    fn explains_common_failures() {
        let rejected = "To /tmp/remote.git\n ! [rejected]        feature-x -> feature-x (fetch first)\n\
                        error: failed to push some refs to '/tmp/remote.git'\nhint: Updates were rejected";
        assert_eq!(explain(rejected, Op::Push).title, "The remote has commits you don't have.");
        let auth = "remote: Invalid username or password.\nfatal: Authentication failed for 'https://x/y.git/'";
        assert_eq!(explain(auth, Op::Push).title, "Git could not sign in to the remote.");
        let nowhere = "fatal: 'nowhere' does not appear to be a git repository\nfatal: Could not read from remote repository.";
        assert_eq!(explain(nowhere, Op::Push).title, "The remote repository was not found.");
        let host = "fatal: unable to access 'https://github.com/x/y.git/': Could not resolve host: github.com";
        assert_eq!(explain(host, Op::Fetch).title, "Could not reach the remote.");
        let ff = "hint: Diverging branches can't be fast-forwarded\nfatal: Not possible to fast-forward, aborting.";
        assert!(explain(ff, Op::Pull).title.starts_with("Your branch and the remote branch"));
        let dirty = "error: Your local changes to the following files would be overwritten by merge:\n\tsrc/a.rs";
        assert_eq!(explain(dirty, Op::Pull).title, "Uncommitted changes are in the way.");
        // Unknown: git's own first error line, readable.
        let other = "warning: something\nfatal: the frobnicator is broken";
        assert_eq!(explain(other, Op::Fetch).title, "Fetch failed: the frobnicator is broken.");
        assert!(explain(rejected, Op::Push).hint.is_some());
    }
}
