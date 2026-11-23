//! App-wide state shared by every window: settings, projects, the detected
//! Claude Code, the child environment, open windows, saving `state.json`,
//! and the hook server (Phase 2).

use std::path::PathBuf;
use std::time::Duration;

use anveg_core::claude_env::{self, ClaudeInfo, EnvMap};
use anveg_core::config::{AppState, ProjectStore, Settings, WindowState};
use anveg_core::paths::AppPaths;
use anveg_core::status::{HookMessage, HookServer, hooks_settings};
use anveg_term::TerminalSettings;
use futures::StreamExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, EventEmitter, Global, Task, WeakEntity,
};
use uuid::Uuid;

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
    Hook(HookMessage),
    /// The number of sessions needing input changed somewhere.
    AttentionChanged,
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
    save_task: Option<Task<()>>,
    _hook_task: Option<Task<()>>,
}

impl EventEmitter<ModelEvent> for AppModel {}

impl AppModel {
    pub fn new(paths: AppPaths, cx: &mut Context<Self>) -> Self {
        let settings = Settings::load(&paths.settings_file());
        let projects = ProjectStore::load(&paths.projects_file());
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
            (env, info)
        });
        cx.spawn(async move |this, cx| {
            let (env, info) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.child_env = Some(env);
                this.claude = Some(info);
                cx.emit(ModelEvent::ClaudeDetected);
                cx.notify();
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
        // The menus read the model, so rebuild them after this update.
        cx.defer(|cx| crate::menus::install(cx));
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

    /// Stop every terminal in every window.
    pub fn stop_everything(&mut self, cx: &mut Context<Self>) {
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
        });
        self._hook_task = Some(cx.spawn(async move |this, cx| {
            while let Some(message) = rx.next().await {
                if this
                    .update(cx, |_, cx| cx.emit(ModelEvent::Hook(message)))
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
