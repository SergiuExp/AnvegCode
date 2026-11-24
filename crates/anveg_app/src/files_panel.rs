//! The Files tab (next phases, 4.1) and quick open (⌘P).
//!
//! - Roots: the main folder and every extra folder.
//! - A folder is listed only when it is expanded, with ignore rules.
//! - Only expanded folders are watched, and only while the tab is visible;
//!   the tree follows Claude creating, renaming and deleting files.
//! - Git status colours come from the window's [`GitStore`].

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::ops::Range;

use anveg_core::files::{self, DirEntry};
use anveg_core::fswatch;
use futures::StreamExt as _;
use gpui_cn::prelude::*;
use gpui_cn::{ContextMenu, Input, InputEvent, InputState, MenuEntry, MenuItem as CnMenuItem, MenuState};
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, UniformListScrollHandle,
    Window, div, px, uniform_list,
};

use crate::git_panel::{GitStore, change_color};
use crate::model::model;
use crate::ui::Colors;
use crate::workspace::{ButtonStyle, Prompt, WorkspaceEvent};

const ROW: f32 = 24.;

type MenuAction = Box<dyn Fn(&mut FilesPanel, &mut Context<FilesPanel>)>;

/// The quick-open index: (root index, relative path) and display labels.
pub type IndexData = (Arc<Vec<(usize, PathBuf)>>, Arc<Vec<String>>);

#[derive(Clone, Debug)]
struct TreeRow {
    path: PathBuf,
    name: String,
    depth: usize,
    is_dir: bool,
    ignored: bool,
    root: bool,
}

pub struct FilesPanel {
    roots: Vec<PathBuf>,
    expanded: BTreeSet<PathBuf>,
    children: HashMap<PathBuf, Vec<DirEntry>>,
    errors: HashMap<PathBuf, String>,
    show_ignored: bool,
    rows: Vec<TreeRow>,
    visible: bool,
    selected: Option<PathBuf>,
    store: Entity<GitStore>,
    scroll: UniformListScrollHandle,
    menu: Entity<MenuState>,
    watches: HashMap<PathBuf, fswatch::Subscription>,
    watch_tx: futures::channel::mpsc::UnboundedSender<PathBuf>,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
    _subs: Vec<gpui_kit::Subscription>,
}

impl EventEmitter<WorkspaceEvent> for FilesPanel {}

impl Focusable for FilesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl FilesPanel {
    pub fn new(
        roots: Vec<PathBuf>,
        saved_expanded: Vec<PathBuf>,
        store: Entity<GitStore>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<PathBuf>();
        let listen = cx.spawn_in(window, async move |this, cx| {
            while let Some(dir) = rx.next().await {
                let mut dirs = BTreeSet::from([dir]);
                while let Ok(d) = rx.try_recv() {
                    dirs.insert(d);
                }
                let ok = this.update(cx, |this, cx| {
                    for d in dirs {
                        this.list(d, cx);
                    }
                });
                if ok.is_err() {
                    break;
                }
            }
        });
        let menu = cx.new(MenuState::new);
        let subs = vec![
            cx.observe(&store, |_, _, cx| cx.notify()),
            cx.observe(&menu, |_, _, cx| cx.notify()),
        ];
        let mut expanded: BTreeSet<PathBuf> = saved_expanded
            .into_iter()
            .filter(|p| p.is_dir() && roots.iter().any(|r| p.starts_with(r)))
            .collect();
        if expanded.is_empty() {
            expanded.extend(roots.iter().cloned());
        }
        let mut this = Self {
            roots,
            expanded,
            children: HashMap::new(),
            errors: HashMap::new(),
            show_ignored: false,
            rows: Vec::new(),
            visible: false,
            selected: None,
            store,
            scroll: UniformListScrollHandle::new(),
            menu,
            watches: HashMap::new(),
            watch_tx: tx,
            focus: cx.focus_handle(),
            _tasks: vec![listen],
            _subs: subs,
        };
        this.rebuild_rows();
        this
    }

    pub fn expanded_list(&self) -> Vec<PathBuf> {
        self.expanded.iter().cloned().collect()
    }

