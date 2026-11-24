//! Live session status from Claude Code hooks (build plan, section 7.1).
//!
//! `anveg-hook` (run by Claude Code for each hook event) sends one JSON
//! line per event to a Unix socket the app listens on. The app turns those
//! into a status per session.
//!
//! Spike result (Milestone 10, Claude Code 2.1.330): hooks passed with
//! `--settings <file>` are **merged** with the user's own hooks: both fire.
//! So the app can add its hooks without touching `~/.claude/settings.json`.

use std::io::{BufRead as _, BufReader};
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Environment variable carrying the app's id for the terminal a `claude`
/// runs in, so hook events can be tied to the terminal even after `/clear`
/// starts a new session id in the same process.
pub const TERMINAL_ID_ENV: &str = "ANVEG_TERMINAL_ID";

/// The event name `anveg-hook statusline` sends: Claude Code's status
/// line data (model, effort, context), not a hook.
pub const STATUS_LINE_EVENT: &str = "StatusLine";

/// The user's own status line command, run by `anveg-hook statusline` so
/// their status line still shows in Claude Code.
pub const USER_STATUS_LINE_ENV: &str = "ANVEG_USER_STATUSLINE";

/// What the session toolbar shows, from Claude Code's status line JSON.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct StatusLineInfo {
    pub model_id: Option<String>,
    /// e.g. "Opus 5.5".
    pub model_name: Option<String>,
    /// e.g. "high".
    pub effort: Option<String>,
    /// 0–100.
    pub used_percentage: Option<u8>,
    pub context_window_size: Option<u64>,
    pub context_tokens: Option<u64>,
}

/// How full a session's context window is, for the bar above sessions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextUse {
    /// 0–100.
    pub percent: u8,
    pub tokens: Option<u64>,
    pub window: Option<u64>,
    /// Worked out from the transcript (Claude Code's status line is off
    /// or has not reported yet), so only roughly right.
    pub estimated: bool,
}

/// The context use from the status line, else an estimate from the
/// transcript's last reply: its tokens against a 200k window, or 1M when
/// it is already past 200k.
pub fn context_use(line: Option<&StatusLineInfo>, transcript_tokens: Option<u64>) -> Option<ContextUse> {
    if let Some(line) = line
        && let Some(percent) = line.used_percentage
    {
        return Some(ContextUse {
            percent: percent.min(100),
            tokens: line.context_tokens,
            window: line.context_window_size,
            estimated: false,
        });
    }
    let tokens = transcript_tokens?;
    let window = line
        .and_then(|l| l.context_window_size)
        .filter(|&w| w > 0)
        .unwrap_or(if tokens > 200_000 { 1_000_000 } else { 200_000 });
    let percent = (tokens.saturating_mul(100) + window / 2) / window;
    Some(ContextUse {
        percent: percent.min(100) as u8,
        tokens: Some(tokens),
        window: Some(window),
        estimated: true,
    })
}

/// The hook events the app listens to.
pub const HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PermissionRequest",
    "Notification",
    "Stop",
    "SessionEnd",
];

/// The `Notification` types that matter for status.
pub const NOTIFICATION_MATCHER: &str = "permission_prompt|idle_prompt|elicitation_dialog";

/// One line on the hook socket.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct HookMessage {
    /// `hook_event_name`, e.g. `Stop`.
    pub event: String,
    pub session_id: String,
    pub notification_type: Option<String>,
    /// `SessionStart` only: `startup`, `resume`, `clear`, `compact`, `fork`.
    pub source: Option<String>,
    pub cwd: Option<String>,
    /// The app's terminal id ([`TERMINAL_ID_ENV`]), when set.
    pub terminal_id: Option<String>,
    /// Unix ms when the hook ran.
    pub ts: i64,
    /// [`STATUS_LINE_EVENT`] only.
    pub status_line: Option<StatusLineInfo>,
}

impl HookMessage {
    /// Build the message from the JSON Claude Code passes a hook on stdin.
    pub fn from_hook_input(
        input: &serde_json::Value,
        terminal_id: Option<String>,
        ts: i64,
    ) -> Self {
        let s = |key: &str| input.get(key).and_then(|v| v.as_str()).map(str::to_owned);
        Self {
            event: s("hook_event_name").unwrap_or_default(),
            session_id: s("session_id").unwrap_or_default(),
            notification_type: s("notification_type"),
            source: s("source"),
            cwd: s("cwd"),
            terminal_id,
            ts,
            status_line: None,
        }
    }
}

/// A session's status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SessionStatus {
    /// Waiting for the user's next prompt.
    #[default]
    Idle,
    /// Claude is working on a turn.
    Working,
    /// Claude asked for a permission or an answer.
    NeedsInput,
    /// The process is not running.
    Stopped,
}

/// What can change a status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusEvent {
    ProcessStarted,
    ProcessExited,
    Hook {
        event: String,
        notification_type: Option<String>,
    },
}

