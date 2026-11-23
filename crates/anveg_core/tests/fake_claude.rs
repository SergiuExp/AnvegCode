//! End-to-end: the launcher's command line, `scripts/fake-claude`, and the
//! session index reading what it wrote (build plan, section 10).

#![cfg(unix)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anveg_core::launcher::{ClaudeOptions, SessionStart, claude_args};
use anveg_core::session_index::SessionIndex;

fn fake_claude() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/fake-claude")
}

fn run(cwd: &Path, config: &Path, args: &[String], input: &str) -> anyhow::Result<String> {
    let mut child = Command::new("bash")
        .arg(fake_claude())
        .args(args)
        .current_dir(cwd)
        .env("CLAUDE_CONFIG_DIR", config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes())?;
    }
    let out = child.wait_with_output()?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn new_session_rename_and_resume_show_up_in_the_index() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let config = tmp.path().join("cfg");
    let project = std::fs::canonicalize(tmp.path())?.join("my project");
    std::fs::create_dir_all(&project)?;
    let options = ClaudeOptions {
        extra_folders: vec!["/x".into()],
        ..ClaudeOptions::default()
    };

    let start = SessionStart::New {
        session_id: "aaaaaaaa-0000-4000-8000-000000000001".into(),
        name: Some("Given name".into()),
    };
    let out = run(
        &project,
        &config,
        &claude_args(&start, &options),
        "Fix the bug\n/exit\n",
    )?;
    assert!(out.contains("echo: Fix the bug"), "{out}");
    assert!(out.contains("--add-dir /x"), "{out}");

    let mut index = SessionIndex::load(config.clone(), project.clone(), tmp.path().join("s.json"));
    assert!(index.refresh());
    let list = index.sessions(false);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].display_name, "Given name");
    assert_eq!(list[0].first_prompt.as_deref(), Some("Fix the bug"));

    // Resume and rename from inside the session.
    let resume = SessionStart::Resume {
        session_id: "aaaaaaaa-0000-4000-8000-000000000001".into(),
    };
    run(
        &project,
        &config,
        &claude_args(&resume, &options),
        "/rename Better name\n/exit\n",
    )?;
    assert!(index.refresh());
    assert_eq!(index.sessions(false)[0].display_name, "Better name");

    // An app alias wins over Claude Code's own name.
    index.set_alias("aaaaaaaa-0000-4000-8000-000000000001", "Mine");
    assert_eq!(index.sessions(false)[0].display_name, "Mine");
    Ok(())
}

#[test]
fn worktree_sessions_land_in_their_own_group() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let config = tmp.path().join("cfg");
    let project = std::fs::canonicalize(tmp.path())?.join("repo");
    std::fs::create_dir_all(&project)?;
    let start = SessionStart::Worktree {
        worktree: "feature".into(),
        session_id: "bbbbbbbb-0000-4000-8000-000000000002".into(),
        name: None,
    };
    run(
        &project,
        &config,
        &claude_args(&start, &ClaudeOptions::default()),
        "hello\n/exit\n",
    )?;
    let mut index = SessionIndex::load(config, project, tmp.path().join("s.json"));
    index.refresh();
    let list = index.sessions(false);
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].worktree.as_deref(), Some("feature"));
    Ok(())
}
