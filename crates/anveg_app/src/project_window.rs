//! A project window: the session list on the left, the selected session's
//! Claude Code terminal (or a shell) on the right (build plan, section 6).
//!
//! This file holds the state and the session lifecycle; `sidebar.rs`,
//! `panes.rs` and `dialogs.rs` render it.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anveg_core::config::{PaneContent, Project, RightTab, SavedBounds, WindowState};
use anveg_core::launcher::{self, ClaudeOptions, LaunchSpec, SessionStart};
use anveg_core::session_index::{SessionIndex, SessionInfo, trash_session_files};
use anveg_core::status::{self, SessionStatus, StatusEvent, TERMINAL_ID_ENV};
use anveg_core::time::now_millis;
use anveg_core::watcher::DirWatcher;
use anveg_term::{ExitInfo, TerminalView, TerminalViewEvent};
use futures::StreamExt as _;
use gpui_cn::{InputEvent, InputState, MenuState};
use gpui_kit::base::{ResizablePanelEvent, ResizableState};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, FocusHandle, Focusable, SharedString,
    Subscription, Task, UniformListScrollHandle, Window, WindowBounds, px,
};
use uuid::Uuid;

use crate::model::{AppModel, ModelEvent, model};

/// Which tab the main pane shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Claude,
    Terminal(usize),
}

/// How a terminal's program ended, for the overlay.
#[derive(Clone, Debug)]
pub enum Ended {
    Exited(ExitInfo),
    Failed(SharedString),
    Stopped,
}

/// A `claude` process the app started.
pub struct Running {
    pub view: Entity<TerminalView>,
    pub terminal_id: String,
    pub status: SessionStatus,
    pub ended: Option<Ended>,
    /// Started as a new session by the app (removed from the list if it
    /// exits before writing a transcript).
    pub new_unsaved: bool,
    /// A `/rename` waiting for the session to become idle.
    pub pending_rename: Option<String>,
    /// The worktree it runs in, if any.
    pub worktree: Option<String>,
    pub memory: Option<u64>,
    idle_timer: Option<Task<()>>,
    _events: Subscription,
}

impl Running {
    pub fn is_live(&self) -> bool {
        self.ended.is_none()
    }
}

/// A terminal tab running a shell.
pub struct TermSlot {
    pub view: Option<Entity<TerminalView>>,
    pub ended: Option<Ended>,
    pub title: SharedString,
    _events: Option<Subscription>,
}

impl TermSlot {
    fn empty() -> Self {
        Self {
            view: None,
            ended: None,
            title: SharedString::default(),
            _events: None,
        }
    }
}

/// One row of the sidebar.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    /// A worktree group header.
    Header {
        worktree: String,
        count: usize,
        collapsed: bool,
    },
    /// `visible[index]`.
    Session(usize),
}

/// A modal dialog in the window.
pub enum DialogKind {
    NewSession {
        name: Entity<InputState>,
        worktree: Option<Entity<InputState>>,
        error: Option<SharedString>,
    },
    Rename {
        session_id: String,
        input: Entity<InputState>,
        error: Option<SharedString>,
    },
    Delete {
        ids: Vec<String>,
        names: Vec<String>,
    },
    ConfirmClose {
        names: Vec<String>,
        quit: bool,
        dont_ask: bool,
    },
    OpenElsewhere {
        session_id: String,
    },
    ProjectSettings {
        name: Entity<InputState>,
        error: Option<SharedString>,
    },
    Message {
        title: SharedString,
        body: SharedString,
    },
}

/// Dragging a session from the sidebar onto a pane (Phase 3).
#[derive(Clone, Debug)]
pub struct DraggedSession {
    pub id: String,
    pub name: SharedString,
}

