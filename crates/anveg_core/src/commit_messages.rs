//! Commit messages the Git tab remembers, per repository: the message
//! being written (kept until it is committed, across restarts and repository
//! switches), and the messages committed from the app, newest first (the
//! Recent menu).
//!
//! Stored in `<support dir>/commit-messages.json`, never in the repository.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::atomic;

/// How many committed messages are kept per repository.
pub const MAX_RECENT: usize = 20;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoMessages {
    /// The unfinished message ("" when there is none).
    #[serde(default)]
    pub draft: String,
    /// Committed messages, newest first.
    #[serde(default)]
    pub recent: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitMessages {
    /// By repository root.
    #[serde(default)]
    repos: BTreeMap<String, RepoMessages>,
}

fn key(repo: &Path) -> String {
    repo.to_string_lossy().into_owned()
}

impl CommitMessages {
    /// The saved messages; empty when the file is missing or unreadable.
    pub fn load(path: &Path) -> Self {
        atomic::read_json_or_default(path)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        atomic::write_json(path, self)
    }

    pub fn draft(&self, repo: &Path) -> &str {
        self.repos.get(&key(repo)).map_or("", |r| r.draft.as_str())
    }

    /// Keep `text` as the repository's unfinished message. Returns whether
    /// anything changed.
    pub fn set_draft(&mut self, repo: &Path, text: &str) -> bool {
        let text = if text.trim().is_empty() { "" } else { text };
        let current = self.draft(repo);
        if current == text {
            return false;
        }
        self.repos.entry(key(repo)).or_default().draft = text.to_owned();
        self.prune(repo);
        true
    }

    pub fn recent(&self, repo: &Path) -> &[String] {
        self.repos.get(&key(repo)).map_or(&[], |r| r.recent.as_slice())
    }

    /// `message` was committed: first in Recent (once), and the draft is
    /// cleared.
    pub fn committed(&mut self, repo: &Path, message: &str) {
        let message = message.trim();
        let entry = self.repos.entry(key(repo)).or_default();
        entry.draft.clear();
        if !message.is_empty() {
            entry.recent.retain(|m| m != message);
            entry.recent.insert(0, message.to_owned());
            entry.recent.truncate(MAX_RECENT);
        }
    }

    /// Drop a repository's entry once it holds nothing.
    fn prune(&mut self, repo: &Path) {
        if self.repos.get(&key(repo)).is_some_and(|r| r.draft.is_empty() && r.recent.is_empty()) {
            self.repos.remove(&key(repo));
        }
    }
}

/// A message's first line (its summary), trimmed.
pub fn summary_line(message: &str) -> &str {
    message.lines().next().unwrap_or("").trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drafts_and_recent_messages() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("commit-messages.json");
        let (a, b) = (Path::new("/r/a"), Path::new("/r/b"));
        let mut m = CommitMessages::load(&file);
        assert_eq!(m.draft(a), "");
        assert!(m.set_draft(a, "Fix it\n\nBecause"));
        assert!(!m.set_draft(a, "Fix it\n\nBecause"));
        assert!(m.set_draft(b, "Other"));
        m.save(&file)?;
        let mut m = CommitMessages::load(&file);
        assert_eq!(m.draft(a), "Fix it\n\nBecause");
        assert_eq!(m.draft(b), "Other");
        m.committed(a, "Fix it\n\nBecause\n");
        m.committed(a, "Second");
        m.committed(a, "Fix it\n\nBecause");
        assert_eq!(m.draft(a), "");
        assert_eq!(m.recent(a), ["Fix it\n\nBecause".to_owned(), "Second".to_owned()]);
        assert!(m.recent(b).is_empty());
        for i in 0..30 {
            m.committed(a, &format!("m{i}"));
        }
        assert_eq!(m.recent(a).len(), MAX_RECENT);
        assert_eq!(m.recent(a)[0], "m29");
        // Whitespace only is no draft; an empty entry goes away.
        assert!(m.set_draft(b, "  \n"));
        assert_eq!(m.draft(b), "");
        assert!(!m.repos.contains_key("/r/b"));
        assert_eq!(summary_line("  Title  \n\nBody"), "Title");
        assert_eq!(summary_line(""), "");
        Ok(())
    }
}
