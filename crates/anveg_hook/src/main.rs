//! `anveg-hook`: run by Claude Code for each hook event (build plan, 7.1).
//!
//! Reads the hook's JSON from stdin and sends one JSON line to the Unix
//! socket in `$ANVEG_HOOK_SOCKET`. Rules: print nothing (Claude Code may add
//! hook output to the conversation), always exit 0, and finish fast (100 ms
//! connect/write timeout).
//!
//! The line has the shape of `anveg_core::status::HookMessage`:
//! `{event, session_id, notification_type, source, cwd, error, terminal_id, ts}`.
//!
//! `anveg-hook statusline` is Claude Code's status line command instead
//! (when the app's session toolbar reads it): it sends the model, effort,
//! context use, the plan's usage limits and the session's estimated cost
//! as a `StatusLine` message, then runs the user's own
//! status line command (`$ANVEG_USER_STATUSLINE`) with the same input and
//! passes its output through, so their status line still shows.

use std::io::{Read as _, Write as _};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_millis(100);
const MAX_INPUT: u64 = 1024 * 1024;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("statusline") {
        status_line();
    } else {
        let _ = run();
    }
    std::process::exit(0);
}

fn read_input() -> Option<Vec<u8>> {
    let mut input = Vec::new();
    std::io::stdin()
        .take(MAX_INPUT)
        .read_to_end(&mut input)
        .ok()?;
    Some(input)
}

fn run() -> Option<()> {
    let socket = std::env::var_os("ANVEG_HOOK_SOCKET")?;
    let input = read_input()?;
    let line = message(&input, std::env::var("ANVEG_TERMINAL_ID").ok())?;
    send(std::path::Path::new(&socket), &line)
}

/// Claude Code's status line: report to the app, then show the user's own.
fn status_line() {
    let Some(input) = read_input() else {
        return;
    };
    if let Some(socket) = std::env::var_os("ANVEG_HOOK_SOCKET")
        && let Some(line) = status_message(&input, std::env::var("ANVEG_TERMINAL_ID").ok())
    {
        let _ = send(std::path::Path::new(&socket), &line);
    }
    if let Ok(command) = std::env::var("ANVEG_USER_STATUSLINE")
        && !command.trim().is_empty()
    {
        run_user_status_line(&command, &input);
    }
}

/// Run the user's status line command with Claude Code's input; its output
/// goes straight to Claude Code.
fn run_user_status_line(command: &str, input: &[u8]) {
    use std::process::{Command, Stdio};
    let Ok(mut child) = Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input);
    }
    let _ = child.wait();
}

/// The `StatusLine` message for Claude Code's status line JSON.
fn status_message(input: &[u8], terminal_id: Option<String>) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(input).ok()?;
    let str_at = |path: &[&str]| {
        let mut cur = &v;
        for key in path {
            cur = cur.get(*key)?;
        }
        cur.as_str().map(str::to_owned)
    };
    let num_at = |path: &[&str]| {
        let mut cur = &v;
        for key in path {
            cur = cur.get(*key)?;
        }
        cur.as_f64()
    };
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let out = serde_json::json!({
        "event": "StatusLine",
        "session_id": str_at(&["session_id"]),
        "terminal_id": terminal_id,
        "ts": ts,
        "status_line": {
            "model_id": str_at(&["model", "id"]),
            "model_name": str_at(&["model", "display_name"]),
            "effort": str_at(&["effort", "level"]),
            "used_percentage": num_at(&["context_window", "used_percentage"])
                .map(|p| p.clamp(0.0, 100.0).round() as u8),
            "context_window_size": num_at(&["context_window", "context_window_size"])
                .map(|n| n as u64),
            "context_tokens": num_at(&["context_window", "total_input_tokens"]).map(|n| n as u64),
            // Pro, Max and Ultra only; each window may be missing.
            "five_hour": limit_window(&v, "five_hour"),
            "seven_day": limit_window(&v, "seven_day"),
            "cost_cents": num_at(&["cost", "total_cost_usd"])
                .filter(|c| c.is_finite() && *c >= 0.0)
                .map(|c| (c * 100.0).round() as u64),
        },
    });
    serde_json::to_string(&out).ok()
}

/// `rate_limits.<name>` as `{used_percentage, resets_at}` (whole
/// percent, Unix seconds), or null.
fn limit_window(v: &serde_json::Value, name: &str) -> serde_json::Value {
    let window = v.get("rate_limits").and_then(|r| r.get(name));
    let used = window
        .and_then(|w| w.get("used_percentage"))
        .and_then(serde_json::Value::as_f64)
        .filter(|p| p.is_finite());
    let resets = window
        .and_then(|w| w.get("resets_at"))
        .and_then(serde_json::Value::as_f64)
        .filter(|t| t.is_finite() && *t > 0.0);
    match (used, resets) {
        (Some(used), Some(resets)) => serde_json::json!({
            "used_percentage": used.clamp(0.0, 100.0).round() as u8,
            "resets_at": resets as i64,
        }),
        _ => serde_json::Value::Null,
    }
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
        // `StopFailure` only: `rate_limit`, `server_error`, …
        "error": field("error"),
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
    fn status_line_message() {
        let input = br#"{"session_id":"s","model":{"id":"claude-opus-5-5","display_name":"Opus 5.5"},
            "effort":{"level":"high"},"context_window":{"total_input_tokens":62000,
            "context_window_size":200000,"used_percentage":31.4,"remaining_percentage":68.6}}"#;
        let line = status_message(input, Some("t".into())).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        assert_eq!(v["event"], "StatusLine");
        assert_eq!(v["status_line"]["model_name"], "Opus 5.5");
        assert_eq!(v["status_line"]["effort"], "high");
        assert_eq!(v["status_line"]["used_percentage"], 31);
        assert_eq!(v["status_line"]["context_window_size"], 200000);
        assert!(v["status_line"]["five_hour"].is_null());
        assert!(v["status_line"]["cost_cents"].is_null());
        // Older versions without these fields still give a message.
        assert!(status_message(br#"{"session_id":"s"}"#, None).is_some());
    }

    #[test]
    fn status_line_plan_limits_and_cost() {
        let input = br#"{"session_id":"s","cost":{"total_cost_usd":1.2345},
            "rate_limits":{"five_hour":{"used_percentage":23.5,"resets_at":1738425600},
            "seven_day":{"used_percentage":41.2}}}"#;
        let line = status_message(input, None).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        assert_eq!(v["status_line"]["five_hour"]["used_percentage"], 24);
        assert_eq!(v["status_line"]["five_hour"]["resets_at"], 1738425600);
        // No reset time: not usable.
        assert!(v["status_line"]["seven_day"].is_null());
        assert_eq!(v["status_line"]["cost_cents"], 123);
    }

    #[test]
    fn stop_failure_carries_the_error() {
        let input = br#"{"session_id":"s","hook_event_name":"StopFailure","error":"rate_limit",
            "error_details":"429 Too Many Requests"}"#;
        let line = message(input, None).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&line).unwrap_or_default();
        assert_eq!(v["event"], "StopFailure");
        assert_eq!(v["error"], "rate_limit");
        // Details may hold request text: not sent.
        assert!(v.get("error_details").is_none());
    }

    #[test]
    fn bad_input_gives_nothing() {
        assert_eq!(message(b"not json", None), None);
    }
}
