//! `DiffView` (next phases, 0.4): a unified, virtualized, syntax-highlighted
//! diff with hunk headers and per-hunk actions. Built on gpui-component's
//! readonly diff; hunk buttons are its line annotations.
//!
//! Used for git file diffs (with Stage / Unstage / Discard), commits, pull
//! requests and "Compare" (disk against an unsaved buffer).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::rc::Rc;

use anveg_core::git::{self, DiffKind, FilePatch};
use gpui_cn::prelude::*;
use gpui_kit::component::diff::{
    Diff, DiffAnnotation, DiffFile, DiffLinePosition, DiffSide, DiffState,
};
use gpui_kit::{
    App, AppContext as _, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Task, WeakEntity, Window, div, px,
};

use crate::model::model;
use crate::ui::Colors;
use crate::workspace::{ButtonStyle, Prompt, WorkspaceEvent};

/// Annotation id → (file, hunk).
type HunkMap = Rc<BTreeMap<u64, (usize, usize)>>;

/// Above this many diff lines, ask before drawing.
pub const LARGE_DIFF_LINES: usize = 5_000;

#[derive(Clone, Debug)]
pub enum DiffSource {
    /// `git diff [--cached] -- path` (or all-new for untracked files).
    GitFile {
        repo: PathBuf,
        path: String,
        kind: DiffKind,
    },
    Commit {
        repo: PathBuf,
        sha: String,
    },
    /// The file on disk against the unsaved buffer.
    Compare {
        path: PathBuf,
        relative: String,
        mine: String,
    },
    /// A ready patch (pull requests).
    Text(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HunkAction {
    Stage,
    Unstage,
    Discard,
}

pub struct DiffView {
    pub source: DiffSource,
    state: Option<Entity<DiffState>>,
    patches: Vec<FilePatch>,
    /// gpui-component's path for each patch (same order).
    paths: Vec<SharedString>,
    lines: usize,
    show_anyway: bool,
    loading: bool,
    busy: bool,
    error: Option<SharedString>,
    focus: FocusHandle,
    _task: Option<Task<()>>,
}

impl EventEmitter<WorkspaceEvent> for DiffView {}

impl Focusable for DiffView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl DiffView {
    pub fn new(source: DiffSource, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            source,
            state: None,
            patches: Vec::new(),
            paths: Vec::new(),
            lines: 0,
            show_anyway: false,
            loading: true,
            busy: false,
            error: None,
            focus: cx.focus_handle(),
            _task: None,
        };
        this.reload(window, cx);
        this
    }

    pub fn title(&self) -> String {
        match &self.source {
            DiffSource::GitFile { path, kind, .. } => {
                let name = path.rsplit('/').next().unwrap_or(path);
                match kind {
                    DiffKind::Staged => format!("{name} (staged)"),
                    DiffKind::Unstaged => format!("{name} (changes)"),
                    DiffKind::Untracked => format!("{name} (new)"),
                }
            }
            DiffSource::Commit { sha, .. } => format!("Commit {}", &sha[..sha.len().min(8)]),
            DiffSource::Compare { relative, .. } => format!("{relative} ↔ disk"),
            DiffSource::Text(_) => "Diff".into(),
        }
    }

    /// Fetch the diff again (after git operations or file changes).
    pub fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.source.clone();
        let git = model(cx).read(cx).git();
        let task = cx.background_spawn(async move {
            match source {
                DiffSource::GitFile { repo, path, kind } => {
                    git.file_diff(&repo, &path, kind).map_err(|e| format!("{e:#}"))
                }
                DiffSource::Commit { repo, sha } => {
                    git.commit_diff(&repo, &sha).map_err(|e| format!("{e:#}"))
                }
                DiffSource::Compare {
                    path,
                    relative,
                    mine,
                } => {
                    let disk = std::fs::read_to_string(&path).unwrap_or_default();
                    Ok(anveg_core::files::unified_diff(&disk, &mine, &relative))
                }
                DiffSource::Text(text) => Ok(text),
            }
        });
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, _, cx| {
                this.loading = false;
                match result {
                    Ok(text) => this.set_text(&text, cx),
                    Err(e) => this.error = Some(e.into()),
                }
                cx.notify();
            });
        }));
    }

    /// Replace the shown patch.
    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.error = None;
        self.patches = git::parse_patch(text);
        self.lines = self.patches.iter().map(FilePatch::line_count).sum();
        let extra = model(cx).read(cx).settings.editor.file_types.clone();
        let files = match DiffFile::parse(text) {
            Ok(files) => files,
            Err(e) => {
                self.error = Some(format!("Could not read the diff: {}", e.message()).into());
                self.state = None;
                return;
            }
        };
        let files: Vec<DiffFile> = files
            .into_iter()
            .map(|f| {
                let lang = crate::languages::language_for(std::path::Path::new(f.path().as_ref()), &extra);
                f.with_language(lang)
            })
            .collect();
        self.paths = files.iter().map(|f| f.path().clone()).collect();
        match &self.state {
            Some(state) => state.update(cx, |s, cx| s.set_files(files, cx)),
            None => self.state = Some(cx.new(|cx| DiffState::new(files, cx))),
        }
        cx.notify();
    }

    fn hunk_actions(&self) -> Vec<HunkAction> {
        match &self.source {
            DiffSource::GitFile {
                kind: DiffKind::Unstaged,
                ..
            } => vec![HunkAction::Stage, HunkAction::Discard],
            DiffSource::GitFile {
                kind: DiffKind::Staged,
                ..
            } => vec![HunkAction::Unstage],
            _ => Vec::new(),
        }
    }

    fn annotations(&self) -> (Vec<DiffAnnotation>, HunkMap) {
        let mut anns = Vec::new();
        let mut map = BTreeMap::new();
        if self.hunk_actions().is_empty() || self.paths.len() != self.patches.len() {
            return (anns, Rc::new(map));
        }
        for (fi, (patch, path)) in self.patches.iter().zip(&self.paths).enumerate() {
            for (hi, hunk) in patch.hunks.iter().enumerate() {
                let (new_side, line) = hunk.anchor();
                let side = if new_side {
                    DiffSide::Modified
                } else {
                    DiffSide::Original
                };
                let id = ((fi as u64) << 32) | hi as u64;
                map.insert(id, (fi, hi));
                anns.push(DiffAnnotation::line(
                    ElementId::Integer(id),
                    DiffLinePosition::new(path.clone(), side, line as usize),
                ));
            }
        }
        (anns, Rc::new(map))
    }

    fn run_hunk(&mut self, file: usize, hunk: usize, action: HunkAction, window: &mut Window, cx: &mut Context<Self>) {
        let DiffSource::GitFile { repo, path, .. } = self.source.clone() else {
            return;
        };
        let Some(patch) = self.patches.get(file).and_then(|p| p.hunk_patch(hunk)) else {
            return;
        };
        if action == HunkAction::Discard {
            let weak = cx.entity().downgrade();
            let prompt = Prompt::new(
                "Discard this change?",
                format!(
                    "The hunk is removed from {path}. A copy of the current file goes to the Trash first, so you can get it back."
                ),
            )
            .cancel()
            .button("Discard", ButtonStyle::Destructive, move |_, window, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.apply_hunk(repo.clone(), path.clone(), patch.clone(), HunkAction::Discard, window, cx)
                });
                Ok(())
            });
            cx.emit(WorkspaceEvent::Prompt(prompt));
            return;
        }
        self.apply_hunk(repo, path, patch, action, window, cx);
    }

    fn apply_hunk(
        &mut self,
        repo: PathBuf,
        path: String,
        patch: String,
        action: HunkAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let git = model(cx).read(cx).git();
        self.busy = true;
        cx.notify();
        let task = cx.background_spawn(async move {
            match action {
                HunkAction::Stage => git.apply(&repo, &patch, true, false),
                HunkAction::Unstage => git.apply(&repo, &patch, true, true),
                HunkAction::Discard => {
                    git::copy_to_trash(&repo.join(&path))?;
                    git.apply(&repo, &patch, false, true)
                }
            }
        });
        self.finish(task, window, cx);
    }

    fn file_op(&mut self, op: FileOp, window: &mut Window, cx: &mut Context<Self>) {
        let DiffSource::GitFile { repo, path, kind } = self.source.clone() else {
            return;
        };
        if op == FileOp::Discard {
            let weak = cx.entity().downgrade();
            let body = if kind == DiffKind::Untracked {
                format!("{path} is moved to the Trash.")
            } else {
                format!(
                    "Your changes to {path} are thrown away. A copy of the current file goes to the Trash first."
                )
            };
            let prompt = Prompt::new("Discard changes?", body)
                .cancel()
                .button("Discard", ButtonStyle::Destructive, move |_, window, cx| {
                    let _ = weak.update(cx, |this, cx| this.file_op(FileOp::DiscardConfirmed, window, cx));
                    Ok(())
                });
            cx.emit(WorkspaceEvent::Prompt(prompt));
            return;
        }
        let git = model(cx).read(cx).git();
        self.busy = true;
        cx.notify();
        let task = cx.background_spawn(async move {
            match op {
                FileOp::Stage => git.stage(&repo, &[path.as_str()]),
                FileOp::Unstage => git.unstage(&repo, &[path.as_str()]),
                FileOp::DiscardConfirmed if kind == DiffKind::Untracked => {
                    trash::delete(repo.join(&path)).map_err(anyhow::Error::from)
                }
                FileOp::DiscardConfirmed => {
                    git::copy_to_trash(&repo.join(&path))?;
                    git.restore_worktree(&repo, &[path.as_str()])
                }
                FileOp::Discard => Ok(()),
            }
        });
        self.finish(task, window, cx);
    }

    fn finish(&mut self, task: Task<anyhow::Result<()>>, window: &mut Window, cx: &mut Context<Self>) {
        self._task = Some(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = false;
                if let Err(e) = result {
                    cx.emit(WorkspaceEvent::Prompt(Prompt::message(
                        "Git reported a problem",
                        format!("{e:#}"),
                    )));
                }
                cx.emit(WorkspaceEvent::GitChanged);
                this.reload(window, cx);
            });
        }));
    }

    fn open_line(&self, position: &DiffLinePosition, cx: &mut Context<Self>) {
        let path = match &self.source {
            DiffSource::GitFile { repo, .. } | DiffSource::Commit { repo, .. } => {
                repo.join(position.path().as_ref())
            }
            DiffSource::Compare { path, .. } => path.clone(),
            DiffSource::Text(_) => return,
        };
        if position.side() == DiffSide::Modified && path.is_file() {
            cx.emit(WorkspaceEvent::OpenFile {
                path,
                line: Some(position.line() as u32),
                column: None,
            });
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FileOp {
    Stage,
    Unstage,
    Discard,
    DiscardConfirmed,
}

impl Render for DiffView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let center = |text: String| {
            div()
                .size_full()
                .flex()
                .flex_col()
                .gap_2()
                .items_center()
                .justify_center()
                .text_color(c.muted_foreground)
                .child(text)
        };
        if self.loading {
            return center("Loading diff…".into()).into_any_element();
        }
        if let Some(e) = &self.error {
            return center(e.to_string()).into_any_element();
        }
        if self.patches.is_empty() {
            return center("No changes.".into()).into_any_element();
        }
        if self.patches.iter().all(|p| p.binary && p.hunks.is_empty()) {
            return center("Binary file changed.".into()).into_any_element();
        }
        if self.lines > LARGE_DIFF_LINES && !self.show_anyway {
            let lines = self.lines;
            return center(format!("Large diff — {lines} lines."))
                .child(
                    Button::new("show-anyway")
                        .outline()
                        .label("Show anyway")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_anyway = true;
                            cx.notify();
                        })),
                )
                .into_any_element();
        }
        let Some(state) = self.state.clone() else {
            return center(String::new()).into_any_element();
        };
        let (anns, map) = self.annotations();
        let actions = self.hunk_actions();
        let weak: WeakEntity<Self> = cx.entity().downgrade();
        let w_ann = weak.clone();
        let busy = self.busy;
        let mut diff = Diff::new(&state)
            .h_full()
            .annotations(anns)
            .render_annotation(move |ann, _, _| {
                let id = match ann.id() {
                    ElementId::Integer(n) => *n,
                    _ => return div().into_any_element(),
                };
                let Some(&(fi, hi)) = map.get(&id) else {
                    return div().into_any_element();
                };
                let mut row = div().flex().gap_1().px_2().py_0p5();
                for action in &actions {
                    let action = *action;
                    let w = w_ann.clone();
                    let (label, style) = match action {
                        HunkAction::Stage => ("Stage Hunk", 0),
                        HunkAction::Unstage => ("Unstage Hunk", 0),
                        HunkAction::Discard => ("Discard Hunk", 1),
                    };
                    let mut b = Button::new(SharedString::from(format!("h-{id}-{label}")))
                        .size(ButtonSize::Sm)
                        .label(label)
                        .disabled(busy);
                    b = if style == 1 { b.ghost() } else { b.outline() };
                    row = row.child(b.on_click(move |_, window, cx| {
                        let _ = w.update(cx, |this, cx| this.run_hunk(fi, hi, action, window, cx));
                    }));
                }
                row.into_any_element()
            })
            .on_line_click({
                let w = weak.clone();
                move |position, _, _, cx| {
                    let _ = w.update(cx, |this, cx| this.open_line(position, cx));
                }
            });
        if let DiffSource::GitFile { kind, .. } = &self.source {
            let kind = *kind;
            let w = weak.clone();
            diff = diff.render_header_suffix(move |_, _, _| {
                let mut row = div().flex().gap_1();
                let ops: Vec<(&str, FileOp)> = match kind {
                    DiffKind::Unstaged => vec![("Stage File", FileOp::Stage), ("Discard…", FileOp::Discard)],
                    DiffKind::Staged => vec![("Unstage File", FileOp::Unstage)],
                    DiffKind::Untracked => vec![("Stage File", FileOp::Stage), ("Delete…", FileOp::Discard)],
                };
                for (label, op) in ops {
                    let w = w.clone();
                    row = row.child(
                        Button::new(SharedString::from(format!("file-{label}")))
                            .size(ButtonSize::Sm)
                            .ghost()
                            .label(label)
                            .disabled(busy)
                            .on_click(move |_, window, cx| {
                                let _ = w.update(cx, |this, cx| this.file_op(op, window, cx));
                            }),
                    );
                }
                row.into_any_element()
            });
        }
        div()
            .id("diff-view")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(div().flex_1().min_h_0().child(diff))
            .when(busy, |el| {
                el.child(
                    div()
                        .h(px(22.))
                        .px_2()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child("Working…"),
                )
            })
            .into_any_element()
    }
}
