//! The app's own files: `settings.json`, `projects.json` and `state.json`
//! (build plan, sections 6.1, 6.8 and 11). Every write is atomic.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::atomic;
use crate::git::{DiffKind, PullMode};
use crate::time::now_millis;

// ---------------------------------------------------------------------------
// settings.json
// ---------------------------------------------------------------------------

/// Terminal preferences (Settings ▸ Terminal).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TerminalPrefs {
    pub font_family: String,
    pub font_size: f32,
    pub scrollback_lines: usize,
    pub cursor_blink: bool,
    pub option_as_meta: bool,
}

impl Default for TerminalPrefs {
    fn default() -> Self {
        Self {
            font_family: "Menlo".to_owned(),
            font_size: 13.0,
            scrollback_lines: 10_000,
            cursor_blink: false,
            option_as_meta: true,
        }
    }
}

/// Everything in Settings (⌘,).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Ask before stopping running sessions on close or quit.
    pub confirm_on_close: bool,
    /// Resume the sessions that were running when the app quit.
    pub resume_on_launch: bool,
    /// Path to `claude`; `None` means auto-detect on `PATH`.
    pub claude_path: Option<PathBuf>,
    /// Extra flags for every `claude` launch, split with shell rules.
    pub default_flags: String,
    /// Show sessions started with `claude -p` or the Agent SDK.
    pub show_noninteractive: bool,
    pub terminal: TerminalPrefs,
    /// Phase 2: notify when a hidden session needs input or finishes.
    pub notifications: bool,
    /// Phase 2: stop idle, hidden sessions after this many minutes (0 = never).
    pub idle_stop_minutes: u32,
    /// Phase 2: show each running session's memory in its tooltip.
    pub show_memory: bool,
    /// Phase 4: the editor.
    pub editor: EditorPrefs,
    /// Phase 4: language server commands, by language name
    /// (`rust`, `typescript`, …). An empty command turns LSP off for it.
    pub lsp_servers: BTreeMap<String, String>,
    /// Phase 5: how Pull integrates (fast-forward only by default).
    pub pull_mode: PullMode,
    /// Session toolbar: read model, effort and context from Claude Code's
    /// status line (the user's own status line still shows). Off: an
    /// estimate from the transcript.
    pub session_status_line: bool,
    /// Session toolbar: the user's own buttons.
    pub session_buttons: Vec<SessionButton>,
    /// A chime when Claude finishes a turn.
    pub finish_sound: FinishSound,
    /// Diffs side by side (before | after) rather than in one column.
    pub diff_split: bool,
    /// The GitHub OAuth App's client ID, for "Sign in with GitHub" (the
    /// device flow). Not a secret. Without one, sign in with a token.
    pub github_client_id: Option<String>,
}

/// When to play the chime as Claude finishes a turn.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishSound {
    Off,
    /// Only when that session is not in front of you (its window is not
    /// active, or it is not shown in a pane).
    WhenNotLooking,
    #[default]
    Always,
}

/// A button on the session toolbar that types text into Claude Code's
/// prompt and presses Enter (a slash command, a skill, a prompt).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionButton {
    pub id: String,
    pub label: String,
    pub text: String,
    /// Usable while Claude is working (otherwise only when it is idle).
    #[serde(default)]
    pub while_working: bool,
}

impl SessionButton {
    pub fn new(label: &str, text: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            label: label.to_owned(),
            text: text.to_owned(),
            while_working: false,
        }
    }
}

/// Editor preferences (Settings ▸ Editor).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EditorPrefs {
    pub soft_wrap: bool,
    pub format_on_save: bool,
    /// Save edited files by themselves: a moment after typing stops, when
    /// the editor or the window loses focus, on closing and on quitting.
    pub autosave: bool,
    /// Extra file types: extension (without the dot) or file name → the
    /// language to highlight it as. Added to the built-in list.
    pub file_types: BTreeMap<String, String>,
}

impl Default for EditorPrefs {
    fn default() -> Self {
        Self {
            soft_wrap: false,
            format_on_save: false,
            autosave: true,
            file_types: BTreeMap::new(),
        }
    }
}

