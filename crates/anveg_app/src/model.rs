//! App-wide state shared by every window: settings, projects, the detected
//! Claude Code, the child environment, open windows, saving `state.json`,
//! and the hook server (Phase 2).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anveg_core::claude_env::{self, ClaudeInfo, EnvMap};
use anveg_core::config::{AppState, AppWindowState, ProjectStore, Settings, WindowState};
use anveg_core::fswatch::FsWatchService;
use anveg_core::github_api::{self, GitHub, Secret, Transport, UreqTransport};
use anveg_core::github_auth::{self, KeychainStore, Poll, TokenStore};
use anveg_core::op_log::{OpLog, Level};
use anveg_core::git::Git;
use anveg_core::paths::AppPaths;
use anveg_core::status::{HookMessage, HookServer, hooks_settings};
use anveg_term::TerminalSettings;
use futures::StreamExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Context, Entity, EntityId, EventEmitter, Global, Task,
    WeakEntity,
};
use uuid::Uuid;

use crate::app_window::AppWindow;
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
    /// Signed in or out of GitHub (or the sign-in is still being checked).
    GitHubChanged,
    /// A project opened, closed or moved, or a window was renamed (for
    /// the project tabs).
    WindowsChanged,
}

/// Signed in to GitHub or not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthState {
    /// Reading the keychain and checking the token.
    Checking,
    SignedOut,
    /// `login` is empty when GitHub could not be reached to check.
    SignedIn { login: String },
}

/// A device-flow sign-in waiting for the user (empty code: still asking
/// GitHub for one).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceSignIn {
    pub user_code: String,
    pub verification_uri: String,
}

enum VerifyError {
    /// 401: the token is wrong, expired or revoked.
    Rejected(String),
    /// Could not ask (offline, GitHub down).
    Unreachable(String),
}

/// Who a token belongs to (`GET /user`).
fn verify(transport: &Arc<dyn Transport>, token: Secret, log: &OpLog) -> Result<String, VerifyError> {
    let api = github_api::Api::new(transport.clone(), Some(token)).with_log(log.clone());
    match log.op("Checking the token with GitHub", || api.get::<github_api::User>("/user")) {
        Ok(u) => Ok(u.login),
        Err(e) if github_api::is_signed_out(&e) => Err(VerifyError::Rejected(
            "GitHub did not accept that token (wrong, expired or revoked).".into(),
        )),
        Err(e) if github_api::api_status(&e).is_some() => Err(VerifyError::Rejected(format!("{e:#}"))),
        Err(e) => Err(VerifyError::Unreachable(format!("{e:#}"))),
    }
}

/// The keychain entry's service: separate for development builds, so they
/// never touch the installed app's sign-in.
fn keychain_service() -> &'static str {
    if cfg!(debug_assertions) {
        "dev.anveg.code.dev.github"
    } else {
        "dev.anveg.code.github"
    }
}

/// An open project: a tab of an app window.
pub struct OpenWindow {
    pub project_id: Uuid,
    /// The app window it is in.
    pub handle: AnyWindowHandle,
    pub view: WeakEntity<ProjectWindow>,
    pub host: WeakEntity<AppWindow>,
}

/// An open app window.
pub struct OpenAppWindow {
    pub id: Uuid,
    pub handle: AnyWindowHandle,
    pub view: WeakEntity<AppWindow>,
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
    /// Open projects, in the order they opened.
    pub windows: Vec<OpenWindow>,
    /// App windows, in the order they opened.
    pub app_windows: Vec<OpenAppWindow>,
    /// The app window last in front: projects opened from elsewhere (the
    /// welcome window, the Dock) become its tabs.
    pub last_active_window: Option<Uuid>,
    pub welcome: Option<AnyWindowHandle>,
    pub settings_window: Option<AnyWindowHandle>,
    /// The Open from GitHub window, if open.
    pub github_window: Option<AnyWindowHandle>,
    pub hooks: Option<HookSetup>,
    /// Set while quitting, so closing windows does not forget them.
    pub quitting: bool,
    /// Phase 4: the one shared file watcher.
    pub fswatch: Option<Arc<FsWatchService>>,
    /// Phase 4: language servers.
    pub lsp: Entity<LspManager>,
    /// `git` on the captured `PATH`.
    pub git_path: Option<PathBuf>,
    /// GitHub: signed in or not (the token itself is in the keychain).
    pub github_auth: AuthState,
    github_token: Option<Secret>,
    /// A device-flow sign-in in progress.
    pub device_sign_in: Option<DeviceSignIn>,
    /// The last sign-in problem, to show next to the sign-in buttons.
    pub github_note: Option<String>,
    /// What the app did with GitHub this run (the log under the GitHub
    /// panel). Memory only.
    pub github_log: OpLog,
    /// What the app did with git (commands that change a repository or
    /// reach a remote).
    pub git_log: OpLog,
    transport: Arc<dyn Transport>,
    token_store: Arc<dyn TokenStore>,
    /// The `GIT_ASKPASS` script that hands git the token.
    askpass_script: Option<PathBuf>,
    device_task: Option<Task<()>>,
    save_task: Option<Task<()>>,
    /// The Git tab's unfinished and recent commit messages.
    pub commit_messages: anveg_core::commit_messages::CommitMessages,
    commit_messages_task: Option<Task<()>>,
    _hook_task: Option<Task<()>>,
}

