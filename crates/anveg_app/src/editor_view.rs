//! A file open in an editor tab (next phases, 4.2–4.3).
//!
//! - gpui-component's code editor with tree-sitter highlighting, line
//!   numbers, find and replace (⌘F), go to line (⌘L), soft wrap.
//! - Line endings, the trailing newline and the indentation style are kept.
//! - Binary, non-UTF-8 and large files open read-only, with a notice.
//! - External changes: a clean buffer reloads silently (cursor and scroll
//!   kept); a dirty one shows "This file changed on disk" with Reload, Keep
//!   Mine and Compare.
//! - Language servers through [`crate::lsp_manager`].
//! - Autosave (Settings ▸ Editor, on by default): a second after typing
//!   stops, when the editor or the window loses focus, on closing the tab
//!   or window and on quitting. Never for a file changed or deleted on disk
//!   while edited here (that still asks), and never formats.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use anveg_core::config::DocKey;
use anveg_core::files::{self, Indent, LineEnding, LoadedText, ReadOnlyReason};
use anveg_core::fswatch;
use anveg_lsp::lsp_types;
use futures::StreamExt as _;
use gpui_cn::prelude::*;
use gpui_cn::{Input, InputEvent, InputState};
use gpui_kit::component::input::{
    Editor, EditorState, InputEvent as EditorEvent, Position, RopeExt as _, TabSize,
};
use gpui_kit::component::text::TextView;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};
use uuid::Uuid;

use crate::actions::*;
use crate::lsp_manager::{LspEvent, LspManager, LspProvider, OpenRequest, ServerKey, ServerStatus};
use crate::model::model;
use crate::ui::Colors;
use crate::workspace::WorkspaceEvent;

/// How long after the last edit an autosave happens.
const AUTOSAVE_DELAY: Duration = Duration::from_millis(1000);

/// The key context of an editor tab.
pub const EDITOR_CONTEXT: &str = "CodeEditorTab";

struct FileMeta {
    line_ending: LineEnding,
    trailing_newline: bool,
    indent: Indent,
    read_only: Option<ReadOnlyReason>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DiskState {
    Same,
    Changed,
    Deleted,
}

pub struct EditorView {
    pub path: PathBuf,
    project: Uuid,
    roots: Vec<PathBuf>,
    language: &'static str,
    state: Option<Entity<EditorState>>,
    error: Option<SharedString>,
    meta: Option<FileMeta>,
    /// The text as last loaded or saved (with `\n` line endings).
    saved_text: String,
    dirty: bool,
    disk: DiskState,
    preview: bool,
    soft_wrap: bool,
    lsp: Option<ServerKey>,
    lsp_status: Option<ServerStatus>,
    problems_open: bool,
    goto: Option<Entity<InputState>>,
    pending_jump: Option<(u32, Option<u32>)>,
    focus: FocusHandle,
    saving: bool,
    /// The pending autosave (restarted by every edit).
    autosave_task: Option<Task<()>>,
    _watch: Option<fswatch::Subscription>,
    _tasks: Vec<Task<()>>,
    _subs: Vec<Subscription>,
}

impl EventEmitter<WorkspaceEvent> for EditorView {}

impl Focusable for EditorView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        match &self.state {
            Some(state) if !self.preview => state.read(cx).focus_handle(cx),
            _ => self.focus.clone(),
        }
    }
}

impl EditorView {
    pub fn new(
        path: PathBuf,
        project: Uuid,
        roots: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (language, soft_wrap) = {
            let m = model(cx).read(cx);
            (
                crate::languages::language_for(&path, &m.settings.editor.file_types),
                m.settings.editor.soft_wrap,
            )
        };
        let mut this = Self {
            path,
            project,
            roots,
            language,
            state: None,
            error: None,
            meta: None,
            saved_text: String::new(),
            dirty: false,
            disk: DiskState::Same,
            preview: false,
            soft_wrap,
            lsp: None,
            lsp_status: None,
            problems_open: false,
            goto: None,
            pending_jump: None,
            focus: cx.focus_handle(),
            saving: false,
            autosave_task: None,
            _watch: None,
            _tasks: Vec::new(),
            _subs: Vec::new(),
        };
        this.load(window, cx);
        this.watch(window, cx);
        let lsp = model(cx).read(cx).lsp.clone();
        this._subs.push(cx.subscribe_in(&lsp, window, Self::on_lsp_event));
        // Switching to another app or window saves.
        this._subs.push(cx.observe_window_activation(window, |this, window, cx| {
            if !window.is_window_active() {
                this.autosave_detached(cx);
            }
        }));
        this
    }