/// The default language servers (next phases, 4.3).
pub fn default_lsp_servers() -> BTreeMap<String, String> {
    [
        ("rust", "rust-analyzer"),
        ("typescript", "typescript-language-server --stdio"),
        ("javascript", "typescript-language-server --stdio"),
        ("python", "pyright-langserver --stdio"),
        ("go", "gopls"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect()
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            confirm_on_close: true,
            resume_on_launch: false,
            claude_path: None,
            default_flags: String::new(),
            show_noninteractive: false,
            terminal: TerminalPrefs::default(),
            notifications: true,
            idle_stop_minutes: 30,
            show_memory: false,
            editor: EditorPrefs::default(),
            lsp_servers: default_lsp_servers(),
            pull_mode: PullMode::default(),
            session_status_line: true,
            session_buttons: Vec::new(),
            finish_sound: FinishSound::default(),
            diff_split: true,
            github_client_id: None,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let mut s: Self = atomic::read_json_or_default(path);
        // Settings files from before Phase 4 get the default servers.
        for (k, v) in default_lsp_servers() {
            s.lsp_servers.entry(k).or_insert(v);
        }
        s
    }

    /// The server command line for `language`, if one is set.
    pub fn lsp_command(&self, language: &str) -> Option<Vec<String>> {
        let line = self.lsp_servers.get(language)?;
        let args = shell_words::split(line).ok()?;
        (!args.is_empty()).then_some(args)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        atomic::write_json(path, self)
    }

    /// The default flags split into arguments. Invalid quoting gives an
    /// error message for the Settings window.
    pub fn default_flag_args(&self) -> std::result::Result<Vec<String>, String> {
        shell_words::split(&self.default_flags).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// projects.json
// ---------------------------------------------------------------------------

/// A project: one main folder plus optional extra folders.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    /// Canonicalized once, when the project is added. Every `claude` runs
    /// with exactly this working directory.
    pub main_folder: PathBuf,
    #[serde(default)]
    pub extra_folders: Vec<PathBuf>,
    /// Toolbar commands (build, run, test…), in toolbar order.
    #[serde(default)]
    pub commands: Vec<ProjectCommand>,
    /// Unix milliseconds.
    pub created_at: i64,
    /// Unix milliseconds.
    pub last_opened_at: i64,
}

impl Project {
    /// A new project for an already-canonical folder.
    pub fn new(main_folder: PathBuf) -> Self {
        let name = main_folder
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| main_folder.display().to_string());
        let now = now_millis();
        Self {
            id: Uuid::new_v4(),
            name,
            main_folder,
            extra_folders: Vec::new(),
            commands: Vec::new(),
            created_at: now,
            last_opened_at: now,
        }
    }

    /// Whether the main folder still exists.
    pub fn folder_exists(&self) -> bool {
        self.main_folder.is_dir()
    }
}

/// A toolbar command: a button (and optional shortcut) that runs a shell
/// command in a terminal tab, in the project's main folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCommand {
    pub id: String,
    pub name: String,
    /// Typed into an interactive login shell, as if by hand.
    pub command: String,
    /// A GPUI keystroke such as `cmd-shift-b`; empty for none.
    #[serde(default)]
    pub shortcut: String,
    #[serde(default = "yes")]
    pub show_in_toolbar: bool,
    /// Off: the command has its own terminal tab and runs there again
    /// (stopping a run that is still going). On: a new tab every time.
    #[serde(default)]
    pub new_terminal: bool,
}

fn yes() -> bool {
    true
}

impl ProjectCommand {
    pub fn new(name: &str, command: &str) -> Self {
        Self {
            id: Uuid::new_v4().to_string(),
            name: name.to_owned(),
            command: command.to_owned(),
            shortcut: String::new(),
            show_in_toolbar: true,
            new_terminal: false,
        }
    }
}

/// The list of known projects.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectStore {
    pub projects: Vec<Project>,
}

impl ProjectStore {
    pub fn load(path: &Path) -> Self {
        atomic::read_json_or_default(path)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        atomic::write_json(path, self)
    }

    pub fn get(&self, id: Uuid) -> Option<&Project> {
        self.projects.iter().find(|p| p.id == id)
    }