/// The status rules (build plan, table in 7.1).
pub fn next_status(current: SessionStatus, event: &StatusEvent) -> SessionStatus {
    match event {
        StatusEvent::ProcessStarted => SessionStatus::Idle,
        StatusEvent::ProcessExited => SessionStatus::Stopped,
        StatusEvent::Hook {
            event,
            notification_type,
        } => match event.as_str() {
            "SessionStart" => SessionStatus::Idle,
            "UserPromptSubmit" | "PreToolUse" => SessionStatus::Working,
            "PermissionRequest" => SessionStatus::NeedsInput,
            "Notification" => match notification_type.as_deref() {
                Some("permission_prompt" | "elicitation_dialog") => SessionStatus::NeedsInput,
                Some("idle_prompt") => SessionStatus::Idle,
                _ => current,
            },
            "Stop" => SessionStatus::Idle,
            "SessionEnd" => SessionStatus::Stopped,
            _ => current,
        },
    }
}

/// What a status change should announce (section 7.2): a session that now
/// needs input, or one that finished its turn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Announcement {
    NeedsInput,
    Finished,
}

/// Whether going from `before` to `after` is worth a notification.
pub fn announcement(before: SessionStatus, after: SessionStatus) -> Option<Announcement> {
    match (before, after) {
        (b, SessionStatus::NeedsInput) if b != SessionStatus::NeedsInput => {
            Some(Announcement::NeedsInput)
        }
        (SessionStatus::Working, SessionStatus::Idle) => Some(Announcement::Finished),
        _ => None,
    }
}

/// The hooks settings file passed with `--settings`: every event in
/// [`HOOK_EVENTS`] runs `hook_binary`.
pub fn hooks_settings(hook_binary: &Path) -> serde_json::Value {
    let command = shell_words::quote(&hook_binary.to_string_lossy()).into_owned();
    let entry = |matcher: Option<&str>| {
        let hooks = json!([{ "type": "command", "command": command, "timeout": 5 }]);
        match matcher {
            Some(m) => json!([{ "matcher": m, "hooks": hooks }]),
            None => json!([{ "hooks": hooks }]),
        }
    };
    let mut map = serde_json::Map::new();
    for event in HOOK_EVENTS {
        let matcher = (*event == "Notification").then_some(NOTIFICATION_MATCHER);
        map.insert((*event).to_owned(), entry(matcher));
    }
    json!({ "hooks": map })
}

/// [`hooks_settings`] plus a status line that runs `anveg-hook statusline`,
/// for the session toolbar's context meter, model and effort.
pub fn hooks_settings_with_status_line(hook_binary: &Path) -> serde_json::Value {
    let mut settings = hooks_settings(hook_binary);
    let command = format!(
        "{} statusline",
        shell_words::quote(&hook_binary.to_string_lossy())
    );
    if let Some(map) = settings.as_object_mut() {
        map.insert(
            "statusLine".to_owned(),
            json!({ "type": "command", "command": command, "padding": 0 }),
        );
    }
    settings
}

/// Listens on a Unix socket for [`HookMessage`] lines.
#[cfg(unix)]
pub struct HookServer {
    path: PathBuf,
}

#[cfg(unix)]
impl HookServer {
    /// Bind `path` (replacing a stale socket) and call `on_message` for each
    /// message, on a background thread.
    pub fn start(path: PathBuf, on_message: impl Fn(HookMessage) + Send + 'static) -> Result<Self> {
        use std::os::unix::net::UnixListener;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)
            .with_context(|| format!("could not listen on {}", path.display()))?;
        std::thread::Builder::new()
            .name("anveg-hook-server".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(500)));
                    let reader = BufReader::new(stream);
                    for line in reader.lines() {
                        let Ok(line) = line else { break };
                        match serde_json::from_str::<HookMessage>(&line) {
                            Ok(message) => on_message(message),
                            Err(error) => tracing::debug!(%error, "bad hook message"),
                        }
                    }
                }
            })?;
        tracing::info!(path = %path.display(), "hook server listening");
        Ok(Self { path })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