    /// Shown or hidden: watch expanded folders only while shown.
    pub fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            for dir in self.expanded.clone() {
                self.watch(&dir, cx);
                self.list(dir, cx);
            }
        } else {
            self.watches.clear();
        }
    }

    fn watch(&mut self, dir: &Path, cx: &mut Context<Self>) {
        if self.watches.contains_key(dir) || !self.visible {
            return;
        }
        let Some(service) = model(cx).read(cx).fswatch.clone() else {
            return;
        };
        let tx = self.watch_tx.clone();
        let d = dir.to_path_buf();
        let sub = service.subscribe(dir, false, move |_| {
            let _ = tx.unbounded_send(d.clone());
        });
        self.watches.insert(dir.to_path_buf(), sub);
    }

    /// (Re)list one folder in the background.
    fn list(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        let show_ignored = self.show_ignored;
        let d = dir.clone();
        let task = cx.background_spawn(async move { files::list_dir(&d, show_ignored) });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(entries) => {
                        this.errors.remove(&dir);
                        this.children.insert(dir.clone(), entries);
                    }
                    Err(e) => {
                        // The folder is gone: forget it.
                        this.children.remove(&dir);
                        if !dir.exists() {
                            this.expanded.remove(&dir);
                            this.watches.remove(&dir);
                        } else {
                            this.errors.insert(dir.clone(), format!("{e:#}"));
                        }
                    }
                }
                this.rebuild_rows();
                cx.notify();
            });
        }));
        // Keep the task list short.
        if self._tasks.len() > 64 {
            self._tasks.drain(1..32);
        }
    }

    fn toggle(&mut self, dir: &Path, cx: &mut Context<Self>) {
        if !self.expanded.remove(dir) {
            self.expanded.insert(dir.to_path_buf());
            self.watch(dir, cx);
            if !self.children.contains_key(dir) {
                self.list(dir.to_path_buf(), cx);
            }
        } else {
            // Collapse: stop watching it and everything below.
            self.watches.retain(|p, _| !p.starts_with(dir));
            self.expanded.retain(|p| !p.starts_with(dir) || p == dir);
            self.expanded.remove(dir);
        }
        self.rebuild_rows();
        cx.notify();
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        for root in &self.roots {
            rows.push(TreeRow {
                path: root.clone(),
                name: root
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| root.display().to_string()),
                depth: 0,
                is_dir: true,
                ignored: false,
                root: true,
            });
            self.push_children(root, 1, &mut rows);
        }
        self.rows = rows;
    }

    fn push_children(&self, dir: &Path, depth: usize, rows: &mut Vec<TreeRow>) {
        if !self.expanded.contains(dir) {
            return;
        }
        let Some(entries) = self.children.get(dir) else {
            return;
        };
        for e in entries {
            rows.push(TreeRow {
                path: e.path.clone(),
                name: e.name.clone(),
                depth,
                is_dir: e.is_dir,
                ignored: e.ignored,
                root: false,
            });
            if e.is_dir {
                self.push_children(&e.path, depth + 1, rows);
            }
        }
    }

    fn relative(&self, path: &Path) -> String {
        files::relative_to_roots(path, &self.roots)
            .unwrap_or_else(|| path.to_path_buf())
            .display()
            .to_string()
    }

    fn click(&mut self, row: &TreeRow, cx: &mut Context<Self>) {
        self.selected = Some(row.path.clone());
        if row.is_dir {
            self.toggle(&row.path, cx);
        } else {
            cx.emit(WorkspaceEvent::OpenFile {
                path: row.path.clone(),
                line: None,
                column: None,
            });
        }
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Actions
    // -----------------------------------------------------------------

    fn new_entry(&mut self, parent: PathBuf, folder: bool, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let what = if folder { "Folder" } else { "File" };
        let prompt = Prompt::new(format!("New {what}"), format!("In {}", self.relative(&parent)))
            .input("Name", "")
            .cancel()
            .button("Create", ButtonStyle::Primary, move |value, _, cx| {
                let value = value.unwrap_or_default();
                let name = files::validate_name(&value)?.to_owned();
                let path = parent.join(&name);
                let result = if folder {
                    std::fs::create_dir(&path).map_err(|e| e.to_string())
                } else {
                    files::create_file(&path).map_err(|e| format!("{e:#}"))
                };
                result?;
                let _ = weak.update(cx, |this, cx| {
                    if !this.expanded.contains(&parent) {
                        this.toggle(&parent, cx);
                    }
                    this.list(parent.clone(), cx);
                    if !folder {
                        cx.emit(WorkspaceEvent::OpenFile {
                            path,
                            line: None,
                            column: None,
                        });
                    }
                });
                Ok(())
            });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn rename(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let prompt = Prompt::new("Rename", self.relative(&path))
            .input("New name", name)
            .cancel()
            .button("Rename", ButtonStyle::Primary, move |value, _, cx| {
                let value = value.unwrap_or_default();
                let name = files::validate_name(&value)?.to_owned();
                files::rename(&path, &name).map_err(|e| format!("{e:#}"))?;
                let _ = weak.update(cx, |this, cx| {
                    if let Some(parent) = path.parent() {
                        this.list(parent.to_path_buf(), cx);
                    }
                });
                Ok(())
            });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn delete(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let prompt = Prompt::new(
            format!("Move {} to the Trash?", self.relative(&path)),
            "You can put it back from the Trash.",
        )
        .cancel()
        .button("Move to Trash", ButtonStyle::Destructive, move |_, _, cx| {
            trash::delete(&path).map_err(|e| e.to_string())?;
            let _ = weak.update(cx, |this, cx| {
                if let Some(parent) = path.parent() {
                    this.list(parent.to_path_buf(), cx);
                }
            });
            Ok(())
        });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn menu_for(&self, row: &TreeRow, this: &Entity<Self>) -> Vec<MenuEntry> {
        let item = |key: &str, label: &str, f: MenuAction| {
            let weak = this.downgrade();
            let f = std::rc::Rc::new(f);
            MenuEntry::Item(CnMenuItem::new(key.to_owned(), label.to_owned()).on_select(move |_, cx| {
                let f = f.clone();
                let _ = weak.update(cx, |this, cx| f(this, cx));
            }))
        };
        let path = row.path.clone();
        let dir = if row.is_dir {
            row.path.clone()
        } else {
            row.path.parent().map(Path::to_path_buf).unwrap_or_default()
        };
        let rel = self.relative(&row.path);
        let mut entries = Vec::new();
        if !row.is_dir {
            let p = path.clone();
            entries.push(item(
                "open",
                "Open",
                Box::new(move |_, cx| {
                    cx.emit(WorkspaceEvent::OpenFile {
                        path: p.clone(),
                        line: None,
                        column: None,
                    })
                }),
            ));
        }
        let mention = if row.root { String::new() } else { rel.clone() };
        if !row.root {
            entries.push(item(
                "mention",
                "Mention in Claude",
                Box::new(move |_, cx| {
                    cx.emit(WorkspaceEvent::TypeIntoClaude(format!("@{mention} ")))
                }),
            ));
        }
        entries.push(MenuEntry::Separator);
        let d1 = dir.clone();
        entries.push(item(
            "new-file",
            "New File…",
            Box::new(move |this, cx| this.new_entry(d1.clone(), false, cx)),
        ));
        let d2 = dir.clone();
        entries.push(item(
            "new-folder",
            "New Folder…",
            Box::new(move |this, cx| this.new_entry(d2.clone(), true, cx)),
        ));
        if !row.root {
            let p = path.clone();
            entries.push(item(
                "rename",
                "Rename…",
                Box::new(move |this, cx| this.rename(p.clone(), cx)),
            ));
        }
        entries.push(MenuEntry::Separator);
        let p = path.clone();
        entries.push(item(
            "reveal",
            "Reveal in Finder",
            Box::new(move |_, cx| cx.reveal_path(&p)),
        ));
        let p = path.display().to_string();
        entries.push(item(
            "copy-path",
            "Copy Path",
            Box::new(move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(p.clone()))),
        ));
        let r = rel.clone();
        entries.push(item(
            "copy-rel",
            "Copy Relative Path",
            Box::new(move |_, cx| cx.write_to_clipboard(ClipboardItem::new_string(r.clone()))),
        ));
        if !row.root {
            entries.push(MenuEntry::Separator);
            let p = path.clone();
            let label = "Move to Trash…".to_owned();
            let weak = this.downgrade();
            entries.push(MenuEntry::Item(
                CnMenuItem::new("delete", label)
                    .destructive()
                    .on_select(move |_, cx| {
                        let _ = weak.update(cx, |this, cx| this.delete(p.clone(), cx));
                    }),
            ));
        }
        entries
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = Colors::of(cx);
        let store = self.store.read(cx);
        let menu_entity = cx.entity();
        range
            .filter_map(|ix| {
                let row = self.rows.get(ix)?.clone();
                let change = if row.is_dir {
                    None
                } else {
                    store.file_change(&row.path)
                };
                let dir_changed = row.is_dir && !row.root && store.dir_changed(&row.path);
                let color = match change {
                    Some(ch) => change_color(ch, &c),
                    None if row.ignored => c.muted_foreground.opacity(0.6),
                    None if dir_changed => gpui_kit::hsla(0.11, 0.8, 0.5, 1.0),
                    None => c.foreground,
                };
                let selected = self.selected.as_deref() == Some(row.path.as_path());
                let icon = if row.is_dir {
                    if self.expanded.contains(&row.path) {
                        "▾"
                    } else {
                        "▸"
                    }
                } else {
                    ""
                };
                let error = self.errors.get(&row.path).cloned();
                let r2 = row.clone();
                let el = div()
                    .id(SharedString::from(format!("f-{}", row.path.display())))
                    .h(px(ROW))
                    .pl(px(6. + row.depth as f32 * 12.))
                    .pr_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .cursor_pointer()
                    .when(selected, |el| el.bg(c.selected_row))
                    .hover(|el| el.bg(c.accent.opacity(0.5)))
                    .child(div().w(px(10.)).text_xs().text_color(c.muted_foreground).child(icon))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(color)
                            .when(row.root, |el| el.font_weight(gpui_kit::FontWeight::SEMIBOLD))
                            .child(row.name.clone()),
                    )
                    .children(change.map(|ch| {
                        div().text_xs().text_color(color).child(ch.letter())
                    }))
                    .children(error.map(|_| div().text_xs().text_color(c.destructive).child("!")))
                    .on_click(cx.listener(move |this, _, _, cx| this.click(&r2, cx)));
                let row_for_menu = row.clone();
                let entity = menu_entity.clone();
                Some(
                    ContextMenu::new(
                        SharedString::from(format!("fm-{}", row.path.display())),
                        &self.menu,
                    )
                    .items(move |_, cx| entity.read(cx).menu_for(&row_for_menu, &entity))
                    .child(el)
                    .into_any_element(),
                )
            })
            .collect()
    }
}

impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let count = self.rows.len();
        div()
            .id("files-panel")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_0p5()
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child("⌘P to open a file by name"),
                    )
                    .child(
                        Button::new("show-ignored")
                            .ghost()
                            .size(ButtonSize::Sm)
                            .icon(Icon::from(if self.show_ignored {
                                gpui_kit::assets::IconName::Eye
                            } else {
                                gpui_kit::assets::IconName::EyeOff
                            }))
                            .tooltip(if self.show_ignored {
                                "Hide Ignored Files"
                            } else {
                                "Show Ignored Files"
                            })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.show_ignored = !this.show_ignored;
                                for dir in this.expanded.clone() {
                                    this.list(dir, cx);
                                }
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("files-refresh")
                            .ghost()
                            .size(ButtonSize::Sm)
                            .icon(Icon::from(gpui_kit::assets::IconName::RefreshCw))
                            .tooltip("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| {
                                for dir in this.expanded.clone() {
                                    this.list(dir, cx);
                                }
                            })),
                    ),
            )
            .child(
                uniform_list(
                    "file-rows",
                    count,
                    cx.processor(|this, range: Range<usize>, window, cx| {
                        this.render_rows(range, window, cx)
                    }),
                )
                .track_scroll(&self.scroll)
                .flex_1(),
            )
    }
}