    pub fn get_mut(&mut self, id: Uuid) -> Option<&mut Project> {
        self.projects.iter_mut().find(|p| p.id == id)
    }

    pub fn find_by_folder(&self, canonical: &Path) -> Option<&Project> {
        self.projects.iter().find(|p| p.main_folder == canonical)
    }

    /// The project for `folder`, creating it if needed. `folder` is
    /// canonicalized first. Returns the project's id.
    pub fn open_folder(&mut self, folder: &Path) -> Result<Uuid> {
        let canonical = std::fs::canonicalize(folder)?;
        let id = match self.find_by_folder(&canonical) {
            Some(project) => project.id,
            None => {
                let project = Project::new(canonical);
                let id = project.id;
                self.projects.push(project);
                id
            }
        };
        self.touch(id);
        Ok(id)
    }

    /// Mark a project as just opened.
    pub fn touch(&mut self, id: Uuid) {
        if let Some(project) = self.get_mut(id) {
            project.last_opened_at = now_millis();
        }
    }

    /// Remove a project from the list (its files are untouched).
    pub fn remove(&mut self, id: Uuid) {
        self.projects.retain(|p| p.id != id);
    }

    /// Projects, most recently opened first.
    pub fn recent(&self) -> Vec<&Project> {
        let mut list: Vec<&Project> = self.projects.iter().collect();
        list.sort_by_key(|p| std::cmp::Reverse(p.last_opened_at));
        list
    }
}

// ---------------------------------------------------------------------------
// state.json
// ---------------------------------------------------------------------------

/// Which tab the right side shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RightTab {
    #[default]
    Claude,
    Terminal,
    /// One of the document tabs (`WindowState::docs[active_doc]`).
    Doc,
}

/// Which tab the sidebar shows (next phases, 0.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SidebarTab {
    #[default]
    Sessions,
    Files,
    Git,
    GitHub,
}

/// A document tab on the right side: what to load when it is first shown.
/// Restored lazily on launch.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DocKey {
    File {
        path: PathBuf,
    },
    GitDiff {
        repo: PathBuf,
        path: String,
        diff: DiffKind,
    },
    Commit {
        repo: PathBuf,
        sha: String,
        title: String,
    },
    PullRequest {
        repo: PathBuf,
        number: u64,
        title: String,
    },
    Run {
        repo: PathBuf,
        id: u64,
        title: String,
    },
    /// The failed-jobs log of a CI run (read-only text).
    RunLog {
        repo: PathBuf,
        id: u64,
        title: String,
    },
    /// The changes between two commits (from the Git activity log).
    RangeDiff {
        repo: PathBuf,
        from: String,
        to: String,
        title: String,
    },
    /// One Git activity log entry's details (not restored after a
    /// restart).
    ActivityDetails {
        id: String,
        title: String,
    },
    /// Publish the project to a new GitHub repository (not restored).
    PublishToGitHub,
    /// The project's toolbar commands editor.
    Commands,
    /// The editor for the custom buttons above Claude sessions (global).
    SessionButtons,
    /// The form for a new pull request from the current branch.
    CreatePullRequest {
        repo: PathBuf,
    },
    /// The unsaved buffer of `path` against the file on disk. Not saved
    /// in `state.json` (the buffer is gone after a restart).
    Compare {
        path: PathBuf,
    },
}

impl DocKey {
    /// Whether this tab comes back after a restart.
    pub fn restorable(&self) -> bool {
        !matches!(
            self,
            Self::Compare { .. } | Self::ActivityDetails { .. } | Self::PublishToGitHub
        )
    }
}

/// A window's frame in screen points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SavedBounds {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    #[serde(default)]
    pub maximized: bool,
    /// macOS full screen (the frame is the size to return to).
    #[serde(default)]
    pub fullscreen: bool,
}

/// What one pane shows (Phase 3 split view).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PaneContent {
    Session { session_id: String },
    Terminal { index: usize },
    /// Phase 4: a document tab.
    Doc { doc: DocKey },
}

