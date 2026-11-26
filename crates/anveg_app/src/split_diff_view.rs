//! The split (side-by-side) diff of one file in the Git panel: before on
//! the left (read-only), after on the right. Changed lines are tinted
//! (green added, red removed, blue changed), and what changed inside a
//! changed line is tinted more strongly. A middle strip links each change
//! on the left to its place on the right and holds its buttons.
//!
//! Which versions, following git:
//! - **Changes** (unstaged): the staged version on the left, the working
//!   file on the right. The right side is editable and saved automatically
//!   (half a second after typing stops). Per change: **Revert** (puts the
//!   left side's lines back, undoable with ⌘Z) and **Stage** (writes just
//!   that change into the index).
//! - **Staged**: the last commit on the left, the staged version on the
//!   right, both read-only. Per change: **Unstage**.
//! - **New** (untracked): nothing on the left; Stage takes a change in.
//!
//! The highlights are painted over the editors as translucent tints, after
//! the editors have laid out the frame, so they never lag while scrolling.
//! Scrolling one side scrolls the other to the matching place.
//!
//! If the file changes on disk (Claude, a formatter) while there are no
//! unsaved edits here, the right side reloads. With unsaved edits, nothing
//! is overwritten: a bar offers to reload or to keep these edits.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use anveg_core::fswatch;
use anveg_core::git::DiffKind;
use anveg_core::side_diff::{self, Hunk};
use futures::StreamExt as _;
use gpui_cn::prelude::*;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Editor, EditorState, InputEvent as EditorEvent};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, EventEmitter, FocusHandle, Focusable, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, PathBuilder, Pixels, Render,
    SharedString, Styled as _, Subscription, Task, Window,
    canvas, div, hsla, point, px,
};

use crate::model::model;
use crate::ui::Colors;
use crate::workspace::WorkspaceEvent;

/// Files bigger than this open in the unified view instead.
const MAX_BYTES: usize = 4 * 1024 * 1024;
/// Autosave this long after the last edit.
const SAVE_DELAY: Duration = Duration::from_millis(500);
/// The middle strip.
const GUTTER: f32 = 64.0;

/// Where the lines are on screen, from the last frame (for the buttons in
/// the middle strip, which are laid out before the editors paint).
#[derive(Clone, Copy, Default)]
struct PaneGeometry {
    /// Top of line `row`, in window coordinates.
    y: f32,
    row: usize,
    line_height: f32,
    ready: bool,
}

impl PaneGeometry {
    fn line_y(&self, line: usize) -> f32 {
        self.y + (line as f32 - self.row as f32) * self.line_height
    }
}

/// One side's text and where its lines start.
#[derive(Default)]
struct Side {
    text: String,
    starts: Vec<usize>,
    /// The original used CRLF line endings (shown and diffed as LF).
    crlf: bool,
}

impl Side {
    fn new(text: String, crlf: bool) -> Self {
        let starts = side_diff::line_starts(&text);
        Self { text, starts, crlf }
    }

    fn set(&mut self, text: String) {
        self.starts = side_diff::line_starts(&text);
        self.text = text;
    }

    fn encoded(&self) -> Vec<u8> {
        if self.crlf {
            self.text.replace('\n', "\r\n").into_bytes()
        } else {
            self.text.clone().into_bytes()
        }
    }
}

struct Loaded {
    left: (String, bool),
    right: (String, bool),
    /// The working file exists (else the right side is read-only).
    right_exists: bool,
}

enum LoadError {
    Binary,
    TooLarge,
    Failed(String),
}

