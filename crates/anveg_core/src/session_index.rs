//! A project's session list (build plan, section 6.2): every transcript in
//! the project's main-folder directory (and, in Phase 3, its worktrees'
//! directories), the app's aliases, and a per-file parse cache stored in
//! `sessions/<project-id>.json`.
//!
//! All methods do blocking file IO; the app calls them on a background
//! thread.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::atomic;
use crate::transcripts::{self, CachedTranscript, TranscriptMeta};

/// Shown until a session has a name or a first prompt.
pub const UNTITLED: &str = "New session";

/// `sessions/<project-id>.json`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionsFile {
    /// Session id → name the user gave it in the app.
    pub aliases: BTreeMap<String, String>,
    /// Transcript path → parse state.
    pub cache: BTreeMap<PathBuf, CachedTranscript>,
}

/// One row of the session list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInfo {
    pub id: String,
    pub display_name: String,
    pub first_prompt: Option<String>,
    /// Unix ms.
    pub last_activity: i64,
    /// Unix ms.
    pub created: Option<i64>,
    pub git_branch: Option<String>,
    pub non_interactive: bool,
    pub transcript: PathBuf,
    /// The worktree name, for sessions in `<main>/.claude/worktrees/<name>`.
    pub worktree: Option<String>,
    /// From the latest reply (see `TranscriptMeta::context_tokens`).
    pub context_tokens: Option<u64>,
    pub model: Option<String>,
}

impl SessionInfo {
    /// Whether `query` (already lowercase) matches the name or first prompt.
    pub fn matches(&self, query_lower: &str) -> bool {
        query_lower.is_empty()
            || self.display_name.to_lowercase().contains(query_lower)
            || self
                .first_prompt
                .as_deref()
                .is_some_and(|p| p.to_lowercase().contains(query_lower))
    }
}

/// Display-name precedence: app alias → custom name → generated title →
/// first prompt (truncated) → "Untitled session".
pub fn display_name(alias: Option<&str>, meta: &TranscriptMeta) -> String {
    alias
        .map(str::to_owned)
        .or_else(|| meta.title())
        .unwrap_or_else(|| UNTITLED.to_owned())
}

/// A transcript directory and the worktree it belongs to (`None` for the
/// main folder).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionDir {
    pub worktree: Option<String>,
    pub path: PathBuf,
}

/// The session list of one project.
#[derive(Debug)]
pub struct SessionIndex {
    config_dir: PathBuf,
    main_folder: PathBuf,
    file_path: PathBuf,
    data: SessionsFile,
    dirty: bool,
}

impl SessionIndex {
    /// Load the cache for a project. `file_path` is
    /// `sessions/<project-id>.json`.
    pub fn load(config_dir: PathBuf, main_folder: PathBuf, file_path: PathBuf) -> Self {
        let data: SessionsFile = atomic::read_json_or_default(&file_path);
        Self {
            config_dir,
            main_folder,
            file_path,
            data,
            dirty: false,
        }
    }

    pub fn main_folder(&self) -> &Path {
        &self.main_folder
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    /// Point the index at another Claude Code config directory (detected
    /// after launch). Clears the cache when it changes.
    pub fn set_config_dir(&mut self, config_dir: PathBuf) {
        if config_dir != self.config_dir {
            self.config_dir = config_dir;
            self.data.cache.clear();
            self.dirty = true;
        }
    }

    /// The worktree folders that exist under `<main>/.claude/worktrees`.
    pub fn worktrees(&self) -> Vec<(String, PathBuf)> {
        let root = self.main_folder.join(".claude").join("worktrees");
        let Ok(entries) = std::fs::read_dir(&root) else {
            return Vec::new();
        };
        let mut list: Vec<(String, PathBuf)> = entries
            .filter_map(|e| e.ok())
            .filter(|e| e.path().is_dir())
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect();
        list.sort();
        list
    }

    /// The transcript directories to read: the main folder's, then one per
    /// worktree. Directories that do not exist yet are included when their
    /// path is known, so the watcher can wait for them.
    pub fn session_dirs(&self) -> Vec<SessionDir> {
        let mut dirs = Vec::new();
        if let Some(path) = transcripts::transcript_dir(&self.config_dir, &self.main_folder) {
            dirs.push(SessionDir {
                worktree: None,
                path,
            });
        }
        for (name, folder) in self.worktrees() {
            if let Some(path) = transcripts::transcript_dir(&self.config_dir, &folder) {
                dirs.push(SessionDir {
                    worktree: Some(name),
                    path,
                });
            }
        }
        dirs
    }

    /// Directories a watcher should observe: the transcript directories
    /// that exist, `<config>/projects` (to see new ones appear) and the
    /// worktrees folder.
    pub fn watch_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = self
            .session_dirs()
            .into_iter()
            .map(|d| d.path)
            .filter(|p| p.is_dir())
            .collect();
        dirs.push(transcripts::projects_root(&self.config_dir));
        let worktrees = self.main_folder.join(".claude").join("worktrees");
        if worktrees.is_dir() {
            dirs.push(worktrees);
        }
        dirs
    }