impl EventEmitter<ModelEvent> for AppModel {}

impl AppModel {
    pub fn new(paths: AppPaths, cx: &mut Context<Self>) -> Self {
        let settings = Settings::load(&paths.settings_file());
        let projects = ProjectStore::load(&paths.projects_file());
        let commit_messages = anveg_core::commit_messages::CommitMessages::load(&paths.commit_messages_file());
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
            app_windows: Vec::new(),
            last_active_window: None,
            welcome: None,
            settings_window: None,
            github_window: None,
            hooks: None,
            quitting: false,
            fswatch,
            lsp,
            git_path: None,
            github_auth: AuthState::Checking,
            github_token: None,
            device_sign_in: None,
            github_note: None,
            github_log: OpLog::new(),
            git_log: OpLog::new(),
            transport: Arc::new(UreqTransport::new()),
            token_store: Arc::new(KeychainStore::new(keychain_service())),
            askpass_script: None,
            device_task: None,
            save_task: None,
            commit_messages,
            commit_messages_task: None,
            _hook_task: None,
        };
        this.start_hooks(cx);
        this.detect(cx);
        this.load_github(cx);
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
            (env, info, git)
        });
        cx.spawn(async move |this, cx| {
            let (env, info, git) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.child_env = Some(env);
                this.claude = Some(info);
                this.git_path = git;
                cx.emit(ModelEvent::ClaudeDetected);
                cx.notify();
            });
        })
        .detach();
    }

    /// How to run git (the captured environment). While signed in to
    /// GitHub, git's https prompts for github.com are answered with the token.
    pub fn git(&self) -> Git {
        let askpass = match (&self.askpass_script, &self.github_token) {
            (Some(script), Some(token)) => Some(anveg_core::git::Askpass {
                script: script.clone(),
                token: token.clone(),
            }),
            _ => None,
        };
        Git::new(
            self.git_path.clone().unwrap_or_else(|| PathBuf::from("git")),
            self.child_env.clone(),
        )
        .with_askpass(askpass)
        .with_log(Some(self.git_log.clone()))
    }

    /// GitHub's API as the signed-in user, if signed in.
    pub fn github(&self) -> Option<GitHub> {
        let token = self.github_token.clone()?;
        Some(GitHub::new(
            github_api::Api::new(self.transport.clone(), Some(token)).with_log(self.github_log.clone()),
            self.git(),
        ))
    }

    pub fn is_signed_in(&self) -> bool {
        matches!(self.github_auth, AuthState::SignedIn { .. })
    }

    fn set_auth(&mut self, auth: AuthState, cx: &mut Context<Self>) {
        if self.github_auth != auth {
            self.github_auth = auth;
            cx.emit(ModelEvent::GitHubChanged);
        }
        cx.notify();
    }

    /// At start: the token from the keychain, checked against GitHub.
    fn load_github(&mut self, cx: &mut Context<Self>) {
        let store = self.token_store.clone();
        let transport = self.transport.clone();
        let askpass_dir = self.paths.support_dir.clone();
        let log = self.github_log.clone();
        let task = cx.background_spawn(async move {
            let askpass = github_auth::write_askpass(&askpass_dir)
                .map_err(|error| tracing::warn!(%error, "could not write the git askpass script"))
                .ok();
            let token = match store.get() {
                Ok(t) => t,
                Err(error) => {
                    tracing::warn!(%error, "keychain");
                    log.push(Level::Failed, format!("Reading the saved sign-in from the keychain: {error:#}"));
                    None
                }
            };
            match &token {
                Some(_) => log.info("Found a saved sign-in in the keychain"),
                None => log.info("No saved GitHub sign-in"),
            }
            let login = token.clone().map(|t| verify(&transport, t, &log));
            (askpass, token, login)
        });
        cx.spawn(async move |this, cx| {
            let (askpass, token, login) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.askpass_script = askpass;
                this.apply_verified(token, login, false, cx);
            });
        })
        .detach();
    }

    /// Act on a token's check: signed in, signed out (revoked: forget it),
    /// or GitHub unreachable (stay signed in, the token may well be fine).
    fn apply_verified(
        &mut self,
        token: Option<Secret>,
        login: Option<Result<String, VerifyError>>,
        save: bool,
        cx: &mut Context<Self>,
    ) {
        match (token, login) {
            (Some(token), Some(Ok(login))) => {
                self.github_note = None;
                self.save_token(&token, save);
                self.github_log.info(format!("Signed in to GitHub as {login}"));
                self.github_token = Some(token);
                self.set_auth(AuthState::SignedIn { login }, cx);
            }
            (Some(token), Some(Err(VerifyError::Unreachable(message)))) => {
                tracing::info!(%message, "GitHub unreachable; keeping the saved token");
                self.github_log
                    .info("GitHub could not be reached; keeping the sign-in (it is checked again on the next call)");
                self.save_token(&token, save);
                self.github_token = Some(token);
                self.set_auth(AuthState::SignedIn { login: String::new() }, cx);
            }
            (Some(_), Some(Err(VerifyError::Rejected(message)))) => {
                if !save {
                    let _ = self.token_store.delete();
                    self.github_log.info("Removed the saved sign-in from the keychain");
                }
                self.github_log.push(Level::Failed, format!("Not signed in: {message}"));
                self.github_token = None;
                self.github_note = Some(message);
                self.set_auth(AuthState::SignedOut, cx);
            }
            _ => {
                self.github_token = None;
                self.set_auth(AuthState::SignedOut, cx);
            }
        }
    }

    /// Keep the token in the keychain (when it is new).
    fn save_token(&mut self, token: &Secret, save: bool) {
        if !save {
            return;
        }
        match self.token_store.set(token) {
            Ok(()) => self.github_log.info("Saved the sign-in in the keychain"),
            Err(e) => {
                self.github_log
                    .push(Level::Failed, format!("Saving the sign-in in the keychain: {e:#}"));
                self.github_note = Some(format!("{e:#}"));
            }
        }
    }

    /// Settings: a pasted personal access token.
    pub fn sign_in_with_token(&mut self, text: &str, cx: &mut Context<Self>) {
        let Some(token) = github_auth::clean_token(text) else {
            self.github_log
                .push(Level::Failed, "Sign in with a token: that does not look like a GitHub token");
            self.github_note = Some("That does not look like a GitHub token.".into());
            cx.notify();
            return;
        };
        self.github_log.info("Signing in with a personal access token");
        self.github_note = None;
        self.set_auth(AuthState::Checking, cx);
        let transport = self.transport.clone();
        let t = token.clone();
        let log = self.github_log.clone();
        let task = cx.background_spawn(async move { verify(&transport, t, &log) });
        cx.spawn(async move |this, cx| {
            let login = task.await;
            let _ = this.update(cx, |this, cx| this.apply_verified(Some(token), Some(login), true, cx));
        })
        .detach();
    }

    /// The OAuth App's client ID: Settings, else `ANVEG_GITHUB_CLIENT_ID`
    /// (at run time, or baked in at build time).
    pub fn github_client_id(&self) -> Option<String> {
        self.settings
            .github_client_id
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| std::env::var("ANVEG_GITHUB_CLIENT_ID").ok().filter(|s| !s.is_empty()))
            .or_else(|| option_env!("ANVEG_GITHUB_CLIENT_ID").map(str::to_owned))
            .map(|s| s.trim().to_owned())
    }

    /// "Sign in with GitHub": show a code, open github.com/login/device,
    /// and wait for the user to approve.
    pub fn start_device_sign_in(&mut self, cx: &mut Context<Self>) {
        let Some(client_id) = self.github_client_id() else {
            self.github_log
                .push(Level::Failed, "Sign in with GitHub: no OAuth App client ID is set (Settings ▸ GitHub)");
            self.github_note = Some(
                "Signing in through the browser needs the OAuth App's client ID (Settings ▸ GitHub). \
                 You can paste a personal access token instead."
                    .into(),
            );
            cx.notify();
            return;
        };
        self.github_note = None;
        let transport = self.transport.clone();
        let log = self.github_log.clone();
        self.device_sign_in = Some(DeviceSignIn {
            user_code: String::new(),
            verification_uri: String::new(),
        });
        cx.notify();
        self.device_task = Some(cx.spawn(async move |this, cx| {
            let t = transport.clone();
            let id = client_id.clone();
            let l = log.clone();
            let code = cx
                .background_spawn(async move {
                    l.op("Asking GitHub for a sign-in code", || {
                        github_auth::request_device_code(t.as_ref(), &id)
                    })
                })
                .await;
            let code = match code {
                Ok(code) => code,
                Err(e) => {
                    let _ = this.update(cx, |this, cx| {
                        this.device_sign_in = None;
                        this.github_note = Some(format!("{e:#}"));
                        cx.notify();
                    });
                    return;
                }
            };
            let _ = this.update(cx, |this, cx| {
                this.device_sign_in = Some(DeviceSignIn {
                    user_code: code.user_code.clone(),
                    verification_uri: code.verification_uri.clone(),
                });
                cx.open_url(&code.verification_uri);
                cx.notify();
            });
            log.info(format!(
                "Opened {} in the browser; waiting for you to enter the code and approve",
                code.verification_uri
            ));
            let mut interval = code.interval.max(1);
            let deadline = Instant::now() + Duration::from_secs(code.expires_in.max(60));
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(interval))
                    .await;
                if Instant::now() > deadline {
                    log.push(Level::Failed, "Sign in with GitHub: the code expired before it was approved");
                    let _ = this.update(cx, |this, cx| {
                        this.device_sign_in = None;
                        this.github_note = Some("The sign-in code expired. Try again.".into());
                        cx.notify();
                    });
                    return;
                }
                let (t, id, dc) = (transport.clone(), client_id.clone(), code.device_code.clone());
                let poll = cx
                    .background_spawn(async move { github_auth::poll_device_token(t.as_ref(), &id, &dc) })
                    .await;
                match poll {
                    Poll::Pending => {}
                    Poll::SlowDown => {
                        interval += 5;
                        log.detail(format!("GitHub asked to poll more slowly (every {interval} s)"));
                    }
                    Poll::Token(token) => {
                        log.push(Level::Ok, "GitHub approved the sign-in");
                        let t = transport.clone();
                        let tok = token.clone();
                        let l = log.clone();
                        let login = cx.background_spawn(async move { verify(&t, tok, &l) }).await;
                        let _ = this.update(cx, |this, cx| {
                            this.device_sign_in = None;
                            this.apply_verified(Some(token), Some(login), true, cx);
                        });
                        return;
                    }
                    other => {
                        let message = match other {
                            Poll::Denied => "The sign-in was cancelled on GitHub.".to_owned(),
                            Poll::Expired => "The sign-in code expired. Try again.".to_owned(),
                            Poll::Failed(m) => m,
                            _ => String::new(),
                        };
                        log.push(Level::Failed, format!("Sign in with GitHub: {message}"));
                        let _ = this.update(cx, |this, cx| {
                            this.device_sign_in = None;
                            this.github_note = Some(message);
                            cx.notify();
                        });
                        return;
                    }
                }
            }
        }));
    }

    pub fn cancel_device_sign_in(&mut self, cx: &mut Context<Self>) {
        if self.device_task.take().is_some() {
            self.github_log.info("Sign in with GitHub cancelled");
        }
        self.device_sign_in = None;
        cx.notify();
    }

    pub fn sign_out(&mut self, cx: &mut Context<Self>) {
        match self.token_store.delete() {
            Ok(()) => self.github_log.info("Signed out; removed the sign-in from the keychain"),
            Err(e) => {
                self.github_log
                    .push(Level::Failed, format!("Signing out: removing the sign-in from the keychain: {e:#}"));
                self.github_note = Some(format!("{e:#}"));
            }
        }
        self.github_token = None;
        self.device_task = None;
        self.device_sign_in = None;
        self.set_auth(AuthState::SignedOut, cx);
    }

    /// A GitHub call answered 401: the token was revoked or expired.
    pub fn github_rejected(&mut self, cx: &mut Context<Self>) {
        self.github_log.push(
            Level::Failed,
            "GitHub answered 401 (bad credentials): the saved sign-in no longer works; signed out",
        );
        let _ = self.token_store.delete();
        self.github_token = None;
        self.github_note = Some("GitHub no longer accepts the saved sign-in. Sign in again.".into());
        self.set_auth(AuthState::SignedOut, cx);
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
        cx.emit(ModelEvent::WindowsChanged);
        // The Window and Dock menus list the open projects.
        cx.defer(|cx| crate::menus::install(cx));
    }

    /// A project's view went away (closed or moved; not by quitting).
    pub fn window_closed(&mut self, view: EntityId, cx: &mut Context<Self>) {
        if self.quitting {
            return;
        }
        self.windows.retain(|w| w.view.entity_id() != view);
        self.save_state_now(cx);
        cx.emit(ModelEvent::WindowsChanged);
        cx.defer(|cx| crate::menus::install(cx));
        if self.windows.is_empty() {
            cx.defer(crate::welcome::open);
        }
    }

    pub fn register_app_window(&mut self, window: OpenAppWindow, cx: &mut Context<Self>) {
        self.app_windows.push(window);
        self.schedule_save(cx);
    }

    pub fn app_window_closed(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if self.quitting {
            return;
        }
        self.app_windows.retain(|w| w.id != id);
        if self.last_active_window == Some(id) {
            self.last_active_window = None;
        }
        self.save_state_now(cx);
        cx.defer(|cx| crate::menus::install(cx));
    }

    pub fn app_window(&self, id: Uuid) -> Option<&OpenAppWindow> {
        self.app_windows.iter().find(|w| w.id == id)
    }

    /// Keep the unfinished commit message for `repo` (saved after a pause).
    pub fn set_commit_draft(&mut self, repo: &std::path::Path, text: &str, cx: &mut Context<Self>) {
        if self.commit_messages.set_draft(repo, text) {
            self.save_commit_messages(Duration::from_millis(700), cx);
        }
    }

    /// `message` was committed in `repo`: into Recent, draft cleared.
    pub fn commit_recorded(&mut self, repo: &std::path::Path, message: &str, cx: &mut Context<Self>) {
        self.commit_messages.committed(repo, message);
        self.save_commit_messages(Duration::ZERO, cx);
    }

    /// Write `commit-messages.json` off the UI thread after `delay`.
    fn save_commit_messages(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let path = self.paths.commit_messages_file();
        self.commit_messages_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let Ok(messages) = this.update(cx, |this, _| this.commit_messages.clone()) else {
                return;
            };
            let write = cx.background_spawn(async move { messages.save(&path) });
            if let Err(error) = write.await {
                tracing::warn!(%error, "could not save commit messages");
            }
        }));
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
        let app_windows: Vec<AppWindowState> = self
            .app_windows
            .iter()
            .filter_map(|w| w.view.upgrade())
            .map(|view| view.read(cx).window_state(cx))
            .filter(|w| !w.projects.is_empty())
            .collect();
        AppState {
            windows,
            app_windows,
        }
    }

    /// Running Claude sessions across all projects: (window, project,
    /// session names).
    pub fn running_sessions(&self, cx: &App) -> Vec<(AnyWindowHandle, Uuid, Vec<String>)> {
        self.windows
            .iter()
            .filter_map(|w| {
                let view = w.view.upgrade()?;
                let names = view.read(cx).running_session_names();
                (!names.is_empty()).then_some((w.handle, w.project_id, names))
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
        // A draft typed in the last moment, before its delayed save.
        if self.commit_messages_task.take().is_some()
            && let Err(error) = self.commit_messages.save(&self.paths.commit_messages_file())
        {
            tracing::warn!(%error, "could not save commit messages");
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