// ---------------------------------------------------------------------
// Quick open
// ---------------------------------------------------------------------

/// What quick open asks for.
pub enum QuickOpenEvent {
    Open(PathBuf),
    Dismiss,
}

/// The ⌘P file finder.
pub struct QuickOpen {
    roots: Vec<PathBuf>,
    input: Entity<InputState>,
    files: Arc<Vec<(usize, PathBuf)>>,
    labels: Arc<Vec<String>>,
    results: Vec<usize>,
    selected: usize,
    indexing: bool,
    _tasks: Vec<Task<()>>,
    _subs: Vec<gpui_kit::Subscription>,
}

impl EventEmitter<QuickOpenEvent> for QuickOpen {}

impl Focusable for QuickOpen {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.input.read(cx).focus_handle(cx)
    }
}

/// Files are listed once per window and reused until the watcher says the
/// tree changed (next phases, 4.1: "built lazily on first use").
pub struct FileIndex {
    pub files: Arc<Vec<(usize, PathBuf)>>,
    pub labels: Arc<Vec<String>>,
    stale: Arc<std::sync::atomic::AtomicBool>,
    _watch: Vec<fswatch::Subscription>,
}

impl QuickOpen {
    pub fn new(
        roots: Vec<PathBuf>,
        index: Option<&FileIndex>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Go to file…"));
        let subs = vec![cx.subscribe_in(&input, window, |this, _, event, _, cx| match event {
            InputEvent::Change => this.update_results(cx),
            InputEvent::PressEnter { .. } => this.confirm(cx),
            _ => {}
        })];
        let (files, labels, fresh) = match index {
            Some(i) => (i.files.clone(), i.labels.clone(), !i.is_stale()),
            None => (Arc::default(), Arc::default(), false),
        };
        let mut this = Self {
            roots,
            input,
            files,
            labels,
            results: Vec::new(),
            selected: 0,
            indexing: !fresh,
            _tasks: Vec::new(),
            _subs: subs,
        };
        this.update_results(cx);
        this
    }