impl Drop for HookServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_line_settings_keep_the_hooks() {
        let v = hooks_settings_with_status_line(Path::new("/A pp/anveg-hook"));
        assert!(v["hooks"]["Stop"].is_array());
        assert_eq!(v["statusLine"]["type"], "command");
        assert_eq!(v["statusLine"]["command"], "'/A pp/anveg-hook' statusline");
        // A status line message changes no status.
        let s = next_status(
            SessionStatus::Working,
            &StatusEvent::Hook {
                event: STATUS_LINE_EVENT.into(),
                notification_type: None,
            },
        );
        assert_eq!(s, SessionStatus::Working);
    }

    fn hook(event: &str) -> StatusEvent {
        StatusEvent::Hook {
            event: event.into(),
            notification_type: None,
        }
    }

    fn note(kind: &str) -> StatusEvent {
        StatusEvent::Hook {
            event: "Notification".into(),
            notification_type: Some(kind.into()),
        }
    }

    #[test]
    fn status_rules() {
        use SessionStatus::*;
        assert_eq!(next_status(Stopped, &StatusEvent::ProcessStarted), Idle);
        assert_eq!(next_status(Stopped, &hook("SessionStart")), Idle);
        assert_eq!(next_status(Idle, &hook("UserPromptSubmit")), Working);
        assert_eq!(next_status(NeedsInput, &hook("PreToolUse")), Working);
        assert_eq!(next_status(Working, &hook("PermissionRequest")), NeedsInput);
        assert_eq!(next_status(Working, &note("permission_prompt")), NeedsInput);
        assert_eq!(
            next_status(Working, &note("elicitation_dialog")),
            NeedsInput
        );
        assert_eq!(next_status(Working, &note("idle_prompt")), Idle);
        assert_eq!(next_status(Working, &note("something_else")), Working);
        assert_eq!(next_status(Working, &hook("Stop")), Idle);
        assert_eq!(next_status(Idle, &hook("SessionEnd")), Stopped);
        assert_eq!(next_status(Working, &StatusEvent::ProcessExited), Stopped);
        assert_eq!(next_status(Working, &hook("UnknownEvent")), Working);
    }

    #[test]
    fn announcements() {
        use SessionStatus::*;
        assert_eq!(
            announcement(Working, NeedsInput),
            Some(Announcement::NeedsInput)
        );
        assert_eq!(announcement(NeedsInput, NeedsInput), None);
        assert_eq!(announcement(Working, Idle), Some(Announcement::Finished));
        assert_eq!(announcement(Idle, Idle), None);
        assert_eq!(announcement(Idle, Working), None);
        assert_eq!(announcement(Working, Stopped), None);
    }

    #[test]
    fn message_from_hook_input() {
        let input = json!({
            "session_id": "s1", "hook_event_name": "Notification",
            "notification_type": "permission_prompt", "cwd": "/p", "transcript_path": "/t"
        });
        let m = HookMessage::from_hook_input(&input, Some("term-1".into()), 5);
        assert_eq!(m.event, "Notification");
        assert_eq!(m.session_id, "s1");
        assert_eq!(m.notification_type.as_deref(), Some("permission_prompt"));
        assert_eq!(m.terminal_id.as_deref(), Some("term-1"));
        let line = serde_json::to_string(&m).unwrap_or_default();
        assert_eq!(serde_json::from_str::<HookMessage>(&line).ok(), Some(m));
    }

    #[test]
    fn settings_cover_every_event_and_quote_the_path() {
        let v = hooks_settings(Path::new(
            "/Applications/Anveg Code.app/Contents/MacOS/anveg-hook",
        ));
        let hooks = &v["hooks"];
        for event in HOOK_EVENTS {
            assert!(hooks.get(*event).is_some(), "{event}");
        }
        let cmd = hooks["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap_or_default();
        assert_eq!(
            cmd,
            "'/Applications/Anveg Code.app/Contents/MacOS/anveg-hook'"
        );
        assert_eq!(hooks["Notification"][0]["matcher"], NOTIFICATION_MATCHER);
        assert!(hooks["Stop"][0].get("matcher").is_none());
    }

    #[test]
    #[cfg(unix)]
    fn server_receives_lines() -> Result<()> {
        use std::io::Write as _;
        use std::sync::mpsc;
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("run/hooks.sock");
        let (tx, rx) = mpsc::channel();
        let server = HookServer::start(path.clone(), move |m| {
            let _ = tx.send(m);
        })?;
        let mut stream = std::os::unix::net::UnixStream::connect(server.path())?;
        let msg = HookMessage {
            event: "Stop".into(),
            session_id: "abc".into(),
            ..HookMessage::default()
        };
        writeln!(stream, "{}", serde_json::to_string(&msg)?)?;
        drop(stream);
        let got = rx.recv_timeout(std::time::Duration::from_secs(5))?;
        assert_eq!(got.session_id, "abc");
        drop(server);
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn context_use_prefers_the_status_line() {
        let line = StatusLineInfo {
            used_percentage: Some(42),
            context_tokens: Some(84_000),
            context_window_size: Some(200_000),
            ..Default::default()
        };
        let u = context_use(Some(&line), Some(10)).map(|u| (u.percent, u.estimated, u.tokens));
        assert_eq!(u, Some((42, false, Some(84_000))));
    }

    #[test]
    fn context_use_estimates_from_the_transcript() {
        assert_eq!(context_use(None, None), None);
        let u = context_use(None, Some(50_000)).map(|u| (u.percent, u.window, u.estimated));
        assert_eq!(u, Some((25, Some(200_000), true)));
        // Past 200k: it must be a 1M window.
        let u = context_use(None, Some(300_000)).map(|u| (u.percent, u.window));
        assert_eq!(u, Some((30, Some(1_000_000))));
        // A status line without a percentage still gives the window.
        let line = StatusLineInfo {
            context_window_size: Some(100_000),
            ..Default::default()
        };
        let u = context_use(Some(&line), Some(150_000)).map(|u| u.percent);
        assert_eq!(u, Some(100));
    }
}
