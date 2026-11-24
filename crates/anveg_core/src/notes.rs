//! Notes: plain-text notes per project, in tabs (the Notes panel).
//!
//! Stored in `<support dir>/notes/<project-id>.json`, never in the project
//! folder. There are always two built-in tabs: **Notes** (shown by default)
//! and **Archive** (where a note goes once it has been sent to a session).
//! The user can add more tabs (for example "Milestone 3"), rename and
//! delete them.

use std::path::Path;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::atomic;
use crate::time::now_millis;

/// What kind of tab.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TabKind {
    #[default]
    Notes,
    Archive,
    Custom,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NoteTab {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub kind: TabKind,
}

/// How a note was sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendKind {
    /// Pasted into a running session, then Enter.
    Paste,
    /// Started a new session with the note as its first message.
    NewSession,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sent {
    /// Unix milliseconds.
    pub at: i64,
    pub session_id: String,
    /// The session's name at the time.
    #[serde(default)]
    pub session_name: String,
    pub how: SendKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: String,
    /// The tab it is in.
    pub tab: String,
    #[serde(default)]
    pub text: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// The tab it was in before it was archived (for Restore).
    #[serde(default)]
    pub archived_from: Option<String>,
    /// Every time it was sent, oldest first.
    #[serde(default)]
    pub sent: Vec<Sent>,
}

impl Note {
    pub fn last_sent(&self) -> Option<&Sent> {
        self.sent.last()
    }
}

/// One project's notes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NotesFile {
    pub tabs: Vec<NoteTab>,
    pub notes: Vec<Note>,
}

fn new_id() -> String {
    Uuid::new_v4().to_string()
}

impl NotesFile {
    /// Load `path` (missing or unreadable gives an empty file), with the
    /// built-in tabs present.
    pub fn load(path: &Path) -> Self {
        let mut file: Self = atomic::read_json_or_default(path);
        file.repair();
        file
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        atomic::write_json(path, self)
    }

    /// Make sure Notes comes first and Archive exists, and that every note
    /// is in a tab that exists.
    fn repair(&mut self) {
        if !self.tabs.iter().any(|t| t.kind == TabKind::Notes) {
            self.tabs.insert(
                0,
                NoteTab {
                    id: new_id(),
                    name: "Notes".into(),
                    kind: TabKind::Notes,
                },
            );
        }
        if !self.tabs.iter().any(|t| t.kind == TabKind::Archive) {
            self.tabs.push(NoteTab {
                id: new_id(),
                name: "Archive".into(),
                kind: TabKind::Archive,
            });
        }
        // Every tab ends with an empty note in the panel (not stored), so
        // a note left empty is just clutter.
        self.notes.retain(|n| !n.text.trim().is_empty());
        let notes_tab = self.notes_tab().to_owned();
        let ids: Vec<String> = self.tabs.iter().map(|t| t.id.clone()).collect();
        for note in &mut self.notes {
            if !ids.contains(&note.tab) {
                note.tab = notes_tab.clone();
            }
        }
    }

    fn tab_of_kind(&self, kind: TabKind) -> &str {
        self.tabs
            .iter()
            .find(|t| t.kind == kind)
            .map(|t| t.id.as_str())
            .unwrap_or("")
    }

    /// The default tab's id.
    pub fn notes_tab(&self) -> &str {
        self.tab_of_kind(TabKind::Notes)
    }

    pub fn archive_tab(&self) -> &str {
        self.tab_of_kind(TabKind::Archive)
    }

    pub fn tab(&self, id: &str) -> Option<&NoteTab> {
        self.tabs.iter().find(|t| t.id == id)
    }

    pub fn note(&self, id: &str) -> Option<&Note> {
        self.notes.iter().find(|n| n.id == id)
    }

    fn note_mut(&mut self, id: &str) -> Option<&mut Note> {
        self.notes.iter_mut().find(|n| n.id == id)
    }

