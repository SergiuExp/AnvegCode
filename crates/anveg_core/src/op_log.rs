//! Operation logs: what the app did, for when something goes wrong.
//! Operations (▶ start, ✓ done, ✗ failed, with how long they took) and the
//! details inside them.
//!
//! - The GitHub log: GitHub operations, their HTTP requests (method, path,
//!   status, time) and the git commands of Publish, Open from GitHub and
//!   pull request checkout.
//! - The Git log: every git command the app runs that changes a repository
//!   or talks to a remote (commit, stage, push, pull, fetch, clone, …),
//!   with a summary of what git printed (`process::run_with`).
//!
//! Kept in memory only (the newest [`MAX_LINES`]), never on disk. Tokens and
//! request bodies are never written; passwords in URLs are hidden.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};

pub const MAX_LINES: usize = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// An operation started.
    Start,
    Ok,
    Failed,
    /// A detail inside an operation (an HTTP request, a git command).
    Detail,
    Info,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    /// Local time, `HH:MM:SS`.
    pub time: String,
    pub level: Level,
    pub text: String,
}

impl Line {
    pub fn render(&self) -> String {
        let mark = match self.level {
            Level::Start => "▶",
            Level::Ok => "✓",
            Level::Failed => "✗",
            Level::Detail => " ",
            Level::Info => "•",
        };
        let indent = if self.level == Level::Detail { "    " } else { "" };
        // Multi-line messages (git's output) stay under their line.
        let text = self.text.replace('\n', "\n           ");
        format!("{} {mark} {indent}{text}", self.time)
    }
}

#[derive(Default)]
struct Inner {
    lines: Vec<Line>,
    /// Lines dropped from the front so far (so watchers can tell).
    dropped: usize,
    watchers: Vec<UnboundedSender<()>>,
}

/// Shared, cheap to clone; usable from any thread.
#[derive(Clone, Default)]
pub struct OpLog(Arc<Mutex<Inner>>);

impl OpLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&self, level: Level, text: impl Into<String>) {
        let line = Line {
            time: crate::time::local_hms(),
            level,
            text: text.into(),
        };
        if let Ok(mut inner) = self.0.lock() {
            inner.lines.push(line);
            let extra = inner.lines.len().saturating_sub(MAX_LINES);
            if extra > 0 {
                inner.lines.drain(..extra);
                inner.dropped += extra;
            }
            inner.watchers.retain(|w| w.unbounded_send(()).is_ok());
        }
    }

    pub fn info(&self, text: impl Into<String>) {
        self.push(Level::Info, text);
    }

    pub fn detail(&self, text: impl Into<String>) {
        self.push(Level::Detail, text);
    }

    /// Run `f` as one operation: "▶ label", then "✓ label (12 ms)" or
    /// "✗ label: why (12 ms)".
    pub fn op<T>(&self, label: &str, f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
        self.push(Level::Start, label);
        let started = Instant::now();
        let result = f();
        let ms = started.elapsed().as_millis();
        match &result {
            Ok(_) => self.push(Level::Ok, format!("{label} ({ms} ms)")),
            Err(e) => self.push(Level::Failed, format!("{label}: {e:#} ({ms} ms)")),
        }
        result
    }

    pub fn lines(&self) -> Vec<Line> {
        self.0.lock().map(|i| i.lines.clone()).unwrap_or_default()
    }

    /// The newest line.
    pub fn last(&self) -> Option<Line> {
        self.0.lock().ok().and_then(|i| i.lines.last().cloned())
    }

    /// All lines as text (what the log window shows and copies).
    pub fn text(&self) -> String {
        self.lines()
            .iter()
            .map(Line::render)
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn len(&self) -> usize {
        self.0.lock().map(|i| i.lines.len()).unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn clear(&self) {
        if let Ok(mut inner) = self.0.lock() {
            inner.lines.clear();
            inner.watchers.retain(|w| w.unbounded_send(()).is_ok());
        }
    }

    /// A stream that yields after every change.
    pub fn watch(&self) -> UnboundedReceiver<()> {
        let (tx, rx) = unbounded();
        if let Ok(mut inner) = self.0.lock() {
            inner.watchers.push(tx);
        }
        rx
    }
}

impl std::fmt::Debug for OpLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "OpLog({} lines)", self.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operations_and_details() {
        let log = OpLog::new();
        let mut rx = log.watch();
        let r: anyhow::Result<u32> = log.op("Listing repositories", || {
            log.detail("GET /user/repos → 200 (12 ms)");
            Ok(3)
        });
        assert_eq!(r.ok(), Some(3));
        let _ = log.op::<()>("Creating x", || Err(anyhow::anyhow!("name already exists")));
        let lines = log.lines();
        let levels: Vec<Level> = lines.iter().map(|l| l.level).collect();
        assert_eq!(levels, [Level::Start, Level::Detail, Level::Ok, Level::Start, Level::Failed]);
        assert!(lines[2].text.starts_with("Listing repositories ("));
        assert!(lines[4].text.contains("name already exists"));
        let text = log.text();
        assert!(text.contains("✓ Listing repositories"));
        assert!(text.contains("✗ Creating x: name already exists"));
        assert!(text.contains("       GET /user/repos"), "{text}");
        // Watchers hear about every line.
        let mut n = 0;
        while rx.try_recv().is_ok() {
            n += 1;
        }
        assert_eq!(n, 5);
        log.clear();
        assert!(log.is_empty());
    }

    #[test]
    fn keeps_the_newest_lines() {
        let log = OpLog::new();
        for i in 0..MAX_LINES + 10 {
            log.info(i.to_string());
        }
        let lines = log.lines();
        assert_eq!(lines.len(), MAX_LINES);
        assert_eq!(lines[0].text, "10");
    }
}