    /// The index finished (or was reused).
    pub fn set_index(&mut self, index: &FileIndex, cx: &mut Context<Self>) {
        self.files = index.files.clone();
        self.labels = index.labels.clone();
        self.indexing = false;
        self.update_results(cx);
    }

    fn update_results(&mut self, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().to_string();
        self.results = files::fuzzy_rank(&query, &self.labels, 50);
        self.selected = 0;
        cx.notify();
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        if let Some(&i) = self.results.get(self.selected)
            && let Some((root, rel)) = self.files.get(i)
            && let Some(root) = self.roots.get(*root)
        {
            cx.emit(QuickOpenEvent::Open(root.join(rel)));
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "down" => {
                if self.selected + 1 < self.results.len() {
                    self.selected += 1;
                }
            }
            "up" => self.selected = self.selected.saturating_sub(1),
            "escape" => cx.emit(QuickOpenEvent::Dismiss),
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

/// Build the quick-open index in the background.
pub fn build_index(
    roots: Vec<PathBuf>,
    cx: &mut App,
) -> Task<IndexData> {
    let multi = roots.len() > 1;
    cx.background_spawn(async move {
        let files = files::index_files(&roots, 200_000);
        let labels = files
            .iter()
            .map(|(i, rel)| {
                if multi {
                    let root = roots[*i]
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    format!("{root}/{}", rel.display())
                } else {
                    rel.display().to_string()
                }
            })
            .collect();
        (Arc::new(files), Arc::new(labels))
    })
}

impl FileIndex {
    pub fn is_stale(&self) -> bool {
        self.stale.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn new(
        files: Arc<Vec<(usize, PathBuf)>>,
        labels: Arc<Vec<String>>,
        roots: &[PathBuf],
        cx: &App,
    ) -> Self {
        let stale = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watch = match model(cx).read(cx).fswatch.clone() {
            Some(service) => roots
                .iter()
                .map(|root| {
                    let flag = stale.clone();
                    let f = move || flag.store(true, std::sync::atomic::Ordering::Relaxed);
                    service.subscribe(root, true, move |paths| {
                        // New or removed files only matter for the index;
                        // ignore churn under .git.
                        if paths
                            .iter()
                            .any(|p| !p.components().any(|c| c.as_os_str() == ".git"))
                        {
                            f();
                        }
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        Self {
            files,
            labels,
            stale,
            _watch: watch,
        }
    }
}

impl Render for QuickOpen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let rows: Vec<AnyElement> = self
            .results
            .iter()
            .enumerate()
            .filter_map(|(n, &i)| {
                let label = self.labels.get(i)?.clone();
                let (dir, name) = match label.rsplit_once('/') {
                    Some((d, n)) => (d.to_owned(), n.to_owned()),
                    None => (String::new(), label.clone()),
                };
                Some(
                    div()
                        .id(SharedString::from(format!("qo-{n}")))
                        .px_2()
                        .py_0p5()
                        .flex()
                        .gap_2()
                        .cursor_pointer()
                        .when(n == self.selected, |el| el.bg(c.selected_row))
                        .hover(|el| el.bg(c.accent.opacity(0.5)))
                        .child(div().text_sm().child(name))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child(dir),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.selected = n;
                            this.confirm(cx);
                        }))
                        .into_any_element(),
                )
            })
            .collect();
        div()
            .id("quick-open")
            .w(px(560.))
            .max_h(px(420.))
            .flex()
            .flex_col()
            .rounded_lg()
            .border_1()
            .border_color(c.border)
            .bg(c.background)
            .shadow_lg()
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .child(div().p_2().child(Input::new(&self.input)))
            .when(self.indexing, |el| {
                el.child(
                    div()
                        .px_3()
                        .pb_1()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child("Indexing files…"),
                )
            })
            .child(
                div()
                    .id("qo-results")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .pb_1()
                    .children(rows),
            )
    }
}
