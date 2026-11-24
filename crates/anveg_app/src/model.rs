//! App-wide state shared by every window: settings, projects, the detected
//! Claude Code, the child environment, open windows, saving `state.json`,
//! and the hook server (Phase 2).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anveg_core::claude_env::{self, ClaudeInfo, EnvMap};
use anveg_core::config::{AppState, ProjectStore, Settings, WindowState};
use anveg_core::fswatch::FsWatchService;
use anveg_core::gh::{Gh, GhState};
use anveg_core::git::Git;
use anveg_core::paths::AppPaths;
use anveg_core::status::{HookMessage, HookServer, hooks_settings};
use anveg_term::TerminalSettings;
use futures::StreamExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, EventEmitter, Global, Task, WeakEntity,
};
use uuid::Uuid;

use crate::lsp_manager::LspManager;
use crate::project_window::ProjectWindow;

/// The handle to the [`AppModel`], stored as a GPUI global.
pub struct ModelHandle(pub Entity<AppModel>);

impl Global for ModelHandle {}

/// The app model.
pub fn model(cx: &App) -> Entity<AppModel> {
    cx.global::<ModelHandle>().0.clone()
}

/// What the model tells windows.
#[derive(Clone, Debug)]
pub enum ModelEvent {
    /// `claude` detection and environment capture finished.
    ClaudeDetected,
    /// Settings changed (saved already).
    SettingsChanged,
    /// The projects list changed (saved already).
    ProjectsChanged,
    /// A hook event arrived from `anveg-hook`.
    Hook(Box<HookMessage>),
    /// The number of sessions needing input changed somewhere.
    AttentionChanged,
    /// Phase 6: `gh` was (re)detected.
    GhChanged,
}

/// An open project window.
pub struct OpenWindow {
    pub project_id: Uuid,
    pub handle: AnyWindowHandle,
    pub view: WeakEntity<ProjectWindow>,
}

/// The hook server and the settings file that points Claude Code at
/// `anveg-hook`.
pub struct HookSetup {
    _server: HookServer,
    pub socket: PathBuf,
    pub settings_file: PathBuf,
    /// The same hooks plus the status line that feeds the session toolbar.
    pub settings_file_status_line: PathBuf,
}

pub struct AppModel {
    pub paths: AppPaths,
    pub settings: Settings,
    pub projects: ProjectStore,
    /// `None` while detection runs.
    pub claude: Option<ClaudeInfo>,
    /// The environment for child processes; `None` while capturing.
    pub child_env: Option<EnvMap>,
    pub windows: Vec<OpenWindow>,
    pub welcome: Option<AnyWindowHandle>,
    pub settings_window: Option<AnyWindowHandle>,
    pub hooks: Option<HookSetup>,
    /// Set while quitting, so closing windows does not forget them.
    pub quitting: bool,
    /// Phase 4: the one shared file watcher.
    pub fswatch: Option<Arc<FsWatchService>>,
    /// Phase 4: language servers.
    pub lsp: Entity<LspManager>,
    /// Phase 5–6: `git` and `gh` on the captured `PATH`.
    pub git_path: Option<PathBuf>,
    pub gh_path: Option<PathBuf>,
    /// Phase 6: `None` until checked.
    pub gh_state: Option<GhState>,
    gh_checking: bool,
    gh_checked_at: Option<Instant>,
    save_task: Option<Task<()>>,
    _hook_task: Option<Task<()>>,
}

impl EventEmitter<ModelEvent> for AppModel {}