/// One open project window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub project_id: Uuid,
    #[serde(default)]
    pub bounds: Option<SavedBounds>,
    #[serde(default)]
    pub selected_session: Option<String>,
    #[serde(default)]
    pub active_tab: RightTab,
    #[serde(default = "default_sidebar_width")]
    pub sidebar_width: f32,
    /// Sessions running when the app quit (for "resume on launch").
    #[serde(default)]
    pub running_sessions: Vec<String>,
    /// Phase 3: how many terminal tabs were open (restored as fresh shells).
    #[serde(default = "one")]
    pub terminal_tabs: usize,
    /// Phase 3: the second pane, when the view is split.
    #[serde(default)]
    pub split: Option<PaneContent>,
    /// Phase 3: the split ratio of the first pane (0.2–0.8).
    #[serde(default = "half")]
    pub split_ratio: f32,
    /// Phase 4: the sidebar tab.
    #[serde(default)]
    pub sidebar_tab: SidebarTab,
    /// Phase 4: open document tabs, in order.
    #[serde(default)]
    pub docs: Vec<DocKey>,
    /// Phase 4: the shown document tab, when `active_tab` is `Doc`.
    #[serde(default)]
    pub active_doc: Option<usize>,
    /// Phase 4: folders expanded in the Files tab.
    #[serde(default)]
    pub expanded_folders: Vec<PathBuf>,
    /// Notes: whether the Notes panel is open, and its width.
    #[serde(default)]
    pub notes_open: bool,
    #[serde(default = "default_notes_width")]
    pub notes_width: f32,
}

fn default_notes_width() -> f32 {
    340.0
}

fn default_sidebar_width() -> f32 {
    260.0
}

fn one() -> usize {
    1
}

fn half() -> f32 {
    0.5
}

impl WindowState {
    pub fn new(project_id: Uuid) -> Self {
        Self {
            project_id,
            bounds: None,
            selected_session: None,
            active_tab: RightTab::Claude,
            sidebar_width: default_sidebar_width(),
            running_sessions: Vec::new(),
            terminal_tabs: 1,
            split: None,
            split_ratio: 0.5,
            sidebar_tab: SidebarTab::Sessions,
            docs: Vec::new(),
            active_doc: None,
            expanded_folders: Vec::new(),
            notes_open: false,
            notes_width: default_notes_width(),
        }
    }
}

/// `state.json`: the windows to reopen on launch.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppState {
    pub windows: Vec<WindowState>,
}