pub struct ProjectWindow {
    pub project_id: Uuid,
    pub model: Entity<AppModel>,
    index: Arc<Mutex<SessionIndex>>,
    watcher: Option<DirWatcher>,
    _refresh_tx: futures::channel::mpsc::UnboundedSender<()>,
    refreshing: bool,
    refresh_again: bool,
    /// From the index (interactive filter applied).
    pub sessions: Vec<SessionInfo>,
    /// New sessions not on disk yet.
    pub pending_new: Vec<SessionInfo>,
    /// What the sidebar shows, after the filter, in display order.
    pub visible: Vec<SessionInfo>,
    pub rows: Vec<Row>,
    pub collapsed: BTreeSet<String>,
    pub filter: Entity<InputState>,
    pub list_focus: FocusHandle,
    pub list_scroll: UniformListScrollHandle,
    pub selected: Option<String>,
    pub multi: BTreeSet<String>,
    anchor: Option<String>,
    pub running: HashMap<String, Running>,
    /// Sessions whose resume is in flight (checking for other processes).
    pub starting: BTreeSet<String>,
    pub tab: Tab,
    last_terminal: usize,
    pub terminals: Vec<TermSlot>,
    pub split: Option<PaneContent>,
    pub sidebar_state: Entity<ResizableState>,
    pub split_state: Entity<ResizableState>,
    pub sidebar_width: f32,
    pub split_ratio: f32,
    bounds: Option<SavedBounds>,
    pub dialog: Option<DialogKind>,
    pub toast: Option<SharedString>,
    toast_task: Option<Task<()>>,
    pub context_menu: Entity<MenuState>,
    close_confirmed: bool,
    pending_resume: Vec<String>,
    memory_task: Option<Task<()>>,
    dialog_subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ProjectWindow {
    pub fn new(
        project_id: Uuid,
        saved: Option<WindowState>,
        resume: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let model = model(cx);
        let (main_folder, paths, config_dir) = {
            let m = model.read(cx);
            let folder = m
                .projects
                .get(project_id)
                .map(|p| p.main_folder.clone())
                .unwrap_or_default();
            (folder, m.paths.clone(), m.config_dir())
        };
        let index = Arc::new(Mutex::new(SessionIndex::load(
            config_dir,
            main_folder,
            paths.sessions_dir().join(format!("{project_id}.json")),
        )));

        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter sessions"));
        let mut subscriptions = vec![cx.subscribe_in(&filter, window, |this, _, event, _, cx| {
            if matches!(event, InputEvent::Change) {
                this.rebuild_rows(cx);
            }
        })];
        subscriptions.push(cx.subscribe_in(&model, window, Self::on_model_event));
        let context_menu = cx.new(MenuState::new);
        subscriptions.push(cx.observe(&context_menu, |_, _, cx| cx.notify()));

        let saved = saved.unwrap_or_else(|| WindowState::new(project_id));
        let sidebar_state = cx.new(|_| ResizableState::default());
        let split_state = cx.new(|_| ResizableState::default());
        subscriptions.push(cx.subscribe(
            &sidebar_state,
            |this, state, _: &ResizablePanelEvent, cx| {
                if let Some(width) = state.read(cx).sizes().first() {
                    this.sidebar_width = f32::from(*width);
                    this.save_soon(cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe(
            &split_state,
            |this, state, _: &ResizablePanelEvent, cx| {
                let sizes = state.read(cx).sizes().clone();
                if sizes.len() == 2 {
                    let total = f32::from(sizes[0]) + f32::from(sizes[1]);
                    if total > 0.0 {
                        this.split_ratio = (f32::from(sizes[0]) / total).clamp(0.2, 0.8);
                        this.save_soon(cx);
                    }
                }
            },
        ));
        subscriptions.push(cx.observe_window_bounds(window, |this, window, cx| {
            this.bounds = Some(saved_bounds(window));
            this.save_soon(cx);
        }));
        subscriptions.push(cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.restart_memory_sampling(cx);
            } else {
                this.memory_task = None;
            }
        }));

        // Refreshes: a channel the watcher pokes, drained on the UI thread.
        let (refresh_tx, mut refresh_rx) = futures::channel::mpsc::unbounded::<()>();
        let refresh_loop = cx.spawn(async move |this, cx| {
            while refresh_rx.next().await.is_some() {
                if this.update(cx, |this, cx| this.refresh(cx)).is_err() {
                    break;
                }
            }
        });
        let watcher = {
            let tx = refresh_tx.clone();
            match DirWatcher::new(move || {
                let _ = tx.unbounded_send(());
            }) {
                Ok(w) => Some(w),
                Err(error) => {
                    tracing::error!(%error, "could not start the file watcher; the list will not update live");
                    None
                }
            }
        };

        // Closing the window: confirm, then stop everything.
        let entity = cx.entity().downgrade();
        window.on_window_should_close(cx, move |window, cx| {
            entity
                .update(cx, |this, cx| this.should_close(window, cx))
                .unwrap_or(true)
        });
        cx.on_release(|this: &mut Self, cx| {
            this.stop_all_released(cx);
            let id = this.project_id;
            let model = this.model.clone();
            cx.defer(move |cx| model.update(cx, |m, cx| m.window_closed(id, cx)));
        })
        .detach();

        let terminal_count = saved.terminal_tabs.clamp(1, 12);
        let tab = match saved.active_tab {
            RightTab::Claude => Tab::Claude,
            RightTab::Terminal => Tab::Terminal(0),
        };
        let mut this = Self {
            project_id,
            model,
            index,
            watcher,
            _refresh_tx: refresh_tx,
            refreshing: false,
            refresh_again: false,
            sessions: Vec::new(),
            pending_new: Vec::new(),
            visible: Vec::new(),
            rows: Vec::new(),
            collapsed: BTreeSet::new(),
            filter,
            list_focus: cx.focus_handle(),
            list_scroll: UniformListScrollHandle::new(),
            selected: saved.selected_session.clone(),
            multi: BTreeSet::new(),
            anchor: None,
            running: HashMap::new(),
            starting: BTreeSet::new(),
            tab,
            last_terminal: 0,
            terminals: (0..terminal_count).map(|_| TermSlot::empty()).collect(),
            split: saved.split.clone(),
            sidebar_state,
            split_state,
            sidebar_width: saved.sidebar_width.clamp(180.0, 520.0),
            split_ratio: saved.split_ratio.clamp(0.2, 0.8),
            bounds: Some(saved_bounds(window)),
            dialog: None,
            toast: None,
            toast_task: None,
            context_menu,
            close_confirmed: false,
            pending_resume: resume,
            memory_task: None,
            dialog_subscriptions: Vec::new(),
            _tasks: vec![refresh_loop],
            _subscriptions: subscriptions,
        };
        this.update_title(window, cx);
        this.refresh(cx);
        if let Tab::Terminal(i) = this.tab {
            this.show_terminal(i, window, cx);
        }
        this.try_pending_resume(window, cx);
        this.focus_main(window, cx);
        this
    }

    // -----------------------------------------------------------------
    // Project
    // -----------------------------------------------------------------

    pub fn project(&self, cx: &App) -> Option<Project> {
        self.model.read(cx).projects.get(self.project_id).cloned()
    }

    pub fn project_name(&self, cx: &App) -> String {
        self.project(cx)
            .map(|p| p.name)
            .unwrap_or_else(|| "Project".into())
    }

    pub fn folder_missing(&self, cx: &App) -> bool {
        self.project(cx).is_some_and(|p| !p.folder_exists())
    }

    pub fn is_git_repo(&self, cx: &App) -> bool {
        self.project(cx)
            .is_some_and(|p| p.main_folder.join(".git").exists())
    }

    fn update_title(&self, window: &mut Window, cx: &App) {
        window.set_window_title(&self.project_name(cx));
    }

    fn on_model_event(
        &mut self,
        _: &Entity<AppModel>,
        event: &ModelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            ModelEvent::ClaudeDetected => {
                let config_dir = self.model.read(cx).config_dir();
                if let Ok(mut index) = self.index.lock() {
                    index.set_config_dir(config_dir);
                }
                self.refresh(cx);
                self.try_pending_resume(window, cx);
                cx.notify();
            }
            ModelEvent::SettingsChanged => {
                let settings = self.model.read(cx).terminal_settings();
                for running in self.running.values() {
                    running
                        .view
                        .update(cx, |v, cx| v.apply_settings(&settings, cx));
                }
                for slot in &self.terminals {
                    if let Some(view) = &slot.view {
                        view.update(cx, |v, cx| v.apply_settings(&settings, cx));
                    }
                }
                self.rebuild_from_index(cx);
                self.restart_memory_sampling(cx);
                self.reschedule_all_idle(cx);
            }
            ModelEvent::ProjectsChanged => {
                self.update_title(window, cx);
                cx.notify();
            }
            ModelEvent::Hook(message) => self.on_hook(message, window, cx),
            ModelEvent::AttentionChanged => {}
        }
    }

    // -----------------------------------------------------------------
    // The session list
    // -----------------------------------------------------------------

    /// Re-scan transcripts on a background thread.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.refreshing {
            self.refresh_again = true;
            return;
        }
        self.refreshing = true;
        let index = self.index.clone();
        let show_all = self.model.read(cx).settings.show_noninteractive;
        let task = cx.background_spawn(async move {
            let Ok(mut index) = index.lock() else {
                return (Vec::new(), Vec::new());
            };
            index.refresh();
            if let Err(error) = index.save_if_dirty() {
                tracing::warn!(%error, "could not save the sessions cache");
            }
            (index.sessions(show_all), index.watch_dirs())
        });
        cx.spawn(async move |this, cx| {
            let (sessions, dirs) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.refreshing = false;
                if let Some(watcher) = &mut this.watcher {
                    watcher.set_dirs(&dirs);
                }
                this.apply_sessions(sessions, cx);
                if std::mem::take(&mut this.refresh_again) {
                    this.refresh(cx);
                }
            });
        })
        .detach();
    }

    /// Rebuild from the index without re-reading files (aliases, settings).
    fn rebuild_from_index(&mut self, cx: &mut Context<Self>) {
        let show_all = self.model.read(cx).settings.show_noninteractive;
        let sessions = self
            .index
            .lock()
            .map(|i| i.sessions(show_all))
            .unwrap_or_default();
        self.apply_sessions(sessions, cx);
    }

    fn apply_sessions(&mut self, sessions: Vec<SessionInfo>, cx: &mut Context<Self>) {
        // Placeholders whose transcript appeared are dropped.
        self.pending_new
            .retain(|p| !sessions.iter().any(|s| s.id == p.id));
        self.sessions = sessions;
        self.rebuild_rows(cx);
    }

    /// All sessions: placeholders first, then the index's.
    pub fn all_sessions(&self) -> impl Iterator<Item = &SessionInfo> {
        self.pending_new.iter().chain(self.sessions.iter())
    }

    pub fn session(&self, id: &str) -> Option<&SessionInfo> {
        self.all_sessions().find(|s| s.id == id)
    }

    pub fn session_name(&self, id: &str) -> String {
        self.session(id)
            .map(|s| s.display_name.clone())
            .unwrap_or_else(|| id.to_owned())
    }

    /// Apply the filter and group by worktree.
    pub fn rebuild_rows(&mut self, cx: &mut Context<Self>) {
        let query = self.filter.read(cx).value().trim().to_lowercase();
        let mut main: Vec<SessionInfo> = Vec::new();
        let mut groups: std::collections::BTreeMap<String, Vec<SessionInfo>> = Default::default();
        for s in self.all_sessions().filter(|s| s.matches(&query)) {
            match &s.worktree {
                None => main.push(s.clone()),
                Some(w) => groups.entry(w.clone()).or_default().push(s.clone()),
            }
        }
        let mut visible = main;
        let mut rows: Vec<Row> = (0..visible.len()).map(Row::Session).collect();
        for (worktree, list) in groups {
            let collapsed = self.collapsed.contains(&worktree) && query.is_empty();
            rows.push(Row::Header {
                worktree: worktree.clone(),
                count: list.len(),
                collapsed,
            });
            for s in list {
                if !collapsed {
                    rows.push(Row::Session(visible.len()));
                }
                visible.push(s);
            }
        }
        self.visible = visible;
        self.rows = rows;
        cx.notify();
    }

    pub fn toggle_group(&mut self, worktree: &str, cx: &mut Context<Self>) {
        if !self.collapsed.remove(worktree) {
            self.collapsed.insert(worktree.to_owned());
        }
        self.rebuild_rows(cx);
    }

    /// The sessions in sidebar order that are not in a collapsed group.
    fn ordered_ids(&self) -> Vec<String> {
        self.rows
            .iter()
            .filter_map(|r| match r {
                Row::Session(i) => self.visible.get(*i).map(|s| s.id.clone()),
                Row::Header { .. } => None,
            })
            .collect()
    }

    // -----------------------------------------------------------------
    // Selection
    // -----------------------------------------------------------------

    /// A plain click: select and open.
    pub fn click_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.multi.clear();
        self.multi.insert(id.to_owned());
        self.anchor = Some(id.to_owned());
        self.open_session(id, window, cx);
    }

    /// ⌘-click: toggle in the multi-selection.
    pub fn toggle_select(&mut self, id: &str, cx: &mut Context<Self>) {
        if self.multi.is_empty()
            && let Some(sel) = &self.selected
        {
            self.multi.insert(sel.clone());
        }
        if !self.multi.remove(id) {
            self.multi.insert(id.to_owned());
        }
        self.anchor = Some(id.to_owned());
        cx.notify();
    }

    /// ⇧-click: select the range from the anchor.
    pub fn range_select(&mut self, id: &str, cx: &mut Context<Self>) {
        let ids = self.ordered_ids();
        let anchor = self.anchor.clone().or_else(|| self.selected.clone());
        let (Some(a), Some(b)) = (
            anchor.and_then(|a| ids.iter().position(|x| *x == a)),
            ids.iter().position(|x| x == id),
        ) else {
            self.toggle_select(id, cx);
            return;
        };
        let (lo, hi) = (a.min(b), a.max(b));
        self.multi = ids[lo..=hi].iter().cloned().collect();
        cx.notify();
    }

    /// The sessions an action applies to: the multi-selection when `id` is
    /// in it, else `id` alone.
    pub fn targets(&self, id: Option<&str>) -> Vec<String> {
        let id = id.map(str::to_owned).or_else(|| self.selected.clone());
        match id {
            Some(id) if self.multi.contains(&id) && self.multi.len() > 1 => self
                .ordered_ids()
                .into_iter()
                .filter(|x| self.multi.contains(x))
                .collect(),
            Some(id) => vec![id],
            None => Vec::new(),
        }
    }

    pub fn is_selected(&self, id: &str) -> bool {
        self.multi.contains(id) || (self.multi.is_empty() && self.selected.as_deref() == Some(id))
    }

    /// ⌘1–⌘9.
    pub fn open_nth(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.ordered_ids().get(n).cloned() {
            self.click_session(&id, window, cx);
        }
    }

    /// ⌘⇧] / ⌘⇧[.
    pub fn step_session(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let ids = self.ordered_ids();
        if ids.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|s| ids.iter().position(|x| x == s));
        let next = match current {
            Some(i) => (i as isize + step).rem_euclid(ids.len() as isize) as usize,
            None => 0,
        };
        let id = ids[next].clone();
        self.click_session(&id, window, cx);
        self.list_scroll
            .scroll_to_item(next, gpui_kit::ScrollStrategy::Center);
    }

    // -----------------------------------------------------------------
    // Running sessions
    // -----------------------------------------------------------------

    /// Show a session, resuming it when it is not running.
    pub fn open_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = Some(id.to_owned());
        self.tab = Tab::Claude;
        let live = self.running.get(id).is_some_and(Running::is_live);
        if live {
            self.show_current(window, cx);
            return;
        }
        self.show_current(window, cx);
        self.resume_checked(id.to_owned(), window, cx);
    }

    /// Resume after checking it is not open in another process.
    fn resume_checked(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let (claude, env) = {
            let m = self.model.read(cx);
            (m.claude_path(), m.child_env.clone())
        };
        let (Some(claude), Some(env)) = (claude, env) else {
            self.resume(id, window, cx);
            return;
        };
        if !self.starting.insert(id.clone()) {
            return;
        }
        cx.notify();
        let task = cx.background_spawn({
            let id = id.clone();
            async move {
                match anveg_core::claude_env::claude_agents(&claude, &env) {
                    Ok(agents) => agents.iter().any(|a| a.session_id == id),
                    Err(error) => {
                        tracing::info!(%error, "could not check for other processes; resuming");
                        false
                    }
                }
            }
        });
        cx.spawn_in(window, async move |this, cx| {
            let elsewhere = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.starting.remove(&id);
                if elsewhere {
                    this.dialog = Some(DialogKind::OpenElsewhere { session_id: id });
                    cx.notify();
                } else {
                    this.resume(id, window, cx);
                }
            });
        })
        .detach();
    }

    fn resume(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let worktree = self.session(&id).and_then(|s| s.worktree.clone());
        let start = SessionStart::Resume {
            session_id: id.clone(),
        };
        if self.start_claude(start, worktree, false, window, cx) {
            self.show_current(window, cx);
        }
    }

    /// Open a copy of a session (fork), from the "open elsewhere" prompt or
    /// the Fork Session menu item.
    pub fn fork_session(&mut self, from: &str, window: &mut Window, cx: &mut Context<Self>) {
        let session_id = launcher::new_session_id();
        let base = self.session(from).cloned();
        let worktree = base.as_ref().and_then(|s| s.worktree.clone());
        let name = format!("Fork of {}", self.session_name(from));
        self.add_placeholder(&session_id, &name, worktree.clone(), cx);
        // A fork copies the transcript, custom title included; an alias keeps
        // the two apart until the user renames one.
        if let Ok(mut index) = self.index.lock() {
            index.set_alias(&session_id, &name);
            let _ = index.save_if_dirty();
        }
        let start = SessionStart::Fork {
            from: from.to_owned(),
            session_id: session_id.clone(),
        };
        if self.start_claude(start, worktree, true, window, cx) {
            self.selected = Some(session_id);
            self.multi.clear();
            self.tab = Tab::Claude;
            self.show_current(window, cx);
        }
    }

    /// Start a new session (⌘N), optionally named, optionally in a new
    /// worktree.
    pub fn new_session(
        &mut self,
        name: Option<String>,
        worktree: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let session_id = launcher::new_session_id();
        let start = match &worktree {
            Some(w) => SessionStart::Worktree {
                worktree: w.clone(),
                session_id: session_id.clone(),
                name: name.clone(),
            },
            None => SessionStart::New {
                session_id: session_id.clone(),
                name: name.clone(),
            },
        };
        let display = name.clone().unwrap_or_else(|| "New session".to_owned());
        self.add_placeholder(&session_id, &display, worktree.clone(), cx);
        if let Some(name) = &name
            && let Ok(mut index) = self.index.lock()
        {
            // Claude Code gets the name with `--name`; keeping an alias too
            // shows it before the transcript exists.
            index.set_alias(&session_id, name);
            let _ = index.save_if_dirty();
        }
        // A worktree session runs in the main folder; `claude --worktree`
        // creates and enters the worktree itself.
        if self.start_claude(start, None, true, window, cx) {
            if let Some(r) = self.running.get_mut(&session_id) {
                r.worktree = worktree;
            }
            self.selected = Some(session_id);
            self.multi.clear();
            self.tab = Tab::Claude;
            self.show_current(window, cx);
        } else {
            self.pending_new.retain(|p| p.id != session_id);
            self.rebuild_rows(cx);
        }
    }

    fn add_placeholder(
        &mut self,
        id: &str,
        name: &str,
        worktree: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let transcript = self
            .index
            .lock()
            .ok()
            .and_then(|i| i.expected_transcript_path(id))
            .unwrap_or_default();
        let now = now_millis();
        self.pending_new.insert(
            0,
            SessionInfo {
                id: id.to_owned(),
                display_name: name.to_owned(),
                first_prompt: None,
                last_activity: now,
                created: Some(now),
                git_branch: None,
                non_interactive: false,
                transcript,
                worktree,
            },
        );
        self.rebuild_rows(cx);
    }

    /// Launch `claude` for `start`. `worktree` is the worktree to run in
    /// (resume/fork of a worktree session). Returns whether it started.
    fn start_claude(
        &mut self,
        start: SessionStart,
        worktree: Option<String>,
        new_unsaved: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let session_id = start.session_id().to_owned();
        let Some(project) = self.project(cx) else {
            return false;
        };
        if !project.folder_exists() {
            self.show_message(
                "Folder not found",
                format!("{} no longer exists.", project.main_folder.display()),
                cx,
            );
            return false;
        }
        let m = self.model.read(cx);
        let (Some(claude), Some(env)) = (m.claude_path(), m.child_env.clone()) else {
            if m.claude.is_none() {
                // Detection still running: try again once it finishes.
                if let SessionStart::Resume { session_id } = &start {
                    self.pending_resume.push(session_id.clone());
                }
                self.toast("Looking for Claude Code…", cx);
            } else {
                self.show_message(
                    "Claude Code not found",
                    "Install Claude Code (https://code.claude.com), or set its path in Settings ▸ Claude.",
                    cx,
                );
            }
            return false;
        };
        let default_flags = match m.settings.default_flag_args() {
            Ok(flags) => flags,
            Err(error) => {
                self.show_message(
                    "Invalid default flags",
                    format!("Settings ▸ Claude ▸ Default extra flags: {error}"),
                    cx,
                );
                return false;
            }
        };
        let options = ClaudeOptions {
            extra_folders: project.extra_folders.clone(),
            default_flags,
            hooks_settings: m.hooks.as_ref().map(|h| h.settings_file.clone()),
            hook_socket: m.hooks.as_ref().map(|h| h.socket.clone()),
        };
        let settings = m.terminal_settings();
        let cwd = match &worktree {
            Some(w) => project
                .main_folder
                .join(".claude")
                .join("worktrees")
                .join(w),
            None => project.main_folder.clone(),
        };
        let terminal_id = Uuid::new_v4().to_string();
        let spec = launcher::claude_launch(&claude, &cwd, &start, &options, &env)
            .with_env(TERMINAL_ID_ENV, terminal_id.clone());
        let view = cx.new(|cx| {
            TerminalView::spawn(
                SharedString::from(format!("claude-{terminal_id}")),
                &spec,
                &settings,
                window,
                cx,
            )
        });
        let tid = terminal_id.clone();
        let events = cx.subscribe_in(&view, window, move |this, _, event, window, cx| {
            this.on_claude_terminal_event(&tid, event, window, cx);
        });
        // Hidden until `show_current` decides it is on screen.
        view.update(cx, |v, cx| v.set_visible(false, cx));
        if let Some(old) = self.running.remove(&session_id) {
            old.view.update(cx, |v, cx| v.stop(cx));
        }
        self.running.insert(
            session_id.clone(),
            Running {
                view,
                terminal_id,
                status: status::next_status(SessionStatus::Stopped, &StatusEvent::ProcessStarted),
                ended: None,
                new_unsaved,
                pending_rename: None,
                worktree,
                memory: None,
                idle_timer: None,
                _events: events,
            },
        );
        self.save_soon(cx);
        cx.notify();
        true
    }

    fn running_key_for_terminal(&self, terminal_id: &str) -> Option<String> {
        self.running
            .iter()
            .find(|(_, r)| r.terminal_id == terminal_id)
            .map(|(k, _)| k.clone())
    }

    fn on_claude_terminal_event(
        &mut self,
        terminal_id: &str,
        event: &TerminalViewEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(key) = self.running_key_for_terminal(terminal_id) else {
            return;
        };
        let ended = match event {
            TerminalViewEvent::Exited(info) => Ended::Exited(info.clone()),
            TerminalViewEvent::Failed(message) => Ended::Failed(message.clone()),
            TerminalViewEvent::TitleChanged(_) => return,
        };
        self.mark_ended(&key, ended, window, cx);
    }

    fn mark_ended(&mut self, key: &str, ended: Ended, window: &mut Window, cx: &mut Context<Self>) {
        let Some(running) = self.running.get_mut(key) else {
            return;
        };
        if running.ended.is_some() {
            return;
        }
        let was_new = running.new_unsaved;
        let worktree = running.worktree.clone();
        running.ended = Some(ended);
        running.status = SessionStatus::Stopped;
        running.idle_timer = None;
        running.pending_rename = None;
        // A new session that exited before writing anything disappears.
        let transcript_exists = self
            .index
            .lock()
            .ok()
            .and_then(|i| {
                i.transcript_path(key)
                    .or_else(|| i.expected_transcript_path(key))
            })
            .is_some_and(|p| p.exists());
        if was_new && !transcript_exists && worktree.is_none() {
            self.running.remove(key);
            self.pending_new.retain(|p| p.id != key);
            if self.selected.as_deref() == Some(key) {
                self.selected = None;
            }
            self.multi.remove(key);
            self.rebuild_rows(cx);
        } else {
            self.refresh(cx);
        }
        self.after_status_change(cx);
        self.show_current(window, cx);
    }

    /// Stop sessions (SIGHUP, then SIGTERM, then SIGKILL). They stay in
    /// the list; clicking one resumes it.
    pub fn stop_sessions(&mut self, ids: &[String], window: &mut Window, cx: &mut Context<Self>) {
        for id in ids {
            if let Some(running) = self.running.get(id)
                && running.is_live()
            {
                running.view.update(cx, |v, cx| v.stop(cx));
                self.mark_ended(id, Ended::Stopped, window, cx);
            }
        }
    }

    /// Stop every Claude session and shell in this window.
    pub fn stop_all(&mut self, cx: &mut Context<Self>) {
        for running in self.running.values_mut() {
            if running.ended.is_none() {
                running.view.update(cx, |v, cx| v.stop(cx));
                running.ended = Some(Ended::Stopped);
                running.status = SessionStatus::Stopped;
            }
        }
        for slot in &mut self.terminals {
            if let Some(view) = &slot.view {
                view.update(cx, |v, cx| v.stop(cx));
            }
        }
    }

    fn stop_all_released(&mut self, cx: &mut App) {
        for running in self.running.values() {
            running.view.update(cx, |v, cx| v.stop(cx));
        }
        for slot in &self.terminals {
            if let Some(view) = &slot.view {
                view.update(cx, |v, cx| v.stop(cx));
            }
        }
    }

    /// Names of the live Claude sessions (for the close/quit dialog).
    pub fn running_session_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .running
            .iter()
            .filter(|(_, r)| r.is_live())
            .map(|(id, _)| self.session_name(id))
            .collect();
        names.sort();
        names
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.running.get(id).is_some_and(Running::is_live)
    }

    pub fn status_of(&self, id: &str) -> Option<SessionStatus> {
        self.running
            .get(id)
            .filter(|r| r.is_live())
            .map(|r| r.status)
    }

    pub fn needs_input_count(&self) -> usize {
        self.running
            .values()
            .filter(|r| r.is_live() && r.status == SessionStatus::NeedsInput)
            .count()
    }

    fn try_pending_resume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.model.read(cx).claude.is_none() || self.pending_resume.is_empty() {
            return;
        }
        let ids = std::mem::take(&mut self.pending_resume);
        for id in ids {
            if !self.is_running(&id) {
                self.resume(id, window, cx);
            }
        }
        self.show_current(window, cx);
    }

    // -----------------------------------------------------------------
    // What is shown
    // -----------------------------------------------------------------

    /// Whether a session's terminal is on screen in this window.
    pub fn is_session_visible(&self, id: &str) -> bool {
        (self.tab == Tab::Claude && self.selected.as_deref() == Some(id))
            || self.split
                == Some(PaneContent::Session {
                    session_id: id.to_owned(),
                })
    }

    fn is_terminal_visible(&self, index: usize) -> bool {
        self.tab == Tab::Terminal(index) || self.split == Some(PaneContent::Terminal { index })
    }

    /// Apply visibility to every terminal and focus the main pane.
    pub fn show_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<String> = self.running.keys().cloned().collect();
        for id in &ids {
            let visible = self.is_session_visible(id);
            if let Some(r) = self.running.get(id) {
                r.view.update(cx, |v, cx| v.set_visible(visible, cx));
            }
        }
        for i in 0..self.terminals.len() {
            let visible = self.is_terminal_visible(i);
            if let Some(view) = &self.terminals[i].view {
                view.update(cx, |v, cx| v.set_visible(visible, cx));
            }
        }
        self.focus_main(window, cx);
        self.reschedule_all_idle(cx);
        self.save_soon(cx);
        cx.notify();
    }

    fn focus_main(&self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        let view = match self.tab {
            Tab::Claude => self
                .selected
                .as_ref()
                .and_then(|id| self.running.get(id))
                .map(|r| r.view.clone()),
            Tab::Terminal(i) => self.terminals.get(i).and_then(|t| t.view.clone()),
        };
        let handle = match view {
            Some(view) => Some(view.read(cx).focus_handle(cx)),
            // Nothing to type into: keep focus inside this window's view so
            // its shortcuts (⌘N, ⌘T, …) work.
            None => Some(self.list_focus.clone()),
        };
        if let Some(handle) = handle {
            window.focus(&handle, cx);
            // A terminal shown for the first time is not in the element
            // tree yet, and focus on a handle that is not rendered is
            // dropped; focus again once it has been drawn.
            window.on_next_frame(move |window, cx| window.focus(&handle, cx));
        }
    }

    // -----------------------------------------------------------------
    // Terminal tabs (6.4, Phase 3 item 1)
    // -----------------------------------------------------------------

    pub fn toggle_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.tab {
            Tab::Claude => {
                let i = self
                    .last_terminal
                    .min(self.terminals.len().saturating_sub(1));
                if self.terminals.is_empty() {
                    self.new_terminal(window, cx);
                } else {
                    self.show_terminal(i, window, cx);
                }
            }
            Tab::Terminal(_) => {
                self.tab = Tab::Claude;
                self.show_current(window, cx);
            }
        }
    }

    pub fn show_claude(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = Tab::Claude;
        self.show_current(window, cx);
    }

    /// Show terminal tab `i`, starting its shell the first time.
    pub fn show_terminal(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.terminals.len() {
            return;
        }
        self.tab = Tab::Terminal(i);
        self.last_terminal = i;
        if self.terminals[i].view.is_none() {
            self.start_shell(i, window, cx);
        }
        self.show_current(window, cx);
    }

    /// ⌘⇧T: a new shell tab.
    pub fn new_terminal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.terminals.push(TermSlot::empty());
        let i = self.terminals.len() - 1;
        self.show_terminal(i, window, cx);
    }

    pub fn close_terminal(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.terminals.len() {
            return;
        }
        let slot = self.terminals.remove(i);
        if let Some(view) = slot.view {
            view.update(cx, |v, cx| v.stop(cx));
        }
        // Fix indices that pointed past the removed tab.
        let fix = |idx: usize| if idx > i { idx - 1 } else { idx };
        match self.split.clone() {
            Some(PaneContent::Terminal { index }) if index == i => self.split = None,
            Some(PaneContent::Terminal { index }) => {
                self.split = Some(PaneContent::Terminal { index: fix(index) })
            }
            _ => {}
        }
        self.tab = match self.tab {
            Tab::Terminal(t) if t == i => {
                if self.terminals.is_empty() {
                    Tab::Claude
                } else {
                    Tab::Terminal(i.min(self.terminals.len() - 1))
                }
            }
            Tab::Terminal(t) => Tab::Terminal(fix(t)),
            Tab::Claude => Tab::Claude,
        };
        self.last_terminal = fix(self.last_terminal).min(self.terminals.len().saturating_sub(1));
        self.show_current(window, cx);
    }

    pub fn restart_terminal(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self.terminals.get(i).and_then(|t| t.view.clone()) {
            view.update(cx, |v, cx| v.stop(cx));
        }
        self.start_shell(i, window, cx);
        self.show_current(window, cx);
    }

    fn start_shell(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (env, settings, folder) = {
            let m = self.model.read(cx);
            let folder = m
                .projects
                .get(self.project_id)
                .map(|p| p.main_folder.clone());
            (m.child_env.clone(), m.terminal_settings(), folder)
        };
        let mut spec = LaunchSpec::login_shell(folder.filter(|f| f.is_dir()));
        if let Some(env) = &env {
            spec = spec.with_child_env(env);
        }
        let id = Uuid::new_v4();
        let view = cx.new(|cx| {
            TerminalView::spawn(
                SharedString::from(format!("shell-{id}")),
                &spec,
                &settings,
                window,
                cx,
            )
        });
        let events = cx.subscribe_in(&view, window, move |this, view, event, _, cx| {
            let Some(slot) = this
                .terminals
                .iter_mut()
                .find(|t| t.view.as_ref() == Some(view))
            else {
                return;
            };
            match event {
                TerminalViewEvent::Exited(info) => slot.ended = Some(Ended::Exited(info.clone())),
                TerminalViewEvent::Failed(m) => slot.ended = Some(Ended::Failed(m.clone())),
                TerminalViewEvent::TitleChanged(title) => slot.title = title.clone(),
            }
            cx.notify();
        });
        let slot = &mut self.terminals[i];
        slot.view = Some(view);
        slot.ended = None;
        slot._events = Some(events);
    }

    // -----------------------------------------------------------------
    // Split view (Phase 3 item 2)
    // -----------------------------------------------------------------

    pub fn toggle_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.split.is_some() {
            self.split = None;
        } else {
            // Show the "other" thing: a terminal next to Claude, or the
            // session next to a terminal.
            self.split = match self.tab {
                Tab::Claude => {
                    if self.terminals.is_empty() {
                        self.terminals.push(TermSlot::empty());
                    }
                    let i = self.last_terminal.min(self.terminals.len() - 1);
                    if self.terminals[i].view.is_none() {
                        self.start_shell(i, window, cx);
                    }
                    Some(PaneContent::Terminal { index: i })
                }
                Tab::Terminal(_) => self
                    .selected
                    .clone()
                    .map(|session_id| PaneContent::Session { session_id }),
            };
        }
        self.show_current(window, cx);
    }

    /// Show `id` in the second pane (dropped from the sidebar).
    pub fn show_in_split(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.split = Some(PaneContent::Session {
            session_id: id.to_owned(),
        });
        if !self.is_running(id) {
            self.resume_checked(id.to_owned(), window, cx);
        }
        self.show_current(window, cx);
    }

    pub fn close_split(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.split = None;
        self.show_current(window, cx);
    }

    // -----------------------------------------------------------------
    // Rename, delete, copy, reveal (6.5, 6.6)
    // -----------------------------------------------------------------

    pub fn begin_rename(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.session_name(id);
        let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let sub = cx.subscribe_in(&input, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.commit_rename(window, cx);
            }
        });
        self.push_subscription(sub);
        self.dialog = Some(DialogKind::Rename {
            session_id: id.to_owned(),
            input,
            error: None,
        });
        cx.notify();
    }

    pub fn commit_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(DialogKind::Rename {
            session_id, input, ..
        }) = &self.dialog
        else {
            return;
        };
        let session_id = session_id.clone();
        let value = input.read(cx).value().to_string();
        let name = match launcher::normalize_name(&value) {
            Ok(name) => name,
            Err(message) => {
                if let Some(DialogKind::Rename { error, .. }) = &mut self.dialog {
                    *error = Some(message.into());
                }
                cx.notify();
                return;
            }
        };
        if let Ok(mut index) = self.index.lock() {
            index.set_alias(&session_id, &name);
            if let Err(error) = index.save_if_dirty() {
                tracing::error!(%error, "could not save the alias");
            }
        }
        if let Some(p) = self.pending_new.iter_mut().find(|p| p.id == session_id) {
            p.display_name = name.clone();
        }
        // Tell Claude Code too, when it runs: now if idle, else when it
        // next becomes idle.
        if let Some(running) = self.running.get_mut(&session_id)
            && running.is_live()
        {
            if running.status == SessionStatus::Idle {
                let view = running.view.clone();
                send_rename(view, name.clone(), cx);
            } else {
                running.pending_rename = Some(name.clone());
            }
        }
        self.dialog = None;
        self.rebuild_from_index(cx);
        self.show_current(window, cx);
    }

    pub fn begin_delete(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        let ids = self.targets(id);
        if ids.is_empty() {
            return;
        }
        let names = ids.iter().map(|i| self.session_name(i)).collect();
        self.dialog = Some(DialogKind::Delete { ids, names });
        cx.notify();
    }

    pub fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(DialogKind::Delete { ids, .. }) = self.dialog.take() else {
            return;
        };
        self.stop_sessions(&ids, window, cx);
        let mut paths = Vec::new();
        for id in &ids {
            if let Some(r) = self.running.remove(id) {
                r.view.update(cx, |v, cx| v.stop(cx));
            }
            let path = self.index.lock().ok().and_then(|i| {
                i.transcript_path(id)
                    .or_else(|| i.expected_transcript_path(id))
            });
            if let Some(path) = path {
                paths.push(path);
            }
            self.pending_new.retain(|p| &p.id != id);
            self.multi.remove(id);
            if self.selected.as_deref() == Some(id.as_str()) {
                self.selected = None;
            }
            if self.split
                == Some(PaneContent::Session {
                    session_id: id.clone(),
                })
            {
                self.split = None;
            }
        }
        let index = self.index.clone();
        let task = cx.background_spawn(async move {
            let mut errors = Vec::new();
            for path in &paths {
                if let Err(error) = trash_session_files(path) {
                    errors.push(format!("{}: {error}", path.display()));
                }
            }
            if let Ok(mut index) = index.lock() {
                for id in &ids {
                    index.forget(id);
                }
                let _ = index.save_if_dirty();
            }
            errors
        });
        cx.spawn(async move |this, cx| {
            let errors = task.await;
            let _ = this.update(cx, |this, cx| {
                if !errors.is_empty() {
                    tracing::error!(?errors, "could not move transcripts to the Trash");
                    this.toast(
                        format!("Could not move to the Trash: {}", errors.join("; ")),
                        cx,
                    );
                }
                this.refresh(cx);
            });
        })
        .detach();
        self.rebuild_rows(cx);
        self.show_current(window, cx);
    }

    pub fn copy_session_id(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        let ids = self.targets(id);
        if !ids.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(ids.join("\n")));
            self.toast("Session ID copied", cx);
        }
    }

    pub fn reveal_transcript(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        let id = id.map(str::to_owned).or_else(|| self.selected.clone());
        let Some(id) = id else { return };
        let path = self.index.lock().ok().and_then(|i| i.transcript_path(&id));
        match path {
            Some(path) => cx.reveal_path(&path),
            None => self.toast("This session has no transcript yet", cx),
        }
    }

    // -----------------------------------------------------------------
    // Closing (6.7)
    // -----------------------------------------------------------------

    /// Whether the window may close now; when sessions are running (and
    /// the setting is on) this shows the confirmation and says no.
    pub fn should_close(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.close_confirmed || self.model.read(cx).quitting {
            self.stop_all(cx);
            return true;
        }
        let names = self.running_session_names();
        if names.is_empty() || !self.model.read(cx).settings.confirm_on_close {
            self.stop_all(cx);
            return true;
        }
        self.dialog = Some(DialogKind::ConfirmClose {
            names,
            quit: false,
            dont_ask: false,
        });
        cx.notify();
        false
    }

    /// Show the quit confirmation listing every running session.
    pub fn ask_quit(&mut self, names: Vec<String>, cx: &mut Context<Self>) {
        self.dialog = Some(DialogKind::ConfirmClose {
            names,
            quit: true,
            dont_ask: false,
        });
        cx.notify();
    }

    pub fn confirm_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(DialogKind::ConfirmClose { quit, dont_ask, .. }) = self.dialog.take() else {
            return;
        };
        if dont_ask {
            self.model.update(cx, |m, cx| {
                m.update_settings(cx, |s| s.confirm_on_close = false)
            });
        }
        if quit {
            // Quitting updates every window, this one included: run it
            // after this update ends.
            let model = self.model.clone();
            cx.defer(move |cx| model.update(cx, |m, cx| m.quit_now(cx)));
        } else {
            self.close_confirmed = true;
            self.stop_all(cx);
            window.remove_window();
        }
    }

    // -----------------------------------------------------------------
    // Status (Phase 2)
    // -----------------------------------------------------------------

    fn on_hook(
        &mut self,
        message: &status::HookMessage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        tracing::debug!(event = %message.event, session = %message.session_id, "hook");
        let key = message
            .terminal_id
            .as_deref()
            .and_then(|t| self.running_key_for_terminal(t))
            .or_else(|| {
                self.running
                    .contains_key(&message.session_id)
                    .then(|| message.session_id.clone())
            });
        let Some(mut key) = key else { return };
        // `/clear` (and similar) start a new session id in the same process.
        if message.event == "SessionStart"
            && !message.session_id.is_empty()
            && message.session_id != key
        {
            key = self.rekey(&key, &message.session_id, cx);
        }
        let Some(running) = self.running.get_mut(&key) else {
            return;
        };
        if !running.is_live() {
            return;
        }
        let before = running.status;
        let after = status::next_status(
            before,
            &StatusEvent::Hook {
                event: message.event.clone(),
                notification_type: message.notification_type.clone(),
            },
        );
        running.status = after;
        if after == SessionStatus::Idle
            && let Some(name) = running.pending_rename.take()
        {
            let view = running.view.clone();
            send_rename(view, name, cx);
        }
        if before != after {
            if let Some(kind) = status::announcement(before, after) {
                self.announce(&key, kind, window, cx);
            }
            self.reschedule_idle(&key, cx);
            self.after_status_change(cx);
            if after == SessionStatus::Idle {
                // A finished turn usually wrote to the transcript.
                self.refresh(cx);
            }
        }
        cx.notify();
    }

    /// Move a running entry to a new session id.
    fn rekey(&mut self, old: &str, new: &str, cx: &mut Context<Self>) -> String {
        if let Some(running) = self.running.remove(old) {
            tracing::info!(old, new, "session id changed in a running process");
            self.running.insert(new.to_owned(), running);
            if self.selected.as_deref() == Some(old) {
                self.selected = Some(new.to_owned());
            }
            if self.multi.remove(old) {
                self.multi.insert(new.to_owned());
            }
            if self.split
                == Some(PaneContent::Session {
                    session_id: old.to_owned(),
                })
            {
                self.split = Some(PaneContent::Session {
                    session_id: new.to_owned(),
                });
            }
            let placeholder_name = self.session_name(old);
            if !self.sessions.iter().any(|s| s.id == new)
                && !self.pending_new.iter().any(|p| p.id == new)
            {
                let worktree = self.running.get(new).and_then(|r| r.worktree.clone());
                self.add_placeholder(new, &placeholder_name, worktree, cx);
            }
            self.refresh(cx);
        }
        new.to_owned()
    }

    fn after_status_change(&mut self, cx: &mut Context<Self>) {
        let model = self.model.clone();
        cx.defer(move |cx| {
            model.read(cx).refresh_badge(cx);
            model.update(cx, |_, cx| cx.emit(ModelEvent::AttentionChanged));
        });
    }

    /// A notification, unless the session is on screen in the focused window.
    fn announce(
        &mut self,
        id: &str,
        kind: status::Announcement,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.model.read(cx).settings.notifications {
            return;
        }
        if window.is_window_active() && self.is_session_visible(id) {
            return;
        }
        let title = format!("{} · {}", self.project_name(cx), self.session_name(id));
        let body = match kind {
            status::Announcement::NeedsInput => "Needs your input",
            status::Announcement::Finished => "Finished",
        };
        crate::platform::notify(&title, body, self.project_id, id);
    }

    /// Idle shutdown (7.3): one timer per idle, hidden session, for its
    /// exact deadline.
    fn reschedule_idle(&mut self, id: &str, cx: &mut Context<Self>) {
        let minutes = self.model.read(cx).settings.idle_stop_minutes;
        let visible = self.is_session_visible(id);
        let Some(running) = self.running.get_mut(id) else {
            return;
        };
        let wants_timer =
            minutes > 0 && running.is_live() && running.status == SessionStatus::Idle && !visible;
        if !wants_timer {
            running.idle_timer = None;
            return;
        }
        if running.idle_timer.is_some() {
            return;
        }
        let id = id.to_owned();
        running.idle_timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_secs(u64::from(minutes) * 60))
                .await;
            let _ = this.update(cx, |this, cx| {
                let still_idle = this
                    .running
                    .get(&id)
                    .is_some_and(|r| r.is_live() && r.status == SessionStatus::Idle);
                if still_idle && !this.is_session_visible(&id) {
                    tracing::info!(session = %id, minutes, "stopping idle session");
                    if let Some(r) = this.running.get_mut(&id) {
                        r.view.update(cx, |v, cx| v.stop(cx));
                        r.ended = Some(Ended::Stopped);
                        r.status = SessionStatus::Stopped;
                        r.idle_timer = None;
                    }
                    this.after_status_change(cx);
                    cx.notify();
                }
            });
        }));
    }

    fn reschedule_all_idle(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<String> = self.running.keys().cloned().collect();
        for id in ids {
            self.reschedule_idle(&id, cx);
        }
    }

    /// Memory display (7.4): sample every 10 s while the setting is on and
    /// the window is active.
    fn restart_memory_sampling(&mut self, cx: &mut Context<Self>) {
        if !self.model.read(cx).settings.show_memory {
            self.memory_task = None;
            return;
        }
        if self.memory_task.is_some() {
            return;
        }
        self.memory_task = Some(cx.spawn(async move |this, cx| {
            let mut sampler = Some(anveg_core::procinfo::MemorySampler::new());
            loop {
                let Ok(pids) = this.update(cx, |this, cx| {
                    this.running
                        .iter()
                        .filter(|(_, r)| r.is_live())
                        .filter_map(|(id, r)| {
                            r.view.read(cx).foreground_pid(cx).map(|p| (id.clone(), p))
                        })
                        .collect::<Vec<_>>()
                }) else {
                    break;
                };
                let Some(mut s) = sampler.take() else { break };
                let roots: Vec<u32> = pids.iter().map(|(_, p)| *p).collect();
                let (s, rss) = cx
                    .background_spawn(async move {
                        let rss = s.tree_rss(&roots);
                        (s, rss)
                    })
                    .await;
                sampler = Some(s);
                let _ = this.update(cx, |this, cx| {
                    for ((id, _), bytes) in pids.iter().zip(rss) {
                        if let Some(r) = this.running.get_mut(id) {
                            r.memory = Some(bytes);
                        }
                    }
                    cx.notify();
                });
                cx.background_executor()
                    .timer(Duration::from_secs(10))
                    .await;
            }
        }));
    }

    // -----------------------------------------------------------------
    // Misc
    // -----------------------------------------------------------------

    pub fn toast(&mut self, text: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.toast = Some(text.into());
        self.toast_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs(4)).await;
            let _ = this.update(cx, |this, cx| {
                this.toast = None;
                cx.notify();
            });
        }));
        cx.notify();
    }

    pub fn show_message(
        &mut self,
        title: impl Into<SharedString>,
        body: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        self.dialog = Some(DialogKind::Message {
            title: title.into(),
            body: body.into(),
        });
        cx.notify();
    }

    /// Keep a dialog's input subscription until the dialog closes.
    pub fn push_subscription(&mut self, subscription: Subscription) {
        self.dialog_subscriptions.push(subscription);
    }

    pub fn close_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.dialog = None;
        self.dialog_subscriptions.clear();
        self.focus_main(window, cx);
        cx.notify();
    }

    pub fn save_soon(&self, cx: &mut Context<Self>) {
        let model = self.model.clone();
        cx.defer(move |cx| model.update(cx, |m, cx| m.schedule_save(cx)));
    }

    /// This window's entry in `state.json`.
    pub fn window_state(&self, _cx: &App) -> WindowState {
        let mut running: Vec<String> = self
            .running
            .iter()
            .filter(|(_, r)| r.is_live())
            .map(|(id, _)| id.clone())
            .collect();
        running.sort();
        WindowState {
            project_id: self.project_id,
            bounds: self.bounds,
            selected_session: self.selected.clone(),
            active_tab: match self.tab {
                Tab::Claude => RightTab::Claude,
                Tab::Terminal(_) => RightTab::Terminal,
            },
            sidebar_width: self.sidebar_width,
            running_sessions: running,
            terminal_tabs: self.terminals.len().max(1),
            split: self.split.clone(),
            split_ratio: self.split_ratio,
        }
    }

    /// Select a session from a notification click.
    pub fn reveal_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        window.activate_window();
        if self.session(id).is_some() || self.running.contains_key(id) {
            self.click_session(id, window, cx);
        }
    }

    /// Point the project at a moved folder (missing-folder banner).
    pub fn locate_folder(&mut self, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let canonical = match std::fs::canonicalize(&folder) {
            Ok(p) => p,
            Err(error) => {
                self.toast(format!("{}: {error}", folder.display()), cx);
                return;
            }
        };
        let id = self.project_id;
        let clash = self
            .model
            .read(cx)
            .projects
            .find_by_folder(&canonical)
            .is_some_and(|p| p.id != id);
        if clash {
            self.toast("Another project already uses that folder", cx);
            return;
        }
        self.model.update(cx, |m, cx| {
            m.update_projects(cx, |p| {
                if let Some(project) = p.get_mut(id) {
                    project.main_folder = canonical.clone();
                }
            })
        });
        let (config_dir, file) = {
            let m = self.model.read(cx);
            (
                m.config_dir(),
                m.paths.sessions_dir().join(format!("{id}.json")),
            )
        };
        self.index = Arc::new(Mutex::new(SessionIndex::load(config_dir, canonical, file)));
        self.refresh(cx);
        self.update_title(window, cx);
        cx.notify();
    }
}

/// Send `/rename <name>` and Enter to a running Claude Code. The Enter goes
/// separately, a moment later, so it is not taken as part of a paste.
fn send_rename(view: Entity<TerminalView>, name: String, cx: &mut App) {
    view.update(cx, |v, cx| v.send_text(format!("/rename {name}"), cx));
    cx.spawn(async move |cx| {
        cx.background_executor()
            .timer(Duration::from_millis(150))
            .await;
        view.update(cx, |v, cx| v.send_text("\r", cx));
    })
    .detach();
}

fn saved_bounds(window: &Window) -> SavedBounds {
    let (bounds, maximized) = match window.window_bounds() {
        WindowBounds::Windowed(b) => (b, false),
        WindowBounds::Maximized(b) | WindowBounds::Fullscreen(b) => (b, true),
    };
    SavedBounds {
        x: f32::from(bounds.origin.x),
        y: f32::from(bounds.origin.y),
        width: f32::from(bounds.size.width).max(px(400.).into()),
        height: f32::from(bounds.size.height).max(px(300.).into()),
        maximized,
    }
}