pub struct SplitDiffView {
    pub repo: PathBuf,
    pub path: String,
    pub kind: DiffKind,
    left_state: Option<Entity<EditorState>>,
    right_state: Option<Entity<EditorState>>,
    left: Side,
    right: Side,
    hunks: Rc<Vec<Hunk>>,
    editable: bool,
    /// The working file as last read or written by this view.
    disk_text: String,
    /// The file changed on disk while there were unsaved edits here.
    conflict: bool,
    error: Option<SharedString>,
    loading: bool,
    busy: bool,
    /// Scroll positions seen last frame, to tell which side the user
    /// scrolled.
    last_scroll: Cell<(f32, f32)>,
    left_geom: Rc<Cell<PaneGeometry>>,
    right_geom: Rc<Cell<PaneGeometry>>,
    gutter_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    current: usize,
    focus: FocusHandle,
    save_task: Option<Task<()>>,
    _watch: Option<fswatch::Subscription>,
    _subs: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<WorkspaceEvent> for SplitDiffView {}

impl Focusable for SplitDiffView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Text from bytes: `None` for binary or non-UTF-8; also whether it used
/// CRLF line endings (returned with LF).
fn decode(bytes: Vec<u8>) -> Result<(String, bool), LoadError> {
    if bytes.len() > MAX_BYTES {
        return Err(LoadError::TooLarge);
    }
    if bytes.iter().take(8000).any(|&b| b == 0) {
        return Err(LoadError::Binary);
    }
    let text = String::from_utf8(bytes).map_err(|_| LoadError::Binary)?;
    if text.contains("\r\n") {
        Ok((text.replace("\r\n", "\n"), true))
    } else {
        Ok((text, false))
    }
}

fn read_disk(path: &std::path::Path) -> Option<Result<(String, bool), LoadError>> {
    match std::fs::read(path) {
        Ok(bytes) => Some(decode(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(Err(LoadError::Failed(e.to_string()))),
    }
}

impl SplitDiffView {
    pub fn new(
        repo: PathBuf,
        path: String,
        kind: DiffKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self {
            repo,
            path,
            kind,
            left_state: None,
            right_state: None,
            left: Side::default(),
            right: Side::default(),
            hunks: Rc::new(Vec::new()),
            editable: false,
            disk_text: String::new(),
            conflict: false,
            error: None,
            loading: true,
            busy: false,
            last_scroll: Cell::new((0.0, 0.0)),
            left_geom: Rc::default(),
            right_geom: Rc::default(),
            gutter_bounds: Rc::default(),
            current: 0,
            focus: cx.focus_handle(),
            save_task: None,
            _watch: None,
            _subs: Vec::new(),
            _tasks: Vec::new(),
        };
        this.load(window, cx);
        if this.kind != DiffKind::Staged {
            this.watch(window, cx);
        }
        this
    }

    pub fn title(&self) -> String {
        let name = self.path.rsplit('/').next().unwrap_or(&self.path);
        match self.kind {
            DiffKind::Staged => format!("{name} (staged)"),
            DiffKind::Unstaged => format!("{name} (changes)"),
            DiffKind::Untracked => format!("{name} (new)"),
        }
    }

    fn file(&self) -> PathBuf {
        self.repo.join(&self.path)
    }

    fn labels(&self) -> (&'static str, &'static str) {
        match self.kind {
            DiffKind::Staged => ("Last commit", "Staged"),
            DiffKind::Unstaged => ("Staged", "Working file"),
            DiffKind::Untracked => ("Not in git", "Working file"),
        }
    }

    /// Read both sides (on open, and after git operations).
    pub fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let git = model(cx).read(cx).git();
        let (repo, path, kind) = (self.repo.clone(), self.path.clone(), self.kind);
        let file = self.file();
        let task = cx.background_spawn(async move {
            let blob = |spec: String| -> Result<(String, bool), LoadError> {
                match git.blob(&repo, &spec) {
                    Ok(Some(bytes)) => decode(bytes),
                    Ok(None) => Ok((String::new(), false)),
                    Err(e) => Err(LoadError::Failed(format!("{e:#}"))),
                }
            };
            let (left, right, right_exists) = match kind {
                DiffKind::Staged => (blob(format!("HEAD:{path}"))?, blob(format!(":{path}"))?, false),
                DiffKind::Unstaged | DiffKind::Untracked => {
                    let left = if kind == DiffKind::Untracked {
                        (String::new(), false)
                    } else {
                        blob(format!(":{path}"))?
                    };
                    match read_disk(&file) {
                        Some(r) => (left, r?, true),
                        None => (left, (String::new(), false), false),
                    }
                }
            };
            Ok::<_, LoadError>(Loaded {
                left,
                right,
                right_exists,
            })
        });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.loading = false;
                match result {
                    Ok(loaded) => this.apply_loaded(loaded, window, cx),
                    Err(LoadError::Binary) => {
                        this.error = Some("This file is binary or not UTF-8 text.".into())
                    }
                    Err(LoadError::TooLarge) => {
                        this.error = Some(
                            "This file is too large for the split view: switch to the unified view."
                                .into(),
                        )
                    }
                    Err(LoadError::Failed(e)) => this.error = Some(e.into()),
                }
                cx.notify();
            });
        }));
    }

    fn apply_loaded(&mut self, loaded: Loaded, window: &mut Window, cx: &mut Context<Self>) {
        self.error = None;
        self.editable = self.kind != DiffKind::Staged && loaded.right_exists;
        let lang = {
            let extra = model(cx).read(cx).settings.editor.file_types.clone();
            crate::languages::language_for(std::path::Path::new(&self.path), &extra)
        };
        self.left = Side::new(loaded.left.0, loaded.left.1);
        let right_text = loaded.right.0;
        if self.kind != DiffKind::Staged {
            self.disk_text = right_text.clone();
        }
        // Left: replace the text in place (keeps the scroll position).
        match &self.left_state {
            // Replaced only when it changed (setting it resets the scroll).
            Some(state) => {
                if state.read(cx).value().as_ref() != self.left.text {
                    let text = self.left.text.clone();
                    state.update(cx, |s, cx| {
                        s.set_readonly(false, cx);
                        s.set_value(text, window, cx);
                        s.set_readonly(true, cx);
                    });
                }
            }
            None => {
                let text = self.left.text.clone();
                let state = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(lang)
                        .line_number(true)
                        .searchable(true)
                        .soft_wrap(false)
                        .default_value(text)
                });
                state.update(cx, |s, cx| s.set_readonly(true, cx));
                self._subs.push(cx.observe(&state, |_, _, cx| cx.notify()));
                self.left_state = Some(state);
            }
        }
        // Right: only replaced when it differs (an edit in progress stays).
        let editable = self.editable;
        match &self.right_state {
            Some(state) => {
                if state.read(cx).value().as_ref() != right_text {
                    let text = right_text.clone();
                    state.update(cx, |s, cx| {
                        s.set_readonly(false, cx);
                        s.set_value(text, window, cx);
                    });
                }
                state.update(cx, |s, cx| s.set_readonly(!editable, cx));
            }
            None => {
                let text = right_text.clone();
                let state = cx.new(|cx| {
                    EditorState::new(window, cx)
                        .language(lang)
                        .line_number(true)
                        .searchable(true)
                        .soft_wrap(false)
                        .default_value(text)
                });
                state.update(cx, |s, cx| s.set_readonly(!editable, cx));
                self._subs.push(cx.observe(&state, |_, _, cx| cx.notify()));
                self._subs
                    .push(cx.subscribe_in(&state, window, |this, state, event, window, cx| {
                        if matches!(event, EditorEvent::Change) {
                            let text = state.read(cx).value().to_string();
                            this.right_changed(text, window, cx);
                        }
                    }));
                self.right_state = Some(state);
            }
        }
        self.right = Side::new(right_text, loaded.right.1);
        self.recompute();
        cx.emit(WorkspaceEvent::TitleChanged);
        cx.notify();
    }

    fn recompute(&mut self) {
        self.hunks = Rc::new(side_diff::hunks(&self.left.text, &self.right.text));
        if self.current >= self.hunks.len() {
            self.current = self.hunks.len().saturating_sub(1);
        }
    }

    fn right_changed(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        if text == self.right.text {
            return;
        }
        self.right.set(text);
        self.recompute();
        if self.editable {
            self.schedule_save(window, cx);
        }
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Saving and outside changes
    // -----------------------------------------------------------------

    fn schedule_save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let _ = this.update_in(cx, |this, window, cx| this.save_now(window, cx));
        }));
    }

    /// Write the right side to disk, unless the file changed underneath.
    fn save_now(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.save_task = None;
        if !self.editable || self.right.text == self.disk_text {
            return;
        }
        let file = self.file();
        let expected = self.disk_text.clone();
        let text = self.right.text.clone();
        let bytes = self.right.encoded();
        let task = cx.background_spawn(async move {
            let on_disk = match read_disk(&file) {
                Some(Ok((t, _))) => t,
                Some(Err(_)) => return Err("could not read the file".to_owned()),
                None => String::new(),
            };
            if on_disk != expected {
                return Ok(false);
            }
            anveg_core::atomic::write_bytes(&file, &bytes)
                .map(|()| true)
                .map_err(|e| format!("{e:#}"))
        });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, _, cx| {
                match result {
                    Ok(true) => {
                        this.disk_text = text;
                        // The Git panel's lists and counts.
                        cx.emit(WorkspaceEvent::GitChanged);
                    }
                    Ok(false) => this.conflict = true,
                    Err(e) => cx.emit(WorkspaceEvent::Toast(
                        format!("Could not save {}: {e}", this.path).into(),
                    )),
                }
                cx.notify();
            });
        }));
    }

    fn watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(service) = model(cx).read(cx).fswatch.clone() else {
            return;
        };
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        self._watch = Some(service.subscribe(&self.file(), false, move |_| {
            let _ = tx.unbounded_send(());
        }));
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            while rx.next().await.is_some() {
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

    /// The working file changed on disk.
    fn check_disk(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading || self.kind == DiffKind::Staged {
            return;
        }
        let file = self.file();
        let task = cx.background_spawn(async move { read_disk(&file) });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let read = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let on_disk = match read {
                    Some(Ok((t, _))) => t,
                    // Deleted, binary or unreadable now: start over.
                    _ => return this.load(window, cx),
                };
                if on_disk == this.disk_text {
                    return; // our own save
                }
                if this.right.text == this.disk_text {
                    this.load(window, cx);
                } else {
                    this.conflict = true;
                    cx.notify();
                }
            });
        }));
    }

    fn resolve_conflict(&mut self, keep_mine: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.conflict = false;
        if keep_mine {
            // Overwrite: take the disk's text as the base, then save ours.
            let file = self.file();
            if let Some(Ok((t, _))) = read_disk(&file) {
                self.disk_text = t;
            }
            self.save_now(window, cx);
        } else {
            self.disk_text = self.right.text.clone();
            self.load(window, cx);
        }
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Hunk actions
    // -----------------------------------------------------------------

    /// Put the left side's lines back on the right (undoable).
    fn revert(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(hunk), Some(state)) = (self.hunks.get(ix).cloned(), self.right_state.clone())
        else {
            return;
        };
        if !self.editable {
            return;
        }
        let range = side_diff::line_bytes(&self.right.text, hunk.right.clone());
        let with = self.left.text[side_diff::line_bytes(&self.left.text, hunk.left.clone())].to_owned();
        state.update(cx, |s, cx| {
            s.set_selected_range(range, cx);
            s.replace(with, window, cx);
        });
    }

    /// Stage (unstaged view) or unstage (staged view) one change.
    fn index_change(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(hunk) = self.hunks.get(ix).cloned() else {
            return;
        };
        let new_index = match self.kind {
            DiffKind::Staged => side_diff::revert(&self.left.text, &self.right.text, &hunk),
            DiffKind::Unstaged | DiffKind::Untracked => {
                side_diff::accept(&self.left.text, &self.right.text, &hunk)
            }
        };
        // The index keeps the line endings it had (the working file's for
        // a new file).
        let crlf = match self.kind {
            DiffKind::Staged => self.right.crlf,
            DiffKind::Unstaged => self.left.crlf,
            DiffKind::Untracked => self.right.crlf,
        };
        let bytes = if crlf {
            new_index.replace('\n', "\r\n").into_bytes()
        } else {
            new_index.into_bytes()
        };
        let git = model(cx).read(cx).git();
        let (repo, path) = (self.repo.clone(), self.path.clone());
        self.busy = true;
        let task =
            cx.background_spawn(async move { git.set_index_content(&repo, &path, &bytes) });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if let Err(e) = result {
                    cx.emit(WorkspaceEvent::Toast(format!("{e:#}").into()));
                }
                cx.emit(WorkspaceEvent::GitChanged);
                this.load(window, cx);
            });
        }));
        cx.notify();
    }

    /// Scroll both sides to change `ix`.
    fn go_to(&mut self, ix: usize, cx: &mut Context<Self>) {
        let (Some(hunk), Some(left), Some(right)) =
            (self.hunks.get(ix), &self.left_state, &self.right_state)
        else {
            return;
        };
        self.current = ix;
        let lh = right.read(cx).line_height().map_or(18.0, f32::from);
        let line = hunk.right.start.saturating_sub(3) as f32;
        let left_line = side_diff::map_line(&self.hunks, f64::from(line), false) as f32;
        let (rx, lx) = (right.read(cx).scroll_offset().x, left.read(cx).scroll_offset().x);
        right.update(cx, |s, cx| s.set_scroll_offset(point(rx, px(-line * lh)), cx));
        left.update(cx, |s, cx| s.set_scroll_offset(point(lx, px(-left_line * lh)), cx));
        self.last_scroll.set((-left_line * lh, -line * lh));
        cx.notify();
    }

    /// Keep the two sides lined up: whichever side the user scrolled leads.
    fn sync_scroll(&self, cx: &mut Context<Self>) {
        let (Some(left), Some(right)) = (&self.left_state, &self.right_state) else {
            return;
        };
        let (Some(lh), true) = (
            right.read(cx).line_height().map(f32::from),
            !self.hunks.is_empty(),
        ) else {
            return;
        };
        let lo = f32::from(left.read(cx).scroll_offset().y);
        let ro = f32::from(right.read(cx).scroll_offset().y);
        let (last_l, last_r) = self.last_scroll.get();
        let moved_l = (lo - last_l).abs() > 0.5;
        let moved_r = (ro - last_r).abs() > 0.5;
        if moved_l && !moved_r {
            let line = side_diff::map_line(&self.hunks, f64::from(-lo / lh), true) as f32;
            let target = -line * lh;
            let x = right.read(cx).scroll_offset().x;
            right.update(cx, |s, cx| s.set_scroll_offset(point(x, px(target)), cx));
            self.last_scroll.set((lo, target));
        } else if moved_r && !moved_l {
            let line = side_diff::map_line(&self.hunks, f64::from(-ro / lh), false) as f32;
            let target = -line * lh;
            let x = left.read(cx).scroll_offset().x;
            left.update(cx, |s, cx| s.set_scroll_offset(point(x, px(target)), cx));
            self.last_scroll.set((target, ro));
        } else {
            self.last_scroll.set((lo, ro));
        }
    }

    // -----------------------------------------------------------------
    // Drawing
    // -----------------------------------------------------------------

    fn render_pane(
        &self,
        state: &Entity<EditorState>,
        left_side: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let dark = cx.theme().is_dark();
        let hunks = self.hunks.clone();
        let starts = if left_side {
            self.left.starts.clone()
        } else {
            self.right.starts.clone()
        };
        let text_len = if left_side {
            self.left.text.len()
        } else {
            self.right.text.len()
        };
        let geom = if left_side {
            self.left_geom.clone()
        } else {
            self.right_geom.clone()
        };
        let overlay_state = state.clone();
        let read_only = left_side || !self.editable;
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .relative()
            .child(Editor::new(state).bordered(false).readonly(read_only).h_full())
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, (), window, cx| {
                        let state = overlay_state.read(cx);
                        let (Some(rows), Some(lh)) = (state.visible_row_range(), state.line_height())
                        else {
                            return;
                        };
                        let line_start = |line: usize| starts.get(line).copied().unwrap_or(text_len);
                        let Some(reference) =
                            state.range_to_bounds(&(line_start(rows.start)..line_start(rows.start)))
                        else {
                            return;
                        };
                        let g = PaneGeometry {
                            y: f32::from(reference.top()),
                            row: rows.start,
                            line_height: f32::from(lh),
                            ready: true,
                        };
                        geom.set(g);
                        window.with_content_mask(Some(gpui_kit::ContentMask { bounds }), |window| {
                            paint_side(&hunks, left_side, g, bounds, &rows, dark, window, |range| {
                                let (line, r) = range;
                                let start = line_start(line) + r.start;
                                let end = line_start(line) + r.end;
                                state.range_to_bounds(&(start..end))
                            });
                        });
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }

    fn render_gutter(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let gutter_bounds = self.gutter_bounds.clone();
        let (lg, rg) = (self.left_geom.get(), self.right_geom.get());
        let mut strip = div()
            .w(px(GUTTER))
            .flex_shrink_0()
            .h_full()
            .relative()
            .overflow_hidden()
            .border_l_1()
            .border_r_1()
            .border_color(c.border)
            .child(
                canvas(
                    move |bounds, _, _| gutter_bounds.set(Some(bounds)),
                    |_, (), _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            );
        if !(lg.ready && rg.ready) || self.busy {
            return strip;
        }
        let top = self.gutter_bounds.get().map_or(0.0, |b| f32::from(b.top()));
        let height = self.gutter_bounds.get().map_or(0.0, |b| f32::from(b.size.height));
        for (ix, hunk) in self.hunks.iter().enumerate() {
            // The buttons sit at the top of the change, on whichever side
            // is lower, so they line up with what they act on.
            let y = lg.line_y(hunk.left.start).max(rg.line_y(hunk.right.start)) - top;
            if y < -20.0 || y > height {
                continue;
            }
            let mut row = div()
                .absolute()
                .top(px(y))
                .left_0()
                .w_full()
                .flex()
                .justify_center()
                .gap_0p5();
            match self.kind {
                DiffKind::Unstaged | DiffKind::Untracked => {
                    if self.editable {
                        row = row.child(
                            Button::new(SharedString::from(format!("revert-{ix}")))
                                .ghost()
                                .size(ButtonSize::Xs)
                                .icon(Icon::from(IconName::Undo2))
                                .tooltip("Revert this change")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.revert(ix, window, cx)
                                })),
                        );
                    }
                    row = row.child(
                        Button::new(SharedString::from(format!("stage-{ix}")))
                            .ghost()
                            .size(ButtonSize::Xs)
                            .icon(Icon::from(IconName::Plus))
                            .tooltip("Stage this change")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.index_change(ix, window, cx)
                            })),
                    );
                }
                DiffKind::Staged => {
                    row = row.child(
                        Button::new(SharedString::from(format!("unstage-{ix}")))
                            .ghost()
                            .size(ButtonSize::Xs)
                            .icon(Icon::from(IconName::Minus))
                            .tooltip("Unstage this change")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.index_change(ix, window, cx)
                            })),
                    );
                }
            }
            strip = strip.child(row);
        }
        strip
    }

    /// The bands linking each change across the middle strip, painted last
    /// (after both editors have laid out this frame).
    fn render_links(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let dark = cx.theme().is_dark();
        let hunks = self.hunks.clone();
        let (lg, rg) = (self.left_geom.clone(), self.right_geom.clone());
        let gutter = self.gutter_bounds.clone();
        canvas(
            |_, _, _| {},
            move |_, (), window, _| {
                let (Some(g), l, r) = (gutter.get(), lg.get(), rg.get()) else {
                    return;
                };
                if !(l.ready && r.ready) {
                    return;
                }
                let (x0, x1) = (f32::from(g.left()), f32::from(g.right()));
                window.with_content_mask(Some(gpui_kit::ContentMask { bounds: g }), |window| {
                    for h in hunks.iter() {
                        let (ly0, ly1) = (l.line_y(h.left.start), l.line_y(h.left.end));
                        let (ry0, ry1) = (r.line_y(h.right.start), r.line_y(h.right.end));
                        let color = kind_color(h, dark).opacity(if dark { 0.28 } else { 0.18 });
                        let mut path = PathBuilder::fill();
                        let mid = (x0 + x1) / 2.0;
                        path.move_to(point(px(x0), px(ly0)));
                        path.cubic_bezier_to(
                            point(px(x1), px(ry0)),
                            point(px(mid), px(ly0)),
                            point(px(mid), px(ry0)),
                        );
                        path.line_to(point(px(x1), px(ry1.max(ry0 + 2.0))));
                        path.cubic_bezier_to(
                            point(px(x0), px(ly1.max(ly0 + 2.0))),
                            point(px(mid), px(ry1.max(ry0 + 2.0))),
                            point(px(mid), px(ly1.max(ly0 + 2.0))),
                        );
                        path.close();
                        if let Ok(path) = path.build() {
                            window.paint_path(path, color);
                        }
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }
}

/// Added (only on the right): green; removed (only on the left): red;
/// changed: blue.
fn kind_color(h: &Hunk, dark: bool) -> Hsla {
    let l = if dark { 0.55 } else { 0.45 };
    if h.is_insertion() {
        hsla(135. / 360., 0.55, l, 1.0)
    } else if h.is_deletion() {
        hsla(0., 0.70, l + 0.05, 1.0)
    } else {
        hsla(212. / 360., 0.75, l + 0.05, 1.0)
    }
}

#[allow(clippy::too_many_arguments)]
fn paint_side(
    hunks: &[Hunk],
    left_side: bool,
    g: PaneGeometry,
    bounds: Bounds<Pixels>,
    rows: &std::ops::Range<usize>,
    dark: bool,
    window: &mut Window,
    inline_bounds: impl Fn((usize, std::ops::Range<usize>)) -> Option<Bounds<Pixels>>,
) {
    let (line_alpha, inline_alpha) = if dark { (0.19, 0.38) } else { (0.11, 0.26) };
    for h in hunks {
        let lines = if left_side { &h.left } else { &h.right };
        let color = kind_color(h, dark);
        // A pure insertion or deletion leaves a gap on this side: a line.
        if lines.is_empty() {
            let y = g.line_y(lines.start);
            if lines.start + 1 < rows.start || lines.start > rows.end + 1 {
                continue;
            }
            window.paint_quad(gpui_kit::fill(
                Bounds::new(
                    point(bounds.left(), px(y - 1.0)),
                    gpui_kit::size(bounds.size.width, px(2.0)),
                ),
                color.opacity(0.7),
            ));
            continue;
        }
        if lines.end < rows.start || lines.start > rows.end {
            continue;
        }
        let (y0, y1) = (g.line_y(lines.start), g.line_y(lines.end));
        window.paint_quad(gpui_kit::fill(
            Bounds::new(
                point(bounds.left(), px(y0)),
                gpui_kit::size(bounds.size.width, px(y1 - y0)),
            ),
            color.opacity(line_alpha),
        ));
        let inline = if left_side { &h.left_inline } else { &h.right_inline };
        for (line, range) in inline {
            if *line < rows.start || *line > rows.end {
                continue;
            }
            if let Some(b) = inline_bounds((*line, range.clone())) {
                window.paint_quad(gpui_kit::fill(b, color.opacity(inline_alpha)));
            }
        }
    }
}

impl Render for SplitDiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        self.sync_scroll(cx);
        let (left_label, right_label) = self.labels();
        let count = self.hunks.len();
        let current = self.current;
        let header = div()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .border_b_1()
            .border_color(c.border)
            .text_sm()
            .child(div().text_color(c.muted_foreground).child(self.path.clone()))
            .child(div().flex_1())
            .child(div().text_xs().text_color(c.muted_foreground).child(if count == 0 {
                "No differences".to_owned()
            } else if count == 1 {
                "1 change".to_owned()
            } else {
                format!("{count} changes")
            }))
            .child(
                Button::new("prev-change")
                    .ghost()
                    .size(ButtonSize::Sm)
                    .icon(Icon::from(IconName::ChevronUp))
                    .tooltip("Previous change")
                    .disabled(count == 0)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let n = this.hunks.len();
                        if n > 0 {
                            this.go_to((current + n - 1) % n, cx);
                        }
                    })),
            )
            .child(
                Button::new("next-change")
                    .ghost()
                    .size(ButtonSize::Sm)
                    .icon(Icon::from(IconName::ChevronDown))
                    .tooltip("Next change")
                    .disabled(count == 0)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let n = this.hunks.len();
                        if n > 0 {
                            this.go_to((current + 1) % n, cx);
                        }
                    })),
            )
            .child(
                Button::new("to-unified")
                    .ghost()
                    .size(ButtonSize::Sm)
                    .label("Unified")
                    .tooltip("Show as one column (for every diff)")
                    .on_click(cx.listener(|_, _, _, cx| {
                        cx.emit(WorkspaceEvent::SetDiffLayout { split: false })
                    })),
            );
        let side_labels = div()
            .flex()
            .flex_shrink_0()
            .text_xs()
            .text_color(c.muted_foreground)
            .border_b_1()
            .border_color(c.border)
            .child(div().flex_1().px_3().py_0p5().child(format!("{left_label} (read-only)")))
            .child(div().w(px(GUTTER)))
            .child(div().flex_1().px_3().py_0p5().child(if self.editable {
                format!("{right_label} (saved automatically)")
            } else {
                format!("{right_label} (read-only)")
            }));
        let conflict = self.conflict.then(|| {
            div()
                .flex()
                .flex_shrink_0()
                .items_center()
                .gap_2()
                .px_3()
                .py_1p5()
                .bg(c.destructive.opacity(0.12))
                .text_sm()
                .child(div().flex_1().child(
                    "The file changed on disk while you were editing it here. Your edits here are not saved yet.",
                ))
                .child(
                    Button::new("conflict-reload")
                        .outline()
                        .size(ButtonSize::Sm)
                        .label("Reload (drop my edits)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.resolve_conflict(false, window, cx)
                        })),
                )
                .child(
                    Button::new("conflict-keep")
                        .outline()
                        .size(ButtonSize::Sm)
                        .label("Keep mine (overwrite)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.resolve_conflict(true, window, cx)
                        })),
                )
        });
        let body = match (&self.error, &self.left_state, &self.right_state) {
            (Some(e), _, _) => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(c.muted_foreground)
                .child(e.clone())
                .into_any_element(),
            (None, Some(l), Some(r)) => div()
                .flex_1()
                .min_h_0()
                .relative()
                .flex()
                .child(self.render_pane(l, true, cx))
                .child(self.render_gutter(cx))
                .child(self.render_pane(r, false, cx))
                .child(self.render_links(cx))
                .into_any_element(),
            _ => div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(c.muted_foreground)
                .child(if self.loading { "Loading…" } else { "" })
                .into_any_element(),
        };
        div()
            .id("split-diff")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(header)
            .child(side_labels)
            .children(conflict)
            .child(body)
    }
}

/// Closing the tab within half a second of an edit: save that edit too (on
/// a background thread), unless the file changed underneath.
impl Drop for SplitDiffView {
    fn drop(&mut self) {
        if self.save_task.is_none() || !self.editable || self.right.text == self.disk_text {
            return;
        }
        let file = self.repo.join(&self.path);
        let expected = std::mem::take(&mut self.disk_text);
        let bytes = self.right.encoded();
        let _ = std::thread::Builder::new()
            .name("diff-save".into())
            .spawn(move || {
                if matches!(read_disk(&file), Some(Ok((ref t, _))) if *t == expected) {
                    let _ = anveg_core::atomic::write_bytes(&file, &bytes);
                }
            });
    }
}
