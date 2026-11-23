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
fn no_socket_still_exits_zero_silently() -> anyhow::Result<()> {
    let out = Command::new(env!("CARGO_BIN_EXE_anveg-hook"))
        .env("ANVEG_HOOK_SOCKET", "/nonexistent/anveg.sock")
        .stdin(Stdio::null())
        .output()?;
    assert!(out.status.success());
    assert!(out.stdout.is_empty() && out.stderr.is_empty());
    Ok(())
}
