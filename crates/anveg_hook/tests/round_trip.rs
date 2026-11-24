//! `anveg-hook` → Unix socket → `HookServer` (build plan, 7.1).

#![cfg(unix)]

use std::io::Write as _;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anveg_core::status::{HookMessage, HookServer};

#[test]
fn a_hook_event_reaches_the_app() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let socket = tmp.path().join("hooks.sock");
    let (tx, rx) = mpsc::channel::<HookMessage>();
    let _server = HookServer::start(socket.clone(), move |m| {
        let _ = tx.send(m);
    })?;

    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_anveg-hook"))
        .env("ANVEG_HOOK_SOCKET", &socket)
        .env("ANVEG_TERMINAL_ID", "term-7")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(
            br#"{"session_id":"s-1","hook_event_name":"Notification","notification_type":"permission_prompt","cwd":"/p"}"#,
        )?;
    }
    let out = child.wait_with_output()?;
    let elapsed = started.elapsed();
    assert!(out.status.success());
    assert!(
        out.stdout.is_empty() && out.stderr.is_empty(),
        "the hook must print nothing"
    );
    assert!(elapsed < Duration::from_millis(500), "took {elapsed:?}");

    let m = rx.recv_timeout(Duration::from_secs(5))?;
    assert_eq!(m.event, "Notification");
    assert_eq!(m.session_id, "s-1");
    assert_eq!(m.notification_type.as_deref(), Some("permission_prompt"));
    assert_eq!(m.terminal_id.as_deref(), Some("term-7"));
    Ok(())
}

#[test]
fn the_status_line_reaches_the_app_and_runs_the_users_own() -> anyhow::Result<()> {
    let tmp = tempfile::tempdir()?;
    let socket = tmp.path().join("hooks.sock");
    let (tx, rx) = mpsc::channel::<HookMessage>();
    let _server = HookServer::start(socket.clone(), move |m| {
        let _ = tx.send(m);
    })?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_anveg-hook"))
        .arg("statusline")
        .env("ANVEG_HOOK_SOCKET", &socket)
        .env("ANVEG_TERMINAL_ID", "term-8")
        // The user's own status line reads the same JSON.
        .env("ANVEG_USER_STATUSLINE", "grep -o 'Opus 5.5' | head -1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(
            br#"{"session_id":"s-2","model":{"id":"m","display_name":"Opus 5.5"},"effort":{"level":"low"},"context_window":{"used_percentage":87.6,"context_window_size":200000,"total_input_tokens":175000}}"#,
        )?;
    }
    let out = child.wait_with_output()?;
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "Opus 5.5");
    let m = rx.recv_timeout(Duration::from_secs(5))?;
    assert_eq!(m.event, anveg_core::status::STATUS_LINE_EVENT);
    assert_eq!(m.terminal_id.as_deref(), Some("term-8"));
    let line = m.status_line.unwrap_or_default();
    assert_eq!(line.used_percentage, Some(88));
    assert_eq!(line.effort.as_deref(), Some("low"));
    assert_eq!(line.context_tokens, Some(175_000));
    Ok(())
}

#[test]
fn no_socket_still_exits_zero_silently() -> anyhow::Result<()> {
    let out = Command::new(env!("CARGO_BIN_EXE_anveg-hook"))
        .env("ANVEG_HOOK_SOCKET", "/nonexistent/anveg.sock")
        .stdin(Stdio::null())
        .output()?;
    assert!(out.status.success());
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    Ok(())
}