    pub fn title(&self) -> String {
        self.path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.path.display().to_string())
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    pub fn text(&self, cx: &App) -> String {
        self.state
            .as_ref()
            .map(|s| s.read(cx).value().to_string())
            .unwrap_or_default()
    }

    fn relative_path(&self) -> String {
        files::relative_to_roots(&self.path, &self.roots)
            .unwrap_or_else(|| self.path.clone())
            .display()
            .to_string()
    }

    // -----------------------------------------------------------------
    // Loading and watching
    // -----------------------------------------------------------------

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let started = std::time::Instant::now();
        let task = cx.background_spawn(async move { files::load_text(&path) });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(loaded) => {
                    let lines = loaded.text.lines().count();
                    this.loaded(loaded, window, cx);
                    let path = this.path.display().to_string();
                    window.on_next_frame(move |_, _| {
                        tracing::info!(
                            %path,
                            lines,
                            ms = started.elapsed().as_millis() as u64,
                            "file opened (first frame)"
                        );
                    });
                }
                Err(error) => {
                    this.error = Some(format!("{error:#}").into());
                    cx.notify();
                }
            });
        }));
    }

    fn loaded(&mut self, loaded: LoadedText, window: &mut Window, cx: &mut Context<Self>) {
        let read_only = loaded.read_only.clone();
        let language = if read_only.is_some() {
            "plaintext"
        } else {
            self.language
        };
        let tab = match loaded.indent {
            Indent::Tabs => TabSize {
                tab_size: 4,
                hard_tabs: true,
            },
            Indent::Spaces(n) => TabSize {
                tab_size: n,
                hard_tabs: false,
            },
        };
        let text = loaded.text.clone();
        let soft_wrap = self.soft_wrap;
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language(language)
                .line_number(true)
                .searchable(true)
                .soft_wrap(soft_wrap)
                .tab_size(tab)
                .default_value(text)
        });
        if read_only.is_some() {
            state.update(cx, |s, cx| s.set_readonly(true, cx));
        }
        self._subs
            .push(cx.subscribe_in(&state, window, |this, state, event, _, cx| {
                if matches!(event, EditorEvent::Change) {
                    let text = state.read(cx).value().to_string();
                    this.text_changed(&text, cx);
                }
            }));
        // Leaving the editor (another tab, the terminal, the sidebar) saves.
        let focus = state.read(cx).focus_handle(cx);
        self._subs.push(cx.on_focus_out(&focus, window, |this, _, _, cx| {
            this.autosave_detached(cx);
        }));
        self.saved_text = loaded.text.clone();
        self.meta = Some(FileMeta {
            line_ending: loaded.line_ending,
            trailing_newline: loaded.trailing_newline,
            indent: loaded.indent,
            read_only,
        });
        self.state = Some(state.clone());
        if let Some((line, col)) = self.pending_jump.take() {
            self.jump_to(line, col, window, cx);
        }
        if self.meta.as_ref().is_some_and(|m| m.read_only.is_none()) {
            self.start_lsp(&loaded.text, &state, cx);
        }
        cx.emit(WorkspaceEvent::TitleChanged);
        cx.notify();
    }

    fn watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(service) = model(cx).read(cx).fswatch.clone() else {
            return;
        };
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        self._watch = Some(service.subscribe(&self.path, false, move |_| {
            let _ = tx.unbounded_send(());
        }));
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            while rx.next().await.is_some() {
                // Coalesce a burst into one check.
                while rx.try_recv().is_ok() {}
                if this
                    .update_in(cx, |this, window, cx| this.check_disk(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// The file changed on disk (or our own save landed).
    fn check_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.saving || self.state.is_none() {
            return;
        }
        let path = self.path.clone();
        let task = cx.background_spawn(async move {
            if path.exists() {
                files::load_text(&path).ok()
            } else {
                None
            }
        });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let loaded = task.await;
            let _ = this.update_in(cx, |this, window, cx| match loaded {
                None => {
                    if !this.path.exists() {
                        this.disk = DiskState::Deleted;
                        cx.notify();
                    }
                }
                Some(loaded) => {
                    if loaded.text == this.saved_text {
                        if this.disk == DiskState::Deleted {
                            this.disk = DiskState::Same;
                            cx.notify();
                        }
                        return;
                    }
                    if this.dirty {
                        this.disk = DiskState::Changed;
                        cx.notify();
                    } else {
                        this.reload_with(loaded, window, cx);
                    }
                }
            });
        }));
    }

    /// Replace the buffer with `loaded`, keeping the cursor and scroll.
    fn reload_with(&mut self, loaded: LoadedText, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.state.clone() else {
            return;
        };
        self.saved_text = loaded.text.clone();
        if let Some(meta) = &mut self.meta {
            meta.line_ending = loaded.line_ending;
            meta.trailing_newline = loaded.trailing_newline;
        }
        let text = loaded.text;
        state.update(cx, |s, cx| {
            let cursor = s.cursor_position();
            let scroll = s.scroll_offset();
            s.set_value(text.clone(), window, cx);
            s.set_cursor_position(cursor, window, cx);
            s.set_scroll_offset(scroll, cx);
        });
        self.dirty = false;
        self.disk = DiskState::Same;
        self.sync_lsp(&text, cx);
        cx.emit(WorkspaceEvent::TitleChanged);
        cx.notify();
    }

    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.path.clone();
        let task = cx.background_spawn(async move { files::load_text(&path) });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| match result {
                Ok(loaded) => this.reload_with(loaded, window, cx),
                Err(error) => {
                    cx.emit(WorkspaceEvent::Toast(format!("{error:#}").into()));
                }
            });
        }));
    }

    fn text_changed(&mut self, text: &str, cx: &mut Context<Self>) {
        let dirty = text != self.saved_text;
        if dirty != self.dirty {
            self.dirty = dirty;
            cx.emit(WorkspaceEvent::TitleChanged);
        }
        self.sync_lsp(text, cx);
        self.schedule_autosave(cx);
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Autosave
    // -----------------------------------------------------------------

    /// Whether unsaved edits here are saved without asking: autosave is on,
    /// the file is writable, and nobody changed or deleted it on disk
    /// meanwhile (that still asks).
    pub fn can_autosave(&self, cx: &App) -> bool {
        self.dirty
            && self.disk == DiskState::Same
            && self.meta.as_ref().is_some_and(|m| m.read_only.is_none())
            && model(cx).read(cx).settings.editor.autosave
    }

    /// Unsaved edits that closing should ask about.
    pub fn needs_asking(&self, cx: &App) -> bool {
        self.dirty && !self.can_autosave(cx)
    }

    /// Save a second after the last edit.
    fn schedule_autosave(&mut self, cx: &mut Context<Self>) {
        if !self.can_autosave(cx) {
            self.autosave_task = None;
            return;
        }
        self.autosave_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AUTOSAVE_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.saving {
                    // Our previous write is still landing: try again.
                    this.schedule_autosave(cx);
                } else {
                    this.autosave_detached(cx);
                }
            });
        }));
    }

    /// Save now if autosave applies. The returned write finishes even if
    /// this editor goes away (closing, quitting); await it or detach it.
    pub fn autosave_now(&mut self, cx: &mut Context<Self>) -> Option<Task<()>> {
        if !self.can_autosave(cx) {
            return None;
        }
        self.autosave_task = None;
        self.write(cx)
    }

    fn autosave_detached(&mut self, cx: &mut Context<Self>) {
        if let Some(task) = self.autosave_now(cx) {
            task.detach();
        }
    }

    // -----------------------------------------------------------------
    // Saving
    // -----------------------------------------------------------------

    /// ⌘S. With format-on-save and a running server, formats first.
    pub fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(meta) = &self.meta else {
            return;
        };
        if meta.read_only.is_some() {
            cx.emit(WorkspaceEvent::Toast("This file is read-only.".into()));
            return;
        }
        let format_first = model(cx).read(cx).settings.editor.format_on_save
            && self.lsp_status == Some(ServerStatus::Ready);
        if format_first {
            self.format(true, window, cx);
        } else if let Some(task) = self.write(cx) {
            task.detach();
        }
    }

    /// Write the buffer to disk. The returned task is the write itself: it
    /// finishes even if this view is dropped meanwhile (so closing a tab
    /// right after saving loses nothing). Detach or await it.
    fn write(&mut self, cx: &mut Context<Self>) -> Option<Task<()>> {
        let (Some(meta), Some(state)) = (&self.meta, &self.state) else {
            return None;
        };
        let text = state.read(cx).value().to_string();
        let path = self.path.clone();
        let (le, nl) = (meta.line_ending, meta.trailing_newline);
        self.saving = true;
        let body = text.clone();
        let (tx, rx) = futures::channel::oneshot::channel();
        let disk = cx.background_spawn(async move {
            let result = files::save_text(&path, &body, le, nl);
            if let Err(error) = &result {
                tracing::warn!(path = %path.display(), error = %format!("{error:#}"), "could not save");
            }
            let _ = tx.send(result);
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let Ok(result) = rx.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(()) => {
                        this.saved_text = text;
                        // Typing may have gone on while the write ran.
                        let current = this.text(cx);
                        this.dirty = current != this.saved_text;
                        this.disk = DiskState::Same;
                        if this.dirty {
                            this.schedule_autosave(cx);
                        }
                        if let Some(key) = this.lsp.clone() {
                            let path = this.path.clone();
                            model(cx)
                                .read(cx)
                                .lsp
                                .clone()
                                .update(cx, |l, _| l.saved(&key, &path));
                        }
                        cx.emit(WorkspaceEvent::TitleChanged);
                    }
                    Err(error) => {
                        cx.emit(WorkspaceEvent::Toast(
                            format!("Could not save: {error:#}").into(),
                        ));
                    }
                }
                cx.notify();
            });
        }));
        Some(disk)
    }

    // -----------------------------------------------------------------
    // Navigation
    // -----------------------------------------------------------------

    /// Put the cursor on 1-based `line` (and column).
    pub fn jump_to(&mut self, line: u32, column: Option<u32>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(state) = self.state.clone() else {
            self.pending_jump = Some((line, column));
            return;
        };
        state.update(cx, |s, cx| {
            s.set_cursor_position(
                Position::new(line.saturating_sub(1), column.unwrap_or(1).saturating_sub(1)),
                window,
                cx,
            );
            s.focus(window, cx);
        });
    }

    fn begin_goto(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Line number, or line:column"));
        self._subs
            .push(cx.subscribe_in(&input, window, |this, input, event, window, cx| {
                match event {
                    InputEvent::PressEnter { .. } => {
                        let value = input.read(cx).value().to_string();
                        let mut parts = value.trim().split(':');
                        let line = parts.next().and_then(|l| l.trim().parse::<u32>().ok());
                        let col = parts.next().and_then(|c| c.trim().parse::<u32>().ok());
                        this.goto = None;
                        if let Some(line) = line {
                            this.jump_to(line.max(1), col, window, cx);
                        }
                        cx.notify();
                    }
                    InputEvent::Blur => {
                        this.goto = None;
                        cx.notify();
                    }
                    _ => {}
                }
            }));
        let handle = input.read(cx).focus_handle(cx);
        self.goto = Some(input);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Ask Claude about the selected lines (or the cursor's line).
    fn ask_claude(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        let s = state.read(cx);
        let range = s.selected_range();
        let text = s.text();
        let start = text.offset_to_position(range.start).line + 1;
        let mut end_pos = text.offset_to_position(range.end);
        // A selection ending at the start of a line does not include it.
        if range.end > range.start && end_pos.character == 0 {
            end_pos.line = end_pos.line.saturating_sub(1);
        }
        let end = (end_pos.line + 1).max(start);
        cx.emit(WorkspaceEvent::TypeIntoClaude(mention(&self.relative_path(), start, end)));
    }

    // -----------------------------------------------------------------
    // Language server
    // -----------------------------------------------------------------

    fn start_lsp(&mut self, text: &str, state: &Entity<EditorState>, cx: &mut Context<Self>) {
        let lsp_language = crate::languages::lsp_language(self.language);
        let (command, env, watch, lsp) = {
            let m = model(cx).read(cx);
            (
                m.settings.lsp_command(lsp_language),
                m.child_env.clone(),
                m.fswatch.clone(),
                m.lsp.clone(),
            )
        };
        let Some(command) = command else {
            return;
        };
        let req = OpenRequest {
            project: self.project,
            roots: self.roots.clone(),
            command,
            env,
            language_id: anveg_lsp::language_id(lsp_language, &self.path),
            path: self.path.clone(),
            text: text.to_owned(),
        };
        let key = lsp.update(cx, |l, cx| l.open(req, watch, cx));
        self.lsp_status = lsp.read(cx).status(&key);
        let this = cx.entity().downgrade();
        let provider = Rc::new(LspProvider {
            manager: lsp.downgrade(),
            key: key.clone(),
            path: self.path.clone(),
            open_location: Rc::new(move |path, line, col, _window, cx| {
                let _ = this.update(cx, |_, cx| {
                    cx.emit(WorkspaceEvent::OpenFile {
                        path,
                        line: Some(line),
                        column: Some(col),
                    })
                });
            }),
        });
        let own_path = self.path.clone();
        let opener = provider.open_location.clone();
        state.update(cx, |s, _| {
            let l = s.lsp_mut();
            l.completion_provider = Some(provider.clone());
            l.hover_provider = Some(provider.clone());
            l.definition_provider = Some(provider.clone());
            l.show_document = Some(Rc::new(move |params: &lsp_types::ShowDocumentParams, window, cx| {
                let Some(path) = anveg_lsp::uri_to_path(&params.uri) else {
                    return false;
                };
                let (line, col) = params
                    .selection
                    .map(|r| (r.start.line + 1, r.start.character + 1))
                    .unwrap_or((1, 1));
                if path == own_path {
                    return false;
                }
                opener(path, line, col, window, cx);
                true
            }));
        });
        self.lsp = Some(key);
        self.apply_diagnostics(cx);
    }

    fn sync_lsp(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(key) = &self.lsp {
            let lsp = model(cx).read(cx).lsp.clone();
            lsp.update(cx, |l, _| l.change(key, &self.path, text));
        }
    }

    fn on_lsp_event(
        &mut self,
        lsp: &Entity<LspManager>,
        event: &LspEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            LspEvent::Diagnostics(path) if *path == self.path => self.apply_diagnostics(cx),
            LspEvent::Status(key) if Some(key) == self.lsp.as_ref() => {
                self.lsp_status = lsp.read(cx).status(key);
                if self.lsp_status == Some(ServerStatus::Ready) {
                    // The server (re)started: give it the current text.
                    let text = self.text(cx);
                    self.sync_lsp(&text, cx);
                }
                cx.notify();
            }
            _ => {}
        }
    }

    fn apply_diagnostics(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.state.clone() else {
            return;
        };
        let diags: Vec<lsp_types::Diagnostic> = model(cx)
            .read(cx)
            .lsp
            .read(cx)
            .diagnostics(&self.path)
            .to_vec();
        state.update(cx, |s, cx| {
            if let Some(set) = s.diagnostics_mut() {
                set.clear();
                set.extend(diags);
            }
            cx.notify();
        });
        cx.notify();
    }

    /// ⌥⇧F (and format-on-save, which then writes).
    fn format(&mut self, then_save: bool, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(key), Some(meta)) = (self.lsp.clone(), &self.meta) else {
            if then_save {
                if let Some(task) = self.write(cx) {
                    task.detach();
                }
            } else {
                cx.emit(WorkspaceEvent::Toast(
                    "No language server for this file.".into(),
                ));
            }
            return;
        };
        let (tab, spaces) = match meta.indent {
            Indent::Tabs => (4, false),
            Indent::Spaces(n) => (n as u32, true),
        };
        let text = self.text(cx);
        self.sync_lsp(&text, cx);
        let lsp = model(cx).read(cx).lsp.clone();
        let task = lsp.read(cx).format(&key, &self.path, tab, spaces, cx);
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(edits) => {
                        if let Some(state) = this.state.clone()
                            && !edits.is_empty()
                        {
                            state.update(cx, |s, cx| s.apply_lsp_edits(&edits, window, cx));
                        }
                    }
                    Err(error) => cx.emit(WorkspaceEvent::Toast(format!("{error:#}").into())),
                }
                if then_save && let Some(task) = this.write(cx) {
                    task.detach();
                }
            });
        }));
    }

    fn toggle_soft_wrap(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.soft_wrap = !self.soft_wrap;
        let wrap = self.soft_wrap;
        if let Some(state) = &self.state {
            state.update(cx, |s, cx| s.set_soft_wrap(wrap, window, cx));
        }
        cx.notify();
    }

    /// Called when the tab closes: release the server document.
    pub fn closed(&mut self, cx: &mut Context<Self>) {
        if let Some(key) = self.lsp.take() {
            let lsp = model(cx).read(cx).lsp.clone();
            let path = self.path.clone();
            lsp.update(cx, |l, cx| l.close(&key, &path, cx));
        }
        self._watch = None;
    }

    // -----------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------

    fn banner(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = Colors::of(cx);
        let row = |text: String| {
            div()
                .px_3()
                .py_1p5()
                .flex()
                .items_center()
                .gap_2()
                .text_sm()
                .bg(c.primary.opacity(0.12))
                .border_b_1()
                .border_color(c.border)
                .child(div().flex_1().child(text))
        };
        if let Some(meta) = &self.meta
            && let Some(reason) = &meta.read_only
        {
            let text = match reason {
                ReadOnlyReason::Binary => "This looks like a binary file. It is shown read-only.",
                ReadOnlyReason::NotUtf8 => "This file isn't UTF-8 text. It is shown read-only.",
                ReadOnlyReason::Large => {
                    "This file is very large (over 200,000 lines or 20 MB). It is shown read-only, as plain text."
                }
            };
            return Some(row(text.to_owned()).into_any_element());
        }
        match self.disk {
            DiskState::Same => None,
            DiskState::Deleted => Some(
                row("This file was deleted on disk. Save to create it again.".into())
                    .into_any_element(),
            ),
            DiskState::Changed => {
                let path = self.path.clone();
                Some(
                    row("This file changed on disk.".into())
                        .child(
                            Button::new("disk-reload")
                                .size(ButtonSize::Sm)
                                .primary()
                                .label("Reload")
                                .on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))),
                        )
                        .child(
                            Button::new("disk-keep")
                                .size(ButtonSize::Sm)
                                .outline()
                                .label("Keep Mine")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.disk = DiskState::Same;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("disk-compare")
                                .size(ButtonSize::Sm)
                                .outline()
                                .label("Compare")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(WorkspaceEvent::OpenDoc(DocKey::Compare {
                                        path: path.clone(),
                                    }))
                                })),
                        )
                        .into_any_element(),
                )
            }
        }
    }

    fn status_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let mut parts: Vec<String> = Vec::new();
        if let Some(state) = &self.state {
            let p = state.read(cx).cursor_position();
            parts.push(format!("Ln {}, Col {}", p.line + 1, p.character + 1));
        }
        if let Some(meta) = &self.meta {
            parts.push(meta.line_ending.label().to_owned());
            parts.push(match meta.indent {
                Indent::Tabs => "Tabs".to_owned(),
                Indent::Spaces(n) => format!("Spaces: {n}"),
            });
        }
        parts.push(self.language.to_owned());
        let lsp_text = match &self.lsp_status {
            Some(ServerStatus::Starting) => Some("Language server starting…".to_owned()),
            Some(ServerStatus::Ready) => None,
            Some(ServerStatus::Unavailable(why)) => Some(
                match crate::languages::install_hint(self.language) {
                    Some(hint) if why.contains("not installed") => {
                        format!("No language server ({why}): {hint}")
                    }
                    _ => format!("No language server: {why}"),
                },
            ),
            None => None,
        };
        let problems = model(cx).read(cx).lsp.read(cx).diagnostics(&self.path).len();
        let is_md = self.language == "markdown";
        div()
            .h(px(24.))
            .px_2()
            .flex()
            .items_center()
            .gap_3()
            .border_t_1()
            .border_color(c.border)
            .text_xs()
            .text_color(c.muted_foreground)
            .child(div().child(parts.join("  ·  ")))
            .children(lsp_text.map(|t| div().truncate().child(t)))
            .child(div().flex_1())
            .when(problems > 0, |el| {
                el.child(
                    div()
                        .id("problems-toggle")
                        .cursor_pointer()
                        .text_color(c.destructive)
                        .child(format!(
                            "{problems} problem{}",
                            if problems == 1 { "" } else { "s" }
                        ))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.problems_open = !this.problems_open;
                            cx.notify();
                        })),
                )
            })
            .when(is_md, |el| {
                el.child(
                    Button::new("md-preview")
                        .ghost()
                        .size(ButtonSize::Sm)
                        .label(if self.preview { "Edit" } else { "Preview" })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.preview = !this.preview;
                            cx.notify();
                        })),
                )
            })
            .child(
                Button::new("soft-wrap")
                    .ghost()
                    .size(ButtonSize::Sm)
                    .label("Wrap")
                    .toggled(self.soft_wrap)
                    .tooltip("Soft Wrap")
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_soft_wrap(window, cx))),
            )
            .child(
                Button::new("ask-claude")
                    .ghost()
                    .size(ButtonSize::Sm)
                    .label("Ask Claude")
                    .tooltip("Mention the selected lines in Claude's prompt (⌘⇧L)")
                    .on_click(cx.listener(|this, _, _, cx| this.ask_claude(cx))),
            )
            .into_any_element()
    }

    fn problems(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.problems_open {
            return None;
        }
        let c = Colors::of(cx);
        let diags = model(cx).read(cx).lsp.read(cx).diagnostics(&self.path).to_vec();
        if diags.is_empty() {
            return None;
        }
        Some(
            div()
                .id("problems")
                .max_h(px(160.))
                .overflow_y_scroll()
                .border_t_1()
                .border_color(c.border)
                .children(diags.into_iter().enumerate().map(|(i, d)| {
                    let line = d.range.start.line + 1;
                    let col = d.range.start.character + 1;
                    let severity = match d.severity {
                        Some(lsp_types::DiagnosticSeverity::ERROR) | None => "Error",
                        Some(lsp_types::DiagnosticSeverity::WARNING) => "Warning",
                        Some(_) => "Info",
                    };
                    div()
                        .id(SharedString::from(format!("problem-{i}")))
                        .px_2()
                        .py_0p5()
                        .text_xs()
                        .cursor_pointer()
                        .hover(|el| el.bg(c.accent.opacity(0.5)))
                        .child(format!("{severity} {line}:{col}  {}", d.message))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.jump_to(line, Some(col), window, cx)
                        }))
                }))
                .into_any_element(),
        )
    }
}