    /// The notes of a tab, in order. The archive shows the most recently
    /// sent first; other tabs keep the order notes were added in.
    pub fn notes_in(&self, tab: &str) -> Vec<&Note> {
        let mut notes: Vec<&Note> = self.notes.iter().filter(|n| n.tab == tab).collect();
        if tab == self.archive_tab() {
            notes.sort_by_key(|n| std::cmp::Reverse(n.last_sent().map_or(n.updated_at, |s| s.at)));
        }
        notes
    }

    pub fn count_in(&self, tab: &str) -> usize {
        self.notes.iter().filter(|n| n.tab == tab).count()
    }

    /// A new, empty note at the end of `tab` (the Archive gets it in
    /// Notes instead). Returns its id.
    pub fn add_note(&mut self, tab: &str) -> String {
        let tab = if tab == self.archive_tab() || self.tab(tab).is_none() {
            self.notes_tab().to_owned()
        } else {
            tab.to_owned()
        };
        let now = now_millis();
        let id = new_id();
        self.notes.push(Note {
            id: id.clone(),
            tab,
            text: String::new(),
            created_at: now,
            updated_at: now,
            archived_from: None,
            sent: Vec::new(),
        });
        id
    }

    /// Returns whether anything changed.
    pub fn set_text(&mut self, id: &str, text: &str) -> bool {
        match self.note_mut(id) {
            Some(n) if n.text != text => {
                n.text = text.to_owned();
                n.updated_at = now_millis();
                true
            }
            _ => false,
        }
    }

    pub fn delete_note(&mut self, id: &str) {
        self.notes.retain(|n| n.id != id);
    }

    pub fn move_note(&mut self, id: &str, tab: &str) {
        if self.tab(tab).is_none() {
            return;
        }
        let archive = self.archive_tab().to_owned();
        if let Some(n) = self.note_mut(id) {
            if tab == archive && n.tab != archive {
                n.archived_from = Some(n.tab.clone());
            }
            n.tab = tab.to_owned();
            n.updated_at = now_millis();
        }
    }

    /// The note was sent: record it and move it to the Archive.
    pub fn mark_sent(&mut self, id: &str, session_id: &str, session_name: &str, how: SendKind) {
        let archive = self.archive_tab().to_owned();
        if let Some(n) = self.note_mut(id) {
            n.sent.push(Sent {
                at: now_millis(),
                session_id: session_id.to_owned(),
                session_name: session_name.to_owned(),
                how,
            });
        }
        self.move_note(id, &archive);
    }

    /// Back from the Archive to the tab it came from (or Notes).
    pub fn restore(&mut self, id: &str) {
        let fallback = self.notes_tab().to_owned();
        let Some(from) = self.note(id).map(|n| n.archived_from.clone()) else {
            return;
        };
        let target = from
            .filter(|t| self.tab(t).is_some_and(|t| t.kind != TabKind::Archive))
            .unwrap_or(fallback);
        if let Some(n) = self.note_mut(id) {
            n.tab = target;
            n.archived_from = None;
            n.updated_at = now_millis();
        }
    }

    /// A new tab before the Archive. Returns its id.
    pub fn add_tab(&mut self, name: &str) -> String {
        let id = new_id();
        let at = self
            .tabs
            .iter()
            .position(|t| t.kind == TabKind::Archive)
            .unwrap_or(self.tabs.len());
        self.tabs.insert(
            at,
            NoteTab {
                id: id.clone(),
                name: name.trim().to_owned(),
                kind: TabKind::Custom,
            },
        );
        id
    }

    /// Only custom tabs can be renamed or deleted.
    pub fn rename_tab(&mut self, id: &str, name: &str) {
        if let Some(t) = self
            .tabs
            .iter_mut()
            .find(|t| t.id == id && t.kind == TabKind::Custom)
        {
            t.name = name.trim().to_owned();
        }
    }

    /// Delete a custom tab; its notes move to Notes.
    pub fn delete_tab(&mut self, id: &str) {
        if self.tab(id).is_none_or(|t| t.kind != TabKind::Custom) {
            return;
        }
        let notes_tab = self.notes_tab().to_owned();
        for n in &mut self.notes {
            if n.tab == id {
                n.tab = notes_tab.clone();
            }
            if n.archived_from.as_deref() == Some(id) {
                n.archived_from = None;
            }
        }
        self.tabs.retain(|t| t.id != id);
    }
}