impl AppModel {
    pub fn new(paths: AppPaths, cx: &mut Context<Self>) -> Self {
        let settings = Settings::load(&paths.settings_file());
        let projects = ProjectStore::load(&paths.projects_file());
        let fswatch = match FsWatchService::new() {
            Ok(w) => Some(Arc::new(w)),
            Err(error) => {
                tracing::error!(%error, "could not start the file watcher; files will not update live");
                None
            }
        };
        let lsp = cx.new(LspManager::new);
        let mut this = Self {
            paths,
            settings,
            projects,
            claude: None,
            child_env: None,
            windows: Vec::new(),
            welcome: None,
            settings_window: None,
            hooks: None,
            quitting: false,
            fswatch,
            lsp,
            git_path: None,
            gh_path: None,
            gh_state: None,
            gh_checking: false,
            gh_checked_at: None,
            save_task: None,
            _hook_task: None,
        };
        this.start_hooks(cx);
        this.detect(cx);
        this
    }

    /// Capture the login environment and detect `claude`, on a background
    /// thread.
    pub fn detect(&mut self, cx: &mut Context<Self>) {
        let override_path = self.settings.claude_path.clone();
        let have_env = self.child_env.clone();
        let task = cx.background_spawn(async move {
            let env = match have_env {
                Some(env) => env,
                None => {
                    let captured = claude_env::capture_login_env(
                        &claude_env::user_shell(),
                        Duration::from_secs(5),
                    );
                    claude_env::child_env(captured.as_ref())
                }
            };
            let info = claude_env::detect_claude(override_path.as_deref(), &env);
            let path_var = env.get("PATH").cloned();
            let git = claude_env::find_in_path("git", path_var.as_deref());
            let gh = claude_env::find_in_path("gh", path_var.as_deref());
            (env, info, git, gh)
        });
        cx.spawn(async move |this, cx| {
            let (env, info, git, gh) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.child_env = Some(env);
                this.claude = Some(info);
                this.git_path = git;
                this.gh_path = gh;
                cx.emit(ModelEvent::ClaudeDetected);
                this.check_gh(true, cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// How to run git (the captured environment).
    pub fn git(&self) -> Git {
        Git::new(
            self.git_path.clone().unwrap_or_else(|| PathBuf::from("git")),
            self.child_env.clone(),
        )
    }

    /// How to run gh, if it is installed.
    pub fn gh(&self) -> Option<Gh> {
        Some(Gh::new(self.gh_path.clone()?, self.child_env.clone()))
    }

    /// Run `gh --version` and `gh auth status` in the background. Without
    /// `force`, only when the last check failed and was over 30 s ago (a
    /// window gained focus).
    pub fn check_gh(&mut self, force: bool, cx: &mut Context<Self>) {
        if self.gh_checking || self.child_env.is_none() {
            return;
        }
        if !force {
            let ready = matches!(self.gh_state, Some(GhState::Ready { .. }));
            let recent = self
                .gh_checked_at
                .is_some_and(|t| t.elapsed() < Duration::from_secs(30));
            if ready || recent {
                return;
            }
            // `gh` may have been installed since: look on PATH again.
            let path_var = self.child_env.as_ref().and_then(|e| e.get("PATH").cloned());
            self.gh_path = claude_env::find_in_path("gh", path_var.as_deref());
        }
        let Some(gh) = self.gh() else {
            self.gh_state = Some(GhState::Missing);
            self.gh_checked_at = Some(Instant::now());
            cx.emit(ModelEvent::GhChanged);
            return;
        };
        self.gh_checking = true;
        let dir = std::env::temp_dir();
        let task = cx.background_spawn(async move { gh.detect(&dir) });
        cx.spawn(async move |this, cx| {
            let state = task.await;
            let _ = this.update(cx, |this, cx| {
                this.gh_checking = false;
                this.gh_checked_at = Some(Instant::now());
                if this.gh_state.as_ref() != Some(&state) {
                    this.gh_state = Some(state);
                    cx.emit(ModelEvent::GhChanged);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Whether `claude` was found and runs.
    pub fn claude_path(&self) -> Option<PathBuf> {
        self.claude.as_ref().and_then(|c| c.path.clone())
    }

    pub fn config_dir(&self) -> PathBuf {
        match &self.claude {
            Some(info) => info.config_dir.clone(),
            None => claude_env::fallback_config_dir(&claude_env::child_env(None)),
        }
    }

    pub fn terminal_settings(&self) -> TerminalSettings {
        let t = &self.settings.terminal;
        TerminalSettings {
            font_family: t.font_family.clone(),
            font_size: t.font_size.clamp(6.0, 72.0),
            scrollback_lines: t.scrollback_lines,
            cursor_blink: t.cursor_blink,
            option_as_meta: t.option_as_meta,
        }
    }

    pub fn update_settings(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings)) {
        let before = self.settings.claude_path.clone();
        f(&mut self.settings);
        if let Err(error) = self.settings.save(&self.paths.settings_file()) {
            tracing::error!(%error, "could not save settings");
        }
        if self.settings.claude_path != before {
            self.claude = None;
            self.detect(cx);
        }
        cx.emit(ModelEvent::SettingsChanged);
        cx.notify();
    }

    pub fn update_projects(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut ProjectStore)) {
        f(&mut self.projects);
        if let Err(error) = self.projects.save(&self.paths.projects_file()) {
            tracing::error!(%error, "could not save projects");
        }
        // The menus read the model, so rebuild them after this update;
        // toolbar command shortcuts may have changed too.
        cx.defer(|cx| {
            crate::menus::install(cx);
            crate::project_commands::bind_shortcuts(cx);
        });
        cx.emit(ModelEvent::ProjectsChanged);
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Windows and state.json
    // -----------------------------------------------------------------

    pub fn window_for(&self, project_id: Uuid) -> Option<&OpenWindow> {
        self.windows.iter().find(|w| w.project_id == project_id)
    }

    pub fn register_window(&mut self, window: OpenWindow, cx: &mut Context<Self>) {
        self.windows.retain(|w| w.project_id != window.project_id);
        self.windows.push(window);
        self.schedule_save(cx);
    }

    /// A project window closed (by the user, not by quitting).
    pub fn window_closed(&mut self, project_id: Uuid, cx: &mut Context<Self>) {
        if self.quitting {
            return;
        }
        self.windows.retain(|w| w.project_id != project_id);
        self.save_state_now(cx);
        if self.windows.is_empty() {
            cx.defer(crate::welcome::open);
        }
    }

    /// Save `state.json` after 500 ms of quiet.
    pub fn schedule_save(&mut self, cx: &mut Context<Self>) {
        if self.quitting {
            return;
        }
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            let _ = this.update(cx, |this, cx| this.save_state_now(cx));
        }));
    }

    /// Gather every open window's state and write `state.json`.
    pub fn save_state_now(&mut self, cx: &mut Context<Self>) {
        self.save_task = None;
        let state = self.collect_state(cx);
        if let Err(error) = state.save(&self.paths.state_file()) {
            tracing::error!(%error, "could not save window state");
        }
    }

    fn collect_state(&self, cx: &App) -> AppState {
        let windows: Vec<WindowState> = self
            .windows
            .iter()
            .filter_map(|w| w.view.upgrade())
            .map(|view| view.read(cx).window_state(cx))
            .collect();
        AppState { windows }
    }

    /// Running Claude sessions across all windows: (window, session ids).
    pub fn running_sessions(&self, cx: &App) -> Vec<(AnyWindowHandle, Vec<String>)> {
        self.windows
            .iter()
            .filter_map(|w| {
                let view = w.view.upgrade()?;
                let names = view.read(cx).running_session_names();
                (!names.is_empty()).then_some((w.handle, names))
            })
            .collect()
    }

    /// Save the state (recording running sessions), stop every child and
    /// quit. Used after the quit confirmation, and by Dock quit / logout.
    pub fn quit_now(&mut self, cx: &mut Context<Self>) {
        if !self.quitting {
            self.quitting = true;
            self.save_state_now_for_quit(cx);
            self.stop_everything(cx);
        }
        cx.quit();
    }

    fn save_state_now_for_quit(&mut self, cx: &mut Context<Self>) {
        let state = self.collect_state(cx);
        if let Err(error) = state.save(&self.paths.state_file()) {
            tracing::error!(%error, "could not save window state");
        }
    }

    /// Stop every terminal in every window, and every language server.
    pub fn stop_everything(&mut self, cx: &mut Context<Self>) {
        self.lsp.update(cx, |l, cx| l.stop_all(cx));
        for window in &self.windows {
            if let Some(view) = window.view.upgrade() {
                view.update(cx, |view, cx| view.stop_all(cx));
            }
        }
    }

    /// Called from `on_app_quit` (Dock quit, logout, or after `quit_now`):
    /// never blocks, never asks.
    pub fn on_app_quit(&mut self, cx: &mut Context<Self>) {
        if !self.quitting {
            self.quitting = true;
            self.save_state_now_for_quit(cx);
            self.stop_everything(cx);
        }
    }

    // -----------------------------------------------------------------
    // Phase 2: hooks
    // -----------------------------------------------------------------

    /// Where `anveg-hook` is: next to this executable (in the `.app` bundle:
    /// `Contents/MacOS/anveg-hook`).
    pub fn hook_binary() -> Option<PathBuf> {
        let exe = std::env::current_exe().ok()?;
        let candidate = exe.parent()?.join("anveg-hook");
        candidate.is_file().then_some(candidate)
    }

    fn start_hooks(&mut self, cx: &mut Context<Self>) {
        let Some(binary) = Self::hook_binary() else {
            tracing::warn!(
                "anveg-hook not found next to the app; live status is off (run `cargo build` to build it)"
            );
            return;
        };
        let settings_file = self.paths.support_dir.join("hooks-settings.json");
        if let Err(error) = anveg_core::atomic::write_json(&settings_file, &hooks_settings(&binary))
        {
            tracing::error!(%error, "could not write hooks settings; live status is off");
            return;
        }
        let settings_file_status_line = self
            .paths
            .support_dir
            .join("hooks-statusline-settings.json");
        if let Err(error) = anveg_core::atomic::write_json(
            &settings_file_status_line,
            &anveg_core::status::hooks_settings_with_status_line(&binary),
        ) {
            tracing::warn!(%error, "could not write the status line settings");
        }
        // Unix socket paths are limited to ~104 bytes on macOS; fall back to
        // a short path in the temp directory when the support path is long.
        let mut socket = self.paths.support_dir.join("run").join("hooks.sock");
        if socket.as_os_str().len() > 100 {
            socket = std::env::temp_dir().join(format!("anveg-{}.sock", std::process::id()));
        }
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<HookMessage>();
        let server = match HookServer::start(socket.clone(), move |message| {
            let _ = tx.unbounded_send(message);
        }) {
            Ok(server) => server,
            Err(error) => {
                tracing::error!(%error, "could not start the hook server; live status is off");
                return;
            }
        };
        self.hooks = Some(HookSetup {
            _server: server,
            socket,
            settings_file,
            settings_file_status_line,
        });
        self._hook_task = Some(cx.spawn(async move |this, cx| {
            while let Some(message) = rx.next().await {
                if this
                    .update(cx, |_, cx| cx.emit(ModelEvent::Hook(Box::new(message))))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// Sessions needing input across all windows (Dock badge).
    pub fn attention_count(&self, cx: &App) -> usize {
        self.windows
            .iter()
            .filter_map(|w| w.view.upgrade())
            .map(|v| v.read(cx).needs_input_count())
            .sum()
    }

    /// Recompute the Dock badge.
    pub fn refresh_badge(&self, cx: &App) {
        crate::platform::set_badge_count(self.attention_count(cx));
    }
}

/// The child environment and the file watcher (for restarting servers).
pub fn model_env_and_watch(cx: &App) -> (Option<EnvMap>, Option<Arc<FsWatchService>>) {
    if !cx.has_global::<ModelHandle>() {
        return (None, None);
    }
    let m = model(cx).read(cx);
    (m.child_env.clone(), m.fswatch.clone())
}