    /// Re-scan the directories and parse what changed. Returns whether the
    /// list may have changed.
    pub fn refresh(&mut self) -> bool {
        let mut seen = BTreeSet::new();
        let mut changed = false;
        for dir in self.session_dirs() {
            let Ok(entries) = std::fs::read_dir(&dir.path) else {
                continue;
            };
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if transcripts::session_id_of(&path).is_none() || !path.is_file() {
                    continue;
                }
                let cache = self.data.cache.entry(path.clone()).or_default();
                match transcripts::update_cached(&path, cache) {
                    Ok(true) => changed = true,
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(path = %path.display(), %error, "could not read transcript")
                    }
                }
                seen.insert(path);
            }
        }
        let before = self.data.cache.len();
        self.data.cache.retain(|path, _| seen.contains(path));
        if self.data.cache.len() != before {
            changed = true;
        }
        if changed {
            self.dirty = true;
        }
        changed
    }

    /// The sessions, most recent activity first.
    pub fn sessions(&self, show_non_interactive: bool) -> Vec<SessionInfo> {
        let dirs = self.session_dirs();
        let worktree_of = |path: &Path| -> Option<String> {
            let parent = path.parent()?;
            dirs.iter()
                .find(|d| d.path == parent)
                .and_then(|d| d.worktree.clone())
        };
        let mut list: Vec<SessionInfo> = self
            .data
            .cache
            .iter()
            .filter_map(|(path, cached)| {
                let id = transcripts::session_id_of(path)?;
                let meta = &cached.meta;
                let non_interactive = meta.non_interactive.unwrap_or(false);
                if non_interactive && !show_non_interactive {
                    return None;
                }
                Some(SessionInfo {
                    display_name: display_name(
                        self.data.aliases.get(&id).map(String::as_str),
                        meta,
                    ),
                    first_prompt: meta.first_prompt.clone(),
                    last_activity: meta.last_activity.unwrap_or(cached.mtime),
                    created: meta.first_activity,
                    git_branch: meta.git_branch.clone(),
                    context_tokens: meta.context_tokens,
                    model: meta.model.clone(),
                    non_interactive,
                    transcript: path.clone(),
                    worktree: worktree_of(path),
                    id,
                })
            })
            .collect();
        list.sort_by(|a, b| {
            b.last_activity
                .cmp(&a.last_activity)
                .then_with(|| a.id.cmp(&b.id))
        });
        list
    }

    /// The transcript for a session, if it exists.
    pub fn transcript_path(&self, session_id: &str) -> Option<PathBuf> {
        self.data
            .cache
            .keys()
            .find(|p| transcripts::session_id_of(p).as_deref() == Some(session_id))
            .cloned()
    }

    /// Where a session's transcript would be in the main folder's directory.
    pub fn expected_transcript_path(&self, session_id: &str) -> Option<PathBuf> {
        transcripts::transcript_dir(&self.config_dir, &self.main_folder)
            .map(|dir| dir.join(format!("{session_id}.jsonl")))
    }

    pub fn alias(&self, session_id: &str) -> Option<&str> {
        self.data.aliases.get(session_id).map(String::as_str)
    }

    pub fn set_alias(&mut self, session_id: &str, name: &str) {
        self.data
            .aliases
            .insert(session_id.to_owned(), name.to_owned());
        self.dirty = true;
    }

    /// Forget everything about a session (after it was deleted).
    pub fn forget(&mut self, session_id: &str) {
        self.data.aliases.remove(session_id);
        self.data
            .cache
            .retain(|p, _| transcripts::session_id_of(p).as_deref() != Some(session_id));
        self.dirty = true;
    }

    /// Write `sessions/<project-id>.json` if anything changed.
    pub fn save_if_dirty(&mut self) -> Result<()> {
        if self.dirty {
            atomic::write_json(&self.file_path, &self.data)?;
            self.dirty = false;
        }
        Ok(())
    }
}

