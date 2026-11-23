//! `anveg-hook`: run by Claude Code for each hook event (build plan, 7.1).
//!
//! Reads the hook's JSON from stdin and sends one JSON line to the Unix
//! socket in `$ANVEG_HOOK_SOCKET`. Rules: print nothing (Claude Code may add
//! hook output to the conversation), always exit 0, and finish fast (100 ms
//! connect/write timeout).
//!
//! The line has the shape of `anveg_core::status::HookMessage`:
//! `{event, session_id, notification_type, source, cwd, terminal_id, ts}`.

use std::io::{Read as _, Write as _};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_millis(100);
const MAX_INPUT: u64 = 1024 * 1024;

fn main() {
    let _ = run();
    std::process::exit(0);
}

fn run() -> Option<()> {
    let socket = std::env::var_os("ANVEG_HOOK_SOCKET")?;
    let mut input = Vec::new();
    std::io::stdin()
        .take(MAX_INPUT)
        .read_to_end(&mut input)
        .ok()?;
    let line = message(&input, std::env::var("ANVEG_TERMINAL_ID").ok())?;
    send(std::path::Path::new(&socket), &line)
}

/// The line to send for a hook's stdin JSON.
fn message(input: &[u8], terminal_id: Option<String>) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(input).ok()?;
    let field = |key: &str| value.get(key).cloned().unwrap_or(serde_json::Value::Null);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let out = serde_json::json!({
        "event": field("hook_event_name"),
        "session_id": field("session_id"),
        "notification_type": field("notification_type"),
        "source": field("source"),
        "cwd": field("cwd"),
        "terminal_id": terminal_id,
        "ts": ts,
    });
    serde_json::to_string(&out).ok()
}

#[cfg(unix)]
fn send(socket: &std::path::Path, line: &str) -> Option<()> {
    use std::os::unix::net::UnixStream;
    // UnixStream has no connect timeout; a local socket connects at once or
    // fails at once, and the write has a timeout.
    let mut stream = UnixStream::connect(socket).ok()?;
    stream.set_write_timeout(Some(TIMEOUT)).ok()?;
    stream.write_all(line.as_bytes()).ok()?;
    stream.write_all(b"\n").ok()?;
    Some(())
}

#[cfg(not(unix))]
fn send(_: &std::path::Path, _: &str) -> Option<()> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_line() {
        let input =
            br#"{"session_id":"s","hook_event_name":"Stop","cwd":"/p","stop_hook_active":false}"#;
        let line = message(input, Some("t1".into())).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        assert_eq!(v["event"], "Stop");
        assert_eq!(v["session_id"], "s");
        assert_eq!(v["terminal_id"], "t1");
        assert!(v["notification_type"].is_null());
        assert!(v["ts"].as_i64().unwrap_or(0) > 0);
    }

    #[test]
    fn bad_input_gives_nothing() {
        assert_eq!(message(b"not json", None), None);
    }
}
