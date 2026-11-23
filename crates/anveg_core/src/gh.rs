//! GitHub through the `gh` command-line tool (next phases, Phase 6).
//!
//! Every call uses `--json` with an explicit field list and parses it with
//! serde; missing fields are optional. Blocking: run on a background thread.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Result, bail};
use serde::Deserialize;

use crate::claude_env::EnvMap;
use crate::process::{self, Cmd, Output};

/// How to run gh.
#[derive(Clone, Debug)]
pub struct Gh {
    pub program: PathBuf,
    pub env: Option<EnvMap>,
}

/// What `gh --version` and `gh auth status` said.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GhState {
    Missing,
    SignedOut { version: String },
    Ready { version: String },
}

/// GitHub said "too many requests".
#[derive(Debug)]
pub struct RateLimited(pub String);

impl std::fmt::Display for RateLimited {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for RateLimited {}

pub fn is_rate_limited(message: &str) -> bool {
    let m = message.to_lowercase();
    m.contains("rate limit") || m.contains("too many requests")
}

impl Gh {
    pub fn new(program: PathBuf, env: Option<EnvMap>) -> Self {
        Self { program, env }
    }

    pub fn cmd(&self, dir: &Path) -> Cmd {
        Cmd::new(&self.program)
            .env(self.env.as_ref())
            .cwd(dir)
            .var("GH_PROMPT_DISABLED", "1")
            .var("GH_NO_UPDATE_NOTIFIER", "1")
            .var("NO_COLOR", "1")
            .var("GIT_TERMINAL_PROMPT", "0")
            .timeout(Duration::from_secs(60))
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Result<Output> {
        let out = process::run(&self.cmd(dir).args(args.iter().copied()))?;
        if !out.success() && is_rate_limited(&out.message()) {
            return Err(RateLimited(out.message()).into());
        }
        Ok(out)
    }

    fn run_ok(&self, dir: &Path, args: &[&str]) -> Result<Output> {
        self.run(dir, args)?.check()
    }

    fn json<T: serde::de::DeserializeOwned>(&self, dir: &Path, args: &[&str]) -> Result<T> {
        self.run_ok(dir, args)?.json()
    }

    /// `gh --version` and `gh auth status`.
    pub fn detect(&self, dir: &Path) -> GhState {
        let Ok(out) = process::run(&self.cmd(dir).arg("--version").timeout(Duration::from_secs(10)))
        else {
            return GhState::Missing;
        };
        if !out.success() {
            return GhState::Missing;
        }
        let version = parse_version(&out.stdout_str());
        match process::run(
            &self
                .cmd(dir)
                .args(["auth", "status"])
                .timeout(Duration::from_secs(20)),
        ) {
            Ok(o) if o.success() => GhState::Ready { version },
            _ => GhState::SignedOut { version },
        }
    }

    pub fn repo(&self, dir: &Path) -> Result<RepoInfo> {
        self.json(dir, &["repo", "view", "--json", "nameWithOwner,url,defaultBranchRef"])
    }

    pub fn pr_list(&self, dir: &Path, filter: PrFilter) -> Result<Vec<PrSummary>> {
        self.json(
            dir,
            &[
                "pr",
                "list",
                "--json",
                PR_LIST_FIELDS,
                "--search",
                filter.search(),
                "--limit",
                "50",
            ],
        )
    }

    pub fn pr_view(&self, dir: &Path, number: u64) -> Result<PrDetail> {
        let n = number.to_string();
        self.json(dir, &["pr", "view", &n, "--json", PR_VIEW_FIELDS])
    }

    /// `gh pr checks` exits 1 when a check failed and 8 while some are
    /// pending; the JSON is valid either way.
    pub fn pr_checks(&self, dir: &Path, number: u64) -> Result<Vec<Check>> {
        let n = number.to_string();
        let out = self.run(
            dir,
            &[
                "pr",
                "checks",
                &n,
                "--json",
                "name,state,bucket,link,workflow,description",
            ],
        )?;
        if out.stdout.iter().all(u8::is_ascii_whitespace) {
            let message = out.message();
            if message.contains("no checks reported") {
                return Ok(Vec::new());
            }
            out.check()?;
            return Ok(Vec::new());
        }
        out.json()
    }

    pub fn pr_diff(&self, dir: &Path, number: u64) -> Result<String> {
        let n = number.to_string();
        Ok(self
            .run_ok(dir, &["pr", "diff", &n, "--color", "never"])?
            .stdout_str())
    }

    pub fn pr_action(&self, dir: &Path, number: u64, action: &PrAction) -> Result<String> {
        let n = number.to_string();
        let args: Vec<&str> = match action {
            PrAction::Checkout => vec!["pr", "checkout", &n],
            PrAction::Ready => vec!["pr", "ready", &n],
            PrAction::Close => vec!["pr", "close", &n],
            PrAction::Merge(method) => vec!["pr", "merge", &n, method.flag()],
        };
        let cmd = self
            .cmd(dir)
            .args(args)
            .timeout(Duration::from_secs(300))
            .locked(dir);
        let out = process::run(&cmd)?;
        if out.success() {
            Ok(out.message())
        } else {
            bail!("{}", out.message())
        }
    }

    pub fn pr_create(&self, dir: &Path, new: &NewPr) -> Result<String> {
        let mut cmd = self
            .cmd(dir)
            .args(["pr", "create", "--base", &new.base, "--title", &new.title])
            .args(["--body", &new.body])
            .timeout(Duration::from_secs(120));
        if new.draft {
            cmd = cmd.arg("--draft");
        }
        let out = process::run(&cmd)?;
        if out.success() {
            Ok(out.stdout_str().trim().to_owned())
        } else {
            bail!("{}", out.message())
        }
    }

    pub fn run_list(&self, dir: &Path, branch: &str) -> Result<Vec<Run>> {
        self.json(
            dir,
            &[
                "run",
                "list",
                "--branch",
                branch,
                "--limit",
                "20",
                "--json",
                RUN_FIELDS,
            ],
        )
    }

    pub fn run_view(&self, dir: &Path, id: u64) -> Result<RunDetail> {
        let id = id.to_string();
        self.json(dir, &["run", "view", &id, "--json", RUN_VIEW_FIELDS])
    }

    pub fn run_failed_log(&self, dir: &Path, id: u64) -> Result<String> {
        let id = id.to_string();
        let cmd = self
            .cmd(dir)
            .args(["run", "view", &id, "--log-failed"])
            .timeout(Duration::from_secs(120));
        Ok(process::run(&cmd)?.check()?.stdout_str())
    }

    pub fn run_rerun_failed(&self, dir: &Path, id: u64) -> Result<()> {
        let id = id.to_string();
        self.run_ok(dir, &["run", "rerun", &id, "--failed"]).map(drop)
    }
}

pub const PR_LIST_FIELDS: &str =
    "number,title,author,headRefName,isDraft,reviewDecision,statusCheckRollup,updatedAt";
pub const PR_VIEW_FIELDS: &str = "number,title,author,body,baseRefName,headRefName,isDraft,state,reviewDecision,url,reviews,comments,statusCheckRollup,additions,deletions,changedFiles";
pub const RUN_FIELDS: &str =
    "databaseId,displayTitle,status,conclusion,workflowName,event,headBranch,createdAt,url";
pub const RUN_VIEW_FIELDS: &str =
    "databaseId,displayTitle,status,conclusion,workflowName,headBranch,url,jobs";

/// The first line of `gh --version`: "gh version 2.63.0 (2024-…)".
pub fn parse_version(text: &str) -> String {
    text.lines()
        .next()
        .and_then(|l| l.strip_prefix("gh version "))
        .and_then(|l| l.split_whitespace().next())
        .unwrap_or("")
        .to_owned()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PrFilter {
    #[default]
    Mine,
    ReviewRequested,
    AllOpen,
}

impl PrFilter {
    pub fn search(self) -> &'static str {
        match self {
            Self::Mine => "is:open author:@me",
            Self::ReviewRequested => "is:open review-requested:@me",
            Self::AllOpen => "is:open",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Mine => "Mine",
            Self::ReviewRequested => "Review Requested",
            Self::AllOpen => "All Open",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeMethod {
    Merge,
    Squash,
    Rebase,
}

impl MergeMethod {
    pub fn flag(self) -> &'static str {
        match self {
            Self::Merge => "--merge",
            Self::Squash => "--squash",
            Self::Rebase => "--rebase",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Merge => "Create a merge commit",
            Self::Squash => "Squash and merge",
            Self::Rebase => "Rebase and merge",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrAction {
    Checkout,
    Ready,
    Merge(MergeMethod),
    Close,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewPr {
    pub base: String,
    pub title: String,
    pub body: String,
    pub draft: bool,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct RepoInfo {
    pub name_with_owner: String,
    pub url: String,
    pub default_branch_ref: Option<BranchRef>,
}

impl RepoInfo {
    pub fn default_branch(&self) -> Option<&str> {
        self.default_branch_ref
            .as_ref()
            .map(|b| b.name.as_str())
            .filter(|n| !n.is_empty())
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct BranchRef {
    pub name: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct Author {
    pub login: String,
}

/// One entry of `statusCheckRollup`: a check run or a commit status.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct RollupItem {
    #[serde(rename = "__typename")]
    pub typename: String,
    pub name: Option<String>,
    pub context: Option<String>,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub state: Option<String>,
}

/// The one icon summarising a PR's checks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChecksSummary {
    None,
    Pending,
    Passing,
    Failing,
}

pub fn summarize_rollup(items: &[RollupItem]) -> ChecksSummary {
    if items.is_empty() {
        return ChecksSummary::None;
    }
    let mut pending = false;
    for item in items {
        let conclusion = item
            .conclusion
            .as_deref()
            .or(item.state.as_deref())
            .unwrap_or("")
            .to_uppercase();
        let status = item.status.as_deref().unwrap_or("COMPLETED").to_uppercase();
        if matches!(
            conclusion.as_str(),
            "FAILURE" | "TIMED_OUT" | "CANCELLED" | "ACTION_REQUIRED" | "ERROR" | "STARTUP_FAILURE"
        ) {
            return ChecksSummary::Failing;
        }
        if status != "COMPLETED" || matches!(conclusion.as_str(), "PENDING" | "EXPECTED" | "") {
            pending = true;
        }
    }
    if pending {
        ChecksSummary::Pending
    } else {
        ChecksSummary::Passing
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct PrSummary {
    pub number: u64,
    pub title: String,
    pub author: Option<Author>,
    pub head_ref_name: String,
    pub is_draft: bool,
    pub review_decision: Option<String>,
    pub status_check_rollup: Vec<RollupItem>,
    pub updated_at: Option<String>,
}

impl PrSummary {
    pub fn checks(&self) -> ChecksSummary {
        summarize_rollup(&self.status_check_rollup)
    }
}

/// "Approved", "Changes requested", "Review required", or nothing.
pub fn review_label(decision: Option<&str>) -> Option<&'static str> {
    match decision? {
        "APPROVED" => Some("Approved"),
        "CHANGES_REQUESTED" => Some("Changes requested"),
        "REVIEW_REQUIRED" => Some("Review required"),
        _ => None,
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Review {
    pub author: Option<Author>,
    pub state: String,
    pub body: String,
    pub submitted_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Comment {
    pub author: Option<Author>,
    pub body: String,
    pub created_at: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct PrDetail {
    pub number: u64,
    pub title: String,
    pub author: Option<Author>,
    pub body: String,
    pub base_ref_name: String,
    pub head_ref_name: String,
    pub is_draft: bool,
    pub state: String,
    pub review_decision: Option<String>,
    pub url: String,
    pub reviews: Vec<Review>,
    pub comments: Vec<Comment>,
    pub status_check_rollup: Vec<RollupItem>,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Check {
    pub name: String,
    pub state: String,
    /// pass, fail, pending, skipping, cancel.
    pub bucket: String,
    pub link: String,
    pub workflow: String,
    pub description: String,
}

impl Check {
    pub fn failed(&self) -> bool {
        self.bucket == "fail"
    }

    /// The Actions run id in a check's link (`…/actions/runs/<id>/job/…`).
    pub fn run_id(&self) -> Option<u64> {
        let rest = self.link.split("/actions/runs/").nth(1)?;
        rest.split('/').next()?.parse().ok()
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Run {
    pub database_id: u64,
    pub display_title: String,
    pub status: String,
    pub conclusion: String,
    pub workflow_name: String,
    pub event: String,
    pub head_branch: String,
    pub created_at: Option<String>,
    pub url: String,
}

/// A run's or job's state as one word.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunState {
    Pending,
    Success,
    Failure,
    Cancelled,
    Skipped,
}

pub fn run_state(status: &str, conclusion: &str) -> RunState {
    if !status.eq_ignore_ascii_case("completed") {
        return RunState::Pending;
    }
    match conclusion.to_lowercase().as_str() {
        "success" => RunState::Success,
        "cancelled" => RunState::Cancelled,
        "skipped" | "neutral" => RunState::Skipped,
        _ => RunState::Failure,
    }
}

impl Run {
    pub fn state(&self) -> RunState {
        run_state(&self.status, &self.conclusion)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Job {
    pub database_id: u64,
    pub name: String,
    pub status: String,
    pub conclusion: String,
    pub url: String,
}

impl Job {
    pub fn state(&self) -> RunState {
        run_state(&self.status, &self.conclusion)
    }
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct RunDetail {
    pub database_id: u64,
    pub display_title: String,
    pub status: String,
    pub conclusion: String,
    pub workflow_name: String,
    pub head_branch: String,
    pub url: String,
    pub jobs: Vec<Job>,
}

impl RunDetail {
    pub fn state(&self) -> RunState {
        run_state(&self.status, &self.conclusion)
    }
}

/// The last `max_lines` lines of a log.
pub fn log_tail(log: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = log.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

/// A PR title and body from the branch's commits (oldest first): one commit
/// gives its subject and body; several give the branch name and a list.
pub fn pr_text_from_commits(branch: &str, commits: &[(String, String)]) -> (String, String) {
    match commits {
        [] => (branch.replace(['-', '_'], " "), String::new()),
        [(subject, body)] => (subject.clone(), body.clone()),
        many => {
            let title = branch.replace(['-', '_'], " ");
            let body = many
                .iter()
                .map(|(s, _)| format!("- {s}"))
                .collect::<Vec<_>>()
                .join("\n");
            (title, body)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pr_list() -> Result<()> {
        let json = r#"[{"number":12,"title":"Fix it","author":{"login":"a","name":"A"},
            "headRefName":"fix","isDraft":true,"reviewDecision":"APPROVED",
            "statusCheckRollup":[
              {"__typename":"CheckRun","name":"ci","status":"COMPLETED","conclusion":"SUCCESS"},
              {"__typename":"StatusContext","context":"x","state":"PENDING"}],
            "updatedAt":"2026-11-20T10:00:00Z"},
            {"number":13,"title":"Other"}]"#;
        let prs: Vec<PrSummary> = serde_json::from_str(json)?;
        assert_eq!(prs.len(), 2);
        assert!(prs[0].is_draft);
        assert_eq!(prs[0].checks(), ChecksSummary::Pending);
        assert_eq!(prs[1].checks(), ChecksSummary::None);
        assert_eq!(review_label(prs[0].review_decision.as_deref()), Some("Approved"));
        assert_eq!(review_label(None), None);
        Ok(())
    }

    #[test]
    fn rollup_summary() {
        let ok = RollupItem {
            status: Some("COMPLETED".into()),
            conclusion: Some("SUCCESS".into()),
            ..Default::default()
        };
        let bad = RollupItem {
            status: Some("COMPLETED".into()),
            conclusion: Some("FAILURE".into()),
            ..Default::default()
        };
        assert_eq!(summarize_rollup(std::slice::from_ref(&ok)), ChecksSummary::Passing);
        assert_eq!(summarize_rollup(&[ok, bad]), ChecksSummary::Failing);
    }

    #[test]
    fn parses_detail_checks_runs() -> Result<()> {
        let detail: PrDetail = serde_json::from_str(
            r#"{"number":5,"title":"T","body":"**hi**","baseRefName":"main","headRefName":"f",
            "url":"https://github.com/o/r/pull/5","reviews":[{"author":{"login":"r"},"state":"APPROVED","body":""}],
            "comments":[{"author":{"login":"c"},"body":"nice"}],"state":"OPEN"}"#,
        )?;
        assert_eq!(detail.base_ref_name, "main");
        assert_eq!(detail.reviews[0].state, "APPROVED");
        let checks: Vec<Check> = serde_json::from_str(
            r#"[{"name":"build","state":"FAILURE","bucket":"fail",
            "link":"https://github.com/o/r/actions/runs/123/job/456","workflow":"CI"}]"#,
        )?;
        assert!(checks[0].failed());
        assert_eq!(checks[0].run_id(), Some(123));
        let runs: Vec<Run> = serde_json::from_str(
            r#"[{"databaseId":9,"displayTitle":"x","status":"completed","conclusion":"failure","workflowName":"CI"},
                {"databaseId":10,"status":"in_progress","conclusion":""}]"#,
        )?;
        assert_eq!(runs[0].state(), RunState::Failure);
        assert_eq!(runs[1].state(), RunState::Pending);
        let detail: RunDetail = serde_json::from_str(
            r#"{"databaseId":9,"status":"completed","conclusion":"success",
            "jobs":[{"databaseId":1,"name":"test","status":"completed","conclusion":"skipped"}]}"#,
        )?;
        assert_eq!(detail.state(), RunState::Success);
        assert_eq!(detail.jobs[0].state(), RunState::Skipped);
        Ok(())
    }

    #[test]
    fn helpers() {
        assert_eq!(parse_version("gh version 2.63.0 (2024-11-27)\nhttps://…"), "2.63.0");
        assert_eq!(log_tail("a\nb\nc", 2), "b\nc");
        assert!(is_rate_limited("HTTP 403: API rate limit exceeded"));
        assert_eq!(
            pr_text_from_commits("fix-login", &[("Fix login".into(), "Body".into())]),
            ("Fix login".into(), "Body".into())
        );
        let (t, b) = pr_text_from_commits(
            "fix-login",
            &[("One".into(), String::new()), ("Two".into(), String::new())],
        );
        assert_eq!(t, "fix login");
        assert_eq!(b, "- One\n- Two");
        assert_eq!(PrFilter::ReviewRequested.search(), "is:open review-requested:@me");
    }

    #[test]
    fn detect_with_fake_gh() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let fake = tmp.path().join("gh");
        std::fs::write(
            &fake,
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'gh version 9.9.9 (x)'; exit 0; fi\n\
             if [ \"$1\" = auth ]; then echo 'not logged in' >&2; exit 1; fi\n",
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755))?;
        }
        let gh = Gh::new(fake, None);
        assert_eq!(
            gh.detect(tmp.path()),
            GhState::SignedOut {
                version: "9.9.9".into()
            }
        );
        let missing = Gh::new(tmp.path().join("nope"), None);
        assert_eq!(missing.detect(tmp.path()), GhState::Missing);
        Ok(())
    }
}