/// Move a session's files to the Trash: `<id>.jsonl` and the sibling
/// `<id>/` directory when there is one. A file that is already gone is not
/// an error.
pub fn trash_session_files(transcript: &Path) -> Result<()> {
    let mut targets = Vec::new();
    if transcript.exists() {
        targets.push(transcript.to_path_buf());
    }
    let sibling = transcript.with_extension("");
    if sibling.is_dir() {
        targets.push(sibling);
    }
    if !targets.is_empty() {
        trash::delete_all(&targets)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_session(dir: &Path, id: &str, lines: &[&str]) -> Result<PathBuf> {
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("{id}.jsonl"));
        std::fs::write(&path, lines.join("\n") + "\n")?;
        Ok(path)
    }

    fn setup() -> Result<(tempfile::TempDir, SessionIndex, PathBuf)> {
        let tmp = tempfile::tempdir()?;
        let cfg = tmp.path().join("cfg");
        let main = tmp.path().join("proj");
        std::fs::create_dir_all(&main)?;
        let index = SessionIndex::load(
            cfg.clone(),
            main.clone(),
            tmp.path().join("sessions/p.json"),
        );
        let dir = transcripts::transcript_dir(&cfg, &main).unwrap_or_default();
        Ok((tmp, index, dir))
    }

    #[test]
    fn precedence() {
        let mut meta = TranscriptMeta {
            first_prompt: Some("prompt".into()),
            ..TranscriptMeta::default()
        };
        assert_eq!(display_name(None, &meta), "prompt");
        meta.generated_title = Some("generated".into());
        assert_eq!(display_name(None, &meta), "generated");
        meta.custom_title = Some("custom".into());
        assert_eq!(display_name(None, &meta), "custom");
        assert_eq!(display_name(Some("alias"), &meta), "alias");
        assert_eq!(display_name(None, &TranscriptMeta::default()), UNTITLED);
    }

    #[test]
    fn lists_sorts_and_filters() -> Result<()> {
        let (_tmp, mut index, dir) = setup()?;
        write_session(
            &dir,
            "old",
            &[
                r#"{"type":"user","message":{"content":"Fix the login bug"},"timestamp":"2026-01-01T00:00:00Z","entrypoint":"cli"}"#,
            ],
        )?;
        write_session(
            &dir,
            "new",
            &[
                r#"{"type":"custom-title","customTitle":"Refactor"}"#,
                r#"{"type":"user","message":{"content":"Refactor the parser"},"timestamp":"2026-02-01T00:00:00Z","entrypoint":"cli"}"#,
            ],
        )?;
        write_session(
            &dir,
            "sdk",
            &[
                r#"{"type":"user","message":{"content":"automated"},"timestamp":"2026-03-01T00:00:00Z","entrypoint":"sdk-cli"}"#,
            ],
        )?;
        assert!(index.refresh());
        let list = index.sessions(false);
        let ids: Vec<_> = list.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["new", "old"]);
        assert_eq!(list[0].display_name, "Refactor");
        assert_eq!(index.sessions(true).len(), 3);
        assert!(list[1].matches("login"));
        assert!(list[0].matches("parser")); // first prompt matches too
        assert!(!list[0].matches("login"));
        Ok(())
    }

    #[test]
    fn aliases_win_and_persist() -> Result<()> {
        let (tmp, mut index, dir) = setup()?;
        write_session(
            &dir,
            "s1",
            &[r#"{"type":"custom-title","customTitle":"Theirs"}"#],
        )?;
        index.refresh();
        index.set_alias("s1", "Mine");
        assert_eq!(index.sessions(false)[0].display_name, "Mine");
        index.save_if_dirty()?;
        let reloaded = SessionIndex::load(
            index.config_dir().to_path_buf(),
            index.main_folder().to_path_buf(),
            tmp.path().join("sessions/p.json"),
        );
        assert_eq!(reloaded.alias("s1"), Some("Mine"));
        // The cache came back too, so the first listing needs no parse.
        assert_eq!(reloaded.sessions(false).len(), 1);
        Ok(())
    }

    #[test]
    fn deleted_files_drop_out_and_forget_clears_alias() -> Result<()> {
        let (_tmp, mut index, dir) = setup()?;
        let path = write_session(&dir, "gone", &[r#"{"type":"mode"}"#])?;
        index.refresh();
        index.set_alias("gone", "x");
        assert_eq!(index.transcript_path("gone"), Some(path.clone()));
        std::fs::remove_file(&path)?;
        assert!(index.refresh());
        assert!(index.sessions(true).is_empty());
        index.forget("gone");
        assert_eq!(index.alias("gone"), None);
        Ok(())
    }

    #[test]
    fn worktree_sessions_are_grouped() -> Result<()> {
        let (_tmp, mut index, dir) = setup()?;
        write_session(&dir, "main-s", &[r#"{"type":"mode"}"#])?;
        let wt_folder = index.main_folder().join(".claude/worktrees/feature");
        std::fs::create_dir_all(&wt_folder)?;
        let wt_dir =
            transcripts::transcript_dir(index.config_dir(), &wt_folder).unwrap_or_default();
        write_session(&wt_dir, "wt-s", &[r#"{"type":"mode"}"#])?;
        index.refresh();
        let list = index.sessions(true);
        let wt = list
            .iter()
            .find(|s| s.id == "wt-s")
            .map(|s| s.worktree.clone());
        assert_eq!(wt, Some(Some("feature".to_owned())));
        let main = list
            .iter()
            .find(|s| s.id == "main-s")
            .map(|s| s.worktree.clone());
        assert_eq!(main, Some(None));
        assert!(
            index
                .watch_dirs()
                .iter()
                .any(|d| d.ends_with(".claude/worktrees"))
        );
        Ok(())
    }

    #[test]
    fn expected_path_for_new_sessions() -> Result<()> {
        let (_tmp, index, dir) = setup()?;
        assert_eq!(
            index.expected_transcript_path("abc"),
            Some(dir.join("abc.jsonl"))
        );
        Ok(())
    }
}
