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
    ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _, Task, UniformListScrollHandle,
    Window, div, hsla, px, uniform_list, FontWeight,
};
use gpui_kit::assets::IconName;

use crate::file_icons::{FOLDER, FOLDER_OPEN, file_icon};

use crate::git_panel::{GitStore, IGNORED_COLOR, change_color};
use crate::model::model;
use crate::ui::Colors;
use crate::workspace::{ButtonStyle, Prompt, WorkspaceEvent};

const ROW: f32 = 24.;
/// Left padding of a top-level row, and the indent per level.
const INDENT_START: f32 = 6.;
const INDENT: f32 = 14.;
/// The chevron column, and the space between it and the icon.
const CHEVRON: f32 = 14.;
const GAP: f32 = 2.;

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
    /// A line under an expanded folder that has nothing to list: "Empty",
    /// or why it can't be read. Not selectable; `path` is the folder's.
    note: Option<String>,
}

impl TreeRow {
    fn note(dir: &Path, depth: usize, text: String) -> Self {
        Self {
            path: dir.to_path_buf(),
            name: String::new(),
            depth,
            is_dir: false,
            ignored: false,
            root: false,
            note: Some(text),
        }
    }
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
                note: None,
            });
            self.push_children(root, 1, false, &mut rows);
        }
        self.rows = rows;
    }

    /// `in_ignored`: `dir` is ignored, so everything in it is too (git
    /// ignores a folder's contents with it).
    fn push_children(&self, dir: &Path, depth: usize, in_ignored: bool, rows: &mut Vec<TreeRow>) {
        if !self.expanded.contains(dir) {
            return;
        }
        if let Some(error) = self.errors.get(dir) {
            rows.push(TreeRow::note(dir, depth, format!("Can't read this folder: {error}")));
            return;
        }
        let Some(entries) = self.children.get(dir) else {
            return;
        };
        if entries.is_empty() {
            rows.push(TreeRow::note(dir, depth, "Empty".to_owned()));
            return;
        }
        for e in entries {
            rows.push(TreeRow {
                path: e.path.clone(),
                name: e.name.clone(),
                depth,
                is_dir: e.is_dir,
                ignored: e.ignored || in_ignored,
                root: false,
                note: None,
            });
            if e.is_dir {
                self.push_children(&e.path, depth + 1, e.ignored || in_ignored, rows);
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

    /// The repository `path` is in (the innermost), and `path` relative
    /// to it ("." for the root itself).
    fn repo_of(&self, path: &Path, cx: &App) -> Option<(PathBuf, String)> {
        let store = self.store.read(cx);
        let root = store
            .repos
            .iter()
            .filter(|r| path.starts_with(&r.root))
            .max_by_key(|r| r.root.as_os_str().len())?
            .root
            .clone();
        let rel = path.strip_prefix(&root).ok()?.to_string_lossy().into_owned();
        Some((root, if rel.is_empty() { ".".into() } else { rel }))
    }

    /// File ▸ Add to Git: stage the file, or a folder's new and changed
    /// files. An ignored path asks first, naming the rule.
    fn add_to_git(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        // A folder with its own .git inside another repository: ask which.
        if path.is_dir() && path.join(".git").exists() {
            let outer = self
                .store
                .read(cx)
                .repos
                .iter()
                .filter(|r| r.root != path && path.starts_with(&r.root))
                .max_by_key(|r| r.root.as_os_str().len())
                .map(|r| r.root.clone());
            if let Some(outer) = outer {
                self.ask_inner_repo(path, outer, cx);
                return;
            }
        }
        let Some((root, rel)) = self.repo_of(&path, cx) else {
            return;
        };
        self.add_in(root, rel, path, cx);
    }

    /// `git add -v` of `rel` in `root`, with a toast; an ignored path asks.
    fn add_in(&mut self, root: PathBuf, rel: String, path: PathBuf, cx: &mut Context<Self>) {
        let git = model(cx).read(cx).git();
        let (r, p) = (root.clone(), rel.clone());
        let task = cx.background_spawn(async move {
            match git.ignore_rule(&r, &p) {
                Ok(Some(rule)) => Ok(Err(rule)),
                Ok(None) => git.add_counted(&r, &[&p]).map(Ok),
                Err(e) => Err(e),
            }
        });
        let shown = self.relative(&path);
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(Ok(0)) => cx.emit(WorkspaceEvent::Toast(
                    format!("Nothing to add: {shown} has no new or changed files.").into(),
                )),
                Ok(Ok(n)) => {
                    let what = if n == 1 { "1 file".to_owned() } else { format!("{n} files") };
                    cx.emit(WorkspaceEvent::Toast(
                        format!("Added {what} to git (staged). Commit in the Git tab.").into(),
                    ));
                    cx.emit(WorkspaceEvent::GitChanged);
                }
                Ok(Err(rule)) => this.ask_add_ignored(root, rel, shown, rule, cx),
                Err(e) => cx.emit(WorkspaceEvent::Prompt(Prompt::message(
                    "Could not add to git",
                    format!("{e:#}"),
                ))),
            });
        }));
    }

    /// Add to Git on a repository inside another one.
    fn ask_inner_repo(&mut self, path: PathBuf, outer: PathBuf, cx: &mut Context<Self>) {
        let name = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string())
        };
        let (inner_name, outer_name) = (name(&path), name(&outer));
        let Ok(rel) = path.strip_prefix(&outer).map(|r| r.to_string_lossy().into_owned()) else {
            return;
        };
        let weak = cx.entity().downgrade();
        let weak2 = weak.clone();
        let (p1, p2, o2, r2) = (path.clone(), path.clone(), outer.clone(), rel.clone());
        let prompt = Prompt::new(
            format!("{inner_name}/ is a separate git repository"),
            format!(
                "It has its own .git folder, so {outer_name} can't see its files. Add its new files to its \
                 own repository, or include them in {outer_name}: that moves {inner_name}/.git to the Trash \
                 (its own commits and branches go with it; you can put it back), then adds the files to \
                 {outer_name}."
            ),
        )
        .cancel()
        .button("Add to Its Own Repository", ButtonStyle::Outline, move |_, _, cx| {
            let path = p1.clone();
            let _ = weak.update(cx, |this, cx| this.add_in(path.clone(), ".".into(), path, cx));
            Ok(())
        })
        .button(format!("Include in {outer_name}"), ButtonStyle::Primary, move |_, _, cx| {
            let (path, outer, rel) = (p2.clone(), o2.clone(), r2.clone());
            let _ = weak2.update(cx, |this, cx| this.include_inner(outer, rel, path, cx));
            Ok(())
        });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    /// Move the inner repository's .git to the Trash, then add its files to
    /// the outer repository.
    fn include_inner(&mut self, outer: PathBuf, rel: String, path: PathBuf, cx: &mut Context<Self>) {
        let git = model(cx).read(cx).git();
        let (o, r) = (outer.clone(), rel.clone());
        let task = cx.background_spawn(async move {
            git.absorb_inner_repo(&o, &r, |dot_git| {
                trash::delete(dot_git)
                    .map_err(|e| anyhow::anyhow!("Could not move {} to the Trash: {e}", dot_git.display()))
            })
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.store.update(cx, |s, cx| s.rediscover(cx));
                match result {
                    Ok(()) => this.add_in(outer, rel, path, cx),
                    Err(e) => cx.emit(WorkspaceEvent::Prompt(Prompt::message(
                        "Could not include the repository",
                        format!("{e:#}"),
                    ))),
                }
            });
        }));
    }

    fn ask_add_ignored(&mut self, root: PathBuf, rel: String, shown: String, rule: String, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let folder = root.join(&rel).is_dir();
        let prompt = Prompt::new(
            format!("{shown} is ignored by git"),
            format!(
                "It matches {rule}, so git never lists it as a new file. Add it anyway (git add -f)?{}",
                if folder {
                    " Everything in the folder is added."
                } else {
                    ""
                }
            ),
        )
        .cancel()
        .button("Add Anyway", ButtonStyle::Primary, move |_, _, cx| {
            let (root, rel, shown) = (root.clone(), rel.clone(), shown.clone());
            let _ = weak.update(cx, |this, cx| this.force_add(root, rel, shown, cx));
            Ok(())
        });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    /// `git add -f`, off the UI thread (a folder can be large).
    fn force_add(&mut self, root: PathBuf, rel: String, shown: String, cx: &mut Context<Self>) {
        let git = model(cx).read(cx).git();
        let task = cx.background_spawn(async move { git.stage_force(&root, &[&rel]) });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |_, cx| match result {
                Ok(()) => {
                    cx.emit(WorkspaceEvent::Toast(
                        format!("Added {shown} to git (staged). Commit in the Git tab.").into(),
                    ));
                    cx.emit(WorkspaceEvent::GitChanged);
                }
                Err(e) => cx.emit(WorkspaceEvent::Prompt(Prompt::message(
                    "Could not add to git",
                    format!("{e:#}"),
                ))),
            });
        }));
    }

    fn menu_for(&self, row: &TreeRow, this: &Entity<Self>, cx: &App) -> Vec<MenuEntry> {
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
        // Only when git has something to take: something new (or ignored),
        // or edits not staged yet. Not for files already in git, unchanged.
        let label = if self.repo_of(&path, cx).is_none() {
            None
        } else if row.ignored {
            Some("Add to Git")
        } else {
            use anveg_core::git::Addable;
            match self.store.read(cx).addable(&path) {
                Some(Addable::New) => Some("Add to Git"),
                Some(Addable::Changes) => Some("Stage Changes"),
                Some(Addable::Nothing) => None,
                // Status not loaded yet: offer it (git add is harmless).
                None => Some("Add to Git"),
            }
        };
        if let Some(label) = label {
            entries.push(MenuEntry::Separator);
            let p = path.clone();
            entries.push(item(
                "add-to-git",
                label,
                Box::new(move |this, cx| this.add_to_git(p.clone(), cx)),
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

    // -----------------------------------------------------------------
    // Keyboard
    // -----------------------------------------------------------------

    fn selectable(&self, ix: usize) -> bool {
        self.rows.get(ix).is_some_and(|r| r.note.is_none())
    }

    fn selected_ix(&self) -> Option<usize> {
        let path = self.selected.as_deref()?;
        self.rows.iter().position(|r| r.note.is_none() && r.path == path)
    }

    fn select_ix(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.rows.get(ix) {
            self.selected = Some(row.path.clone());
            self.scroll.scroll_to_item(ix, ScrollStrategy::Nearest);
            cx.notify();
        }
    }

    fn move_selection(&mut self, down: bool, cx: &mut Context<Self>) {
        let n = self.rows.len();
        let next = match self.selected_ix() {
            None => (0..n).find(|&i| self.selectable(i)),
            Some(i) if down => (i + 1..n).find(|&j| self.selectable(j)),
            Some(i) => (0..i).rev().find(|&j| self.selectable(j)),
        };
        if let Some(ix) = next {
            self.select_ix(ix, cx);
        }
    }

    /// ↑ ↓ move, → opens a folder (then goes into it), ← closes it (or
    /// goes up to its folder), ↩ opens a file or opens/closes a folder.
    fn on_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let m = &event.keystroke.modifiers;
        if m.platform || m.control || m.alt || m.function {
            return;
        }
        let current = self.selected_ix().and_then(|ix| Some((ix, self.rows.get(ix)?.clone())));
        match (event.keystroke.key.as_str(), current) {
            ("down", _) => self.move_selection(true, cx),
            ("up", _) => self.move_selection(false, cx),
            ("right", Some((ix, row))) if row.is_dir => {
                if !self.expanded.contains(&row.path) {
                    self.toggle(&row.path, cx);
                } else if self.rows.get(ix + 1).is_some_and(|r| r.depth > row.depth) && self.selectable(ix + 1) {
                    self.select_ix(ix + 1, cx);
                }
            }
            ("left", Some((ix, row))) => {
                if row.is_dir && self.expanded.contains(&row.path) {
                    self.toggle(&row.path, cx);
                } else if let Some(parent) =
                    (0..ix).rev().find(|&j| self.rows.get(j).is_some_and(|r| r.depth < row.depth))
                {
                    self.select_ix(parent, cx);
                }
            }
            ("enter", Some((_, row))) => self.click(&row, cx),
            _ => return,
        }
        cx.stop_propagation();
    }

    /// The folder New File and New Folder go in: the selected folder, the
    /// selected file's folder, or the main folder.
    fn target_dir(&self) -> Option<PathBuf> {
        let row = self.selected_ix().and_then(|ix| self.rows.get(ix));
        match row {
            Some(r) if r.is_dir => Some(r.path.clone()),
            Some(r) => r.path.parent().map(Path::to_path_buf),
            None => self.roots.first().cloned(),
        }
    }

    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.expanded.retain(|p| self.roots.contains(p));
        self.watches.retain(|p, _| self.roots.contains(p));
        if self
            .selected
            .as_ref()
            .is_some_and(|s| !self.roots.iter().any(|r| s.parent() == Some(r.as_path()) || s == r))
        {
            self.selected = None;
        }
        self.rebuild_rows();
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------

    fn render_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let c = Colors::of(cx);
        let dark = cx.theme().is_dark();
        let folder_color = if dark {
            hsla(0.58, 0.45, 0.66, 1.0)
        } else {
            hsla(0.58, 0.55, 0.52, 1.0)
        };
        let guide = c.foreground.opacity(if dark { 0.10 } else { 0.12 });
        let focused = self.focus.is_focused(window);
        let store = self.store.read(cx);
        let menu_entity = cx.entity();
        range
            .filter_map(|ix| {
                let row = self.rows.get(ix)?.clone();
                let indent = INDENT_START + row.depth as f32 * INDENT;
                // A line down from each enclosing folder's chevron.
                let guides = (0..row.depth).map(|level| {
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(INDENT_START + level as f32 * INDENT + CHEVRON / 2.))
                        .w(px(1.))
                        .bg(guide)
                });
                let base = div()
                    .relative()
                    .h(px(ROW))
                    .pl(px(indent))
                    .pr_2()
                    .flex()
                    .items_center()
                    .children(guides);
                if let Some(note) = row.note {
                    return Some(
                        base.child(div().w(px(CHEVRON + GAP)).flex_none())
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .italic()
                                    .text_color(if self.errors.contains_key(&row.path) {
                                        c.destructive
                                    } else {
                                        c.muted_foreground.opacity(0.8)
                                    })
                                    .child(note),
                            )
                            .into_any_element(),
                    );
                }
                let change = if row.is_dir { None } else { store.file_change(&row.path) };
                // A folder takes the colour of the most pressing change in
                // it (a folder of new files is red).
                let dir_change = (row.is_dir && !row.root).then(|| store.dir_change(&row.path)).flatten();
                let name_color = match change.or(dir_change) {
                    Some(ch) => change_color(ch, &c),
                    None if row.ignored => IGNORED_COLOR,
                    None => c.foreground,
                };
                let selected = self.selected.as_deref() == Some(row.path.as_path());
                let expanded = row.is_dir && self.expanded.contains(&row.path);
                let chevron = div().w(px(CHEVRON)).flex_none().flex().justify_center().when(row.is_dir, |el| {
                    el.child(
                        Icon::from(if expanded { IconName::ChevronDown } else { IconName::ChevronRight })
                            .size_3()
                            .text_color(c.muted_foreground),
                    )
                });
                let icon = if row.is_dir {
                    Icon::from(if expanded { FOLDER_OPEN } else { FOLDER })
                        .size(px(15.))
                        .text_color(if row.ignored { IGNORED_COLOR.opacity(0.75) } else { folder_color })
                } else {
                    Icon::from(file_icon(&row.name))
                        .size(px(14.))
                        .text_color(if row.ignored { IGNORED_COLOR.opacity(0.75) } else { c.muted_foreground })
                };
                // Right: a file's change letter, or a dot on a folder with
                // changes inside.
                let badge = div()
                    .w(px(14.))
                    .flex_none()
                    .flex()
                    .justify_end()
                    .text_xs()
                    .text_color(name_color)
                    .when_some(change, |el, ch| el.child(ch.letter()))
                    .when(dir_change.is_some(), |el| {
                        el.child(div().size(px(6.)).rounded_full().bg(name_color))
                    });
                let error = self.errors.contains_key(&row.path);
                let r2 = row.clone();
                let el = base
                    .id(SharedString::from(format!("f-{}", row.path.display())))
                    .cursor_pointer()
                    .when(row.root && ix > 0, |el| el.border_t_1().border_color(c.border))
                    .when(selected, |el| {
                        el.bg(if focused { c.selected_row } else { c.selected_row.opacity(0.55) })
                    })
                    .when(!selected, |el| el.hover(|el| el.bg(c.accent.opacity(0.5))))
                    .when(row.ignored, |el| el.opacity(0.8))
                    .child(chevron)
                    .child(div().w(px(GAP)).flex_none())
                    .child(icon)
                    .child(
                        div()
                            .ml(px(6.))
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .text_color(name_color)
                            .when(row.is_dir, |el| el.font_weight(FontWeight::MEDIUM))
                            .when(row.root, |el| el.font_weight(FontWeight::SEMIBOLD))
                            .child(row.name.clone()),
                    )
                    .when(error, |el| el.child(div().text_xs().text_color(c.destructive).mr_1().child("!")))
                    .child(badge)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        window.focus(&this.focus, cx);
                        this.click(&r2, cx);
                    }));
                let row_for_menu = row.clone();
                let entity = menu_entity.clone();
                Some(
                    ContextMenu::new(SharedString::from(format!("fm-{}", row.path.display())), &self.menu)
                        .items(move |_, cx| entity.read(cx).menu_for(&row_for_menu, &entity, cx))
                        .child(el)
                        .into_any_element(),
                )
            })
            .collect()
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let tool = |id: &'static str, icon: IconName, tip: &'static str| {
            Button::new(id).ghost().size(ButtonSize::Sm).icon(Icon::from(icon)).tooltip(tip)
        };
        div()
            .flex_none()
            .flex()
            .items_center()
            .gap_0p5()
            .px_1()
            .py_1()
            .border_b_1()
            .border_color(c.border)
            .child(
                div().flex_1().min_w_0().child(
                    Button::new("files-go-to")
                        .ghost()
                        .size(ButtonSize::Sm)
                        .icon(Icon::from(IconName::Search))
                        .label("Go to File… ⌘P")
                        .tooltip("Open a file by typing part of its name")
                        .on_click(|_, window, cx| window.dispatch_action(Box::new(crate::actions::QuickOpen), cx)),
                ),
            )
            .child(tool("files-new-file", IconName::FilePlus, "New File…").on_click(cx.listener(
                |this, _, _, cx| {
                    if let Some(dir) = this.target_dir() {
                        this.new_entry(dir, false, cx);
                    }
                },
            )))
            .child(tool("files-new-folder", IconName::FolderPlus, "New Folder…").on_click(cx.listener(
                |this, _, _, cx| {
                    if let Some(dir) = this.target_dir() {
                        this.new_entry(dir, true, cx);
                    }
                },
            )))
            .child(
                tool("files-collapse", IconName::ChevronsDownUp, "Collapse All Folders")
                    .on_click(cx.listener(|this, _, _, cx| this.collapse_all(cx))),
            )
            .child(
                tool(
                    "show-ignored",
                    if self.show_ignored { IconName::Eye } else { IconName::EyeOff },
                    if self.show_ignored { "Hide Ignored Files" } else { "Show Ignored Files" },
                )
                .toggled(self.show_ignored)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.show_ignored = !this.show_ignored;
                    for dir in this.expanded.clone() {
                        this.list(dir, cx);
                    }
                    cx.notify();
                })),
            )
            .child(tool("files-refresh", IconName::RefreshCw, "Refresh").on_click(cx.listener(
                |this, _, _, cx| {
                    for dir in this.expanded.clone() {
                        this.list(dir, cx);
                    }
                },
            )))
    }
}

impl Render for FilesPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let count = self.rows.len();
        div()
            .id("files-panel")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| this.on_key(event, cx)))
            .size_full()
            .flex()
            .flex_col()
            .child(self.render_toolbar(cx))
            .child(
                uniform_list(
                    "file-rows",
                    count,
                    cx.processor(|this, range: Range<usize>, window, cx| this.render_rows(range, window, cx)),
                )
                .track_scroll(&self.scroll)
                .py_1()
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