/// `@path#L10 ` or `@path#L10-20 `: the mention syntax Claude Code
/// (2.1.331) inserts for a selection in an IDE.
pub fn mention(relative: &str, start: u32, end: u32) -> String {
    if start == end {
        format!("@{relative}#L{start} ")
    } else {
        format!("@{relative}#L{start}-{end} ")
    }
}

impl Render for EditorView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let read_only = self
            .meta
            .as_ref()
            .is_some_and(|m| m.read_only.is_some());
        let body: AnyElement = if let Some(error) = &self.error {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(c.muted_foreground)
                .child(format!("Could not open {}: {error}", self.title()))
                .into_any_element()
        } else if let Some(state) = &self.state {
            if self.preview {
                div()
                    .id("md-preview")
                    .size_full()
                    .overflow_y_scroll()
                    .p_4()
                    .track_focus(&self.focus)
                    .child(TextView::markdown(
                        SharedString::from(format!("md-{}", self.path.display())),
                        state.read(cx).value(),
                    ))
                    .into_any_element()
            } else {
                Editor::new(state)
                    .bordered(false)
                    .readonly(read_only)
                    .h_full()
                    .into_any_element()
            }
        } else {
            div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(c.muted_foreground)
                .child("Opening…")
                .into_any_element()
        };
        let banner = self.banner(cx);
        let status = self.status_bar(cx);
        let problems = self.problems(cx);
        let goto = self.goto.clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .key_context(EDITOR_CONTEXT)
            .on_action(cx.listener(|this, _: &SaveFile, window, cx| this.save(window, cx)))
            .on_action(cx.listener(|this, _: &GoToLine, window, cx| this.begin_goto(window, cx)))
            .on_action(cx.listener(|this, _: &FormatDocument, window, cx| {
                this.format(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &AskClaudeAboutSelection, _, cx| this.ask_claude(cx)))
            .on_action(cx.listener(|this, _: &ToggleSoftWrap, window, cx| {
                this.toggle_soft_wrap(window, cx)
            }))
            .children(banner)
            .children(goto.map(|input| {
                div()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(c.border)
                    .child(Input::new(&input))
            }))
            .child(div().flex_1().min_h_0().child(body))
            .children(problems)
            .child(status)
    }
}

/// Whether two paths name the same file (for "already open" checks).
pub fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(x), Ok(y)) if x == y
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mentions() {
        assert_eq!(mention("src/a.rs", 3, 3), "@src/a.rs#L3 ");
        assert_eq!(mention("src/a.rs", 3, 9), "@src/a.rs#L3-9 ");
    }
}