/// Check a tab name.
pub fn validate_tab_name(name: &str) -> std::result::Result<&str, &'static str> {
    let name = name.trim();
    if name.is_empty() {
        Err("Enter a name.")
    } else if name.chars().count() > 60 {
        Err("Keep the name under 60 characters.")
    } else {
        Ok(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_tabs_and_round_trip() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("notes").join("p.json");
        let mut f = NotesFile::load(&path);
        assert_eq!(f.tabs.len(), 2);
        assert_eq!(f.tabs[0].kind, TabKind::Notes);
        assert_eq!(f.tabs[1].kind, TabKind::Archive);
        let tab = f.notes_tab().to_owned();
        let id = f.add_note(&tab);
        assert!(f.set_text(&id, "first\nsecond line"));
        assert!(!f.set_text(&id, "first\nsecond line"));
        f.save(&path)?;
        assert_eq!(NotesFile::load(&path), f);
        Ok(())
    }

    #[test]
    fn sending_archives_and_restore_goes_back() {
        let mut f = NotesFile::default();
        f.repair();
        let m3 = f.add_tab("Milestone 3");
        // Custom tabs sit before the Archive.
        assert_eq!(f.tabs[1].id, m3);
        assert_eq!(f.tabs[2].kind, TabKind::Archive);
        let a = f.add_note(&m3);
        let b = f.add_note(&m3);
        f.mark_sent(&a, "s1", "Fix the login", SendKind::Paste);
        let archive = f.archive_tab().to_owned();
        assert_eq!(f.note(&a).map(|n| n.tab.clone()), Some(archive.clone()));
        assert_eq!(f.count_in(&m3), 1);
        f.mark_sent(&b, "s2", "x", SendKind::NewSession);
        // Most recently sent first (same millisecond: either order is fine,
        // so send `a` again to be sure).
        std::thread::sleep(std::time::Duration::from_millis(2));
        f.mark_sent(&a, "s3", "again", SendKind::Paste);
        let order: Vec<&str> = f.notes_in(&archive).iter().map(|n| n.id.as_str()).collect();
        assert_eq!(order, [a.as_str(), b.as_str()]);
        assert_eq!(f.note(&a).map(|n| n.sent.len()), Some(2));
        f.restore(&a);
        assert_eq!(f.note(&a).map(|n| n.tab.clone()), Some(m3.clone()));
        // Restoring after the tab is gone lands in Notes.
        f.delete_tab(&m3);
        f.restore(&b);
        assert_eq!(f.note(&b).map(|n| n.tab.clone()), Some(f.notes_tab().to_owned()));
        assert_eq!(f.note(&a).map(|n| n.tab.clone()), Some(f.notes_tab().to_owned()));
    }

    #[test]
    fn built_in_tabs_are_protected() {
        let mut f = NotesFile::default();
        f.repair();
        let notes = f.notes_tab().to_owned();
        let archive = f.archive_tab().to_owned();
        f.rename_tab(&notes, "X");
        f.delete_tab(&archive);
        assert_eq!(f.tabs.len(), 2);
        assert_eq!(f.tabs[0].name, "Notes");
        // New notes never start in the Archive.
        let id = f.add_note(&archive);
        assert_eq!(f.note(&id).map(|n| n.tab.clone()), Some(notes));
    }

    #[test]
    fn notes_in_missing_tabs_are_rescued() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let path = tmp.path().join("p.json");
        std::fs::write(
            &path,
            r#"{"notes":[{"id":"n","tab":"gone","text":"hi","created_at":1,"updated_at":1},
                         {"id":"e","tab":"gone","text":"  ","created_at":1,"updated_at":1}]}"#,
        )?;
        let f = NotesFile::load(&path);
        // Empty notes are dropped.
        assert!(f.note("e").is_none());
        assert_eq!(f.note("n").map(|n| n.tab.clone()), Some(f.notes_tab().to_owned()));
        Ok(())
    }

    #[test]
    fn tab_names() {
        assert!(validate_tab_name(" Milestone 3 ").is_ok());
        assert!(validate_tab_name("  ").is_err());
    }
}