impl AppState {
    pub fn load(path: &Path) -> Self {
        atomic::read_json_or_default(path)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        atomic::write_json(path, self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_defaults_match_the_plan() {
        let s = Settings::default();
        assert!(s.confirm_on_close);
        assert!(!s.resume_on_launch);
        assert_eq!(s.terminal.font_family, "Menlo");
        assert_eq!(s.terminal.font_size, 13.0);
        assert_eq!(s.terminal.scrollback_lines, 10_000);
        assert!(!s.terminal.cursor_blink);
        assert!(s.terminal.option_as_meta);
        assert_eq!(s.idle_stop_minutes, 30);
        assert!(!s.editor.format_on_save);
        assert!(s.editor.autosave);
        // Settings saved before autosave existed get it turned on.
        let old: Settings = serde_json::from_str(r#"{"editor":{"soft_wrap":true}}"#).unwrap_or_default();
        assert!(old.editor.soft_wrap && old.editor.autosave);
        assert_eq!(s.pull_mode, PullMode::FastForwardOnly);
        assert_eq!(
            s.lsp_command("typescript"),
            Some(vec![
                "typescript-language-server".to_owned(),
                "--stdio".to_owned()
            ])
        );
        assert_eq!(s.lsp_command("cobol"), None);
    }

    #[test]
    fn settings_round_trip_and_tolerate_missing_fields() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("settings.json");
        let s = Settings {
            default_flags: "--permission-mode plan".into(),
            ..Settings::default()
        };
        s.save(&path)?;
        assert_eq!(Settings::load(&path), s);

        std::fs::write(&path, r#"{"confirm_on_close": false, "unknown_key": 1}"#)?;
        let loaded = Settings::load(&path);
        assert!(!loaded.confirm_on_close);
        assert_eq!(loaded.terminal, TerminalPrefs::default());
        Ok(())
    }

    #[test]
    fn default_flags_split_with_shell_rules() {
        let s = Settings {
            default_flags: r#"--permission-mode plan --append-system-prompt "be brief""#.into(),
            ..Settings::default()
        };
        assert_eq!(
            s.default_flag_args(),
            Ok(vec![
                "--permission-mode".to_owned(),
                "plan".to_owned(),
                "--append-system-prompt".to_owned(),
                "be brief".to_owned(),
            ])
        );
        let bad = Settings {
            default_flags: "\"unterminated".into(),
            ..Settings::default()
        };
        assert!(bad.default_flag_args().is_err());
    }

    #[test]
    fn opening_the_same_folder_twice_gives_one_project() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let folder = dir.path().join("my-app");
        std::fs::create_dir(&folder)?;
        let mut store = ProjectStore::default();
        let a = store.open_folder(&folder)?;
        // A different spelling of the same folder.
        let b = store.open_folder(&folder.join("."))?;
        assert_eq!(a, b);
        assert_eq!(store.projects.len(), 1);
        let project = store.get(a).map(|p| p.name.clone());
        assert_eq!(project.as_deref(), Some("my-app"));
        Ok(())
    }

    #[test]
    fn recent_orders_by_last_opened() {
        let mut store = ProjectStore::default();
        let mut a = Project::new("/a".into());
        a.last_opened_at = 1;
        let mut b = Project::new("/b".into());
        b.last_opened_at = 2;
        store.projects = vec![a, b];
        let names: Vec<_> = store.recent().iter().map(|p| p.name.clone()).collect();
        assert_eq!(names, vec!["b", "a"]);
    }

    #[test]
    fn project_commands_round_trip_with_defaults() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("projects.json");
        let mut store = ProjectStore::default();
        let id = store.open_folder(dir.path())?;
        if let Some(p) = store.get_mut(id) {
            p.commands.push(ProjectCommand::new("Build", "cargo build"));
        }
        store.save(&path)?;
        assert_eq!(ProjectStore::load(&path), store);
        // Older files (and minimal entries) get the defaults.
        let c: ProjectCommand =
            serde_json::from_str(r#"{"id":"x","name":"Run","command":"cargo run"}"#)?;
        assert!(c.show_in_toolbar);
        assert!(!c.new_terminal);
        assert!(c.shortcut.is_empty());
        Ok(())
    }

    #[test]
    fn state_round_trip() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.json");
        let mut w = WindowState::new(Uuid::new_v4());
        w.bounds = Some(SavedBounds {
            x: 10.0,
            y: 20.0,
            width: 800.0,
            height: 600.0,
            maximized: false,
            fullscreen: true,
        });
        w.selected_session = Some("abc".into());
        w.active_tab = RightTab::Terminal;
        w.running_sessions = vec!["abc".into()];
        w.split = Some(PaneContent::Terminal { index: 0 });
        w.sidebar_tab = SidebarTab::Git;
        w.docs = vec![
            DocKey::File {
                path: "/tmp/a.rs".into(),
            },
            DocKey::GitDiff {
                repo: "/tmp".into(),
                path: "a.rs".into(),
                diff: DiffKind::Staged,
            },
            DocKey::PullRequest {
                repo: "/tmp".into(),
                number: 3,
                title: "T".into(),
            },
        ];
        w.active_tab = RightTab::Doc;
        w.active_doc = Some(1);
        let state = AppState { windows: vec![w] };
        state.save(&path)?;
        assert_eq!(AppState::load(&path), state);
        Ok(())
    }

    #[test]
    fn old_state_files_get_defaults() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.json");
        let id = Uuid::new_v4();
        std::fs::write(&path, format!(r#"{{"windows":[{{"project_id":"{id}"}}]}}"#))?;
        let state = AppState::load(&path);
        assert_eq!(state.windows.len(), 1);
        assert_eq!(state.windows[0].sidebar_width, 260.0);
        assert_eq!(state.windows[0].terminal_tabs, 1);
        Ok(())
    }
}
