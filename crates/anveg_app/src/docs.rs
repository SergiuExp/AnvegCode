//! Document tabs on the right side (next phases, 0.1): files, diffs, pull
//! requests and CI runs, next to the Claude and Terminal tabs.
//!
//! Tabs restored on launch are placeholders: their view is created the
//! first time they are shown (lazy restore).

use std::path::PathBuf;

use anveg_core::config::{DocKey, PaneContent};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable as _, IntoElement,
    SharedString, Subscription, Window,
};

use crate::commands_view::CommandsView;
use crate::diff_view::{DiffSource, DiffView};
use crate::editor_view::EditorView;
use crate::github::{CreatePrView, LogView, PrView, RunView};
use crate::project_window::{ProjectWindow, Tab};
use crate::workspace::{ButtonStyle, Prompt, WorkspaceEvent};

/// A loaded document view.
#[derive(Clone)]
pub enum DocView {
    Editor(Entity<EditorView>),
    Diff(Entity<DiffView>),
    Pr(Entity<PrView>),
    Run(Entity<RunView>),
    Log(Entity<LogView>),
    CreatePr(Entity<CreatePrView>),
    Commands(Entity<CommandsView>),
}

impl DocView {
    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        match self {
            Self::Editor(v) => v.read(cx).focus_handle(cx),
            Self::Diff(v) => v.read(cx).focus_handle(cx),
            Self::Pr(v) => v.read(cx).focus_handle(cx),
            Self::Run(v) => v.read(cx).focus_handle(cx),
            Self::Log(v) => v.read(cx).focus_handle(cx),
            Self::CreatePr(v) => v.read(cx).focus_handle(cx),
            Self::Commands(v) => v.read(cx).focus_handle(cx),
        }
    }

    pub fn element(&self) -> AnyElement {
        match self {
            Self::Editor(v) => v.clone().into_any_element(),
            Self::Diff(v) => v.clone().into_any_element(),
            Self::Pr(v) => v.clone().into_any_element(),
            Self::Run(v) => v.clone().into_any_element(),
            Self::Log(v) => v.clone().into_any_element(),
            Self::CreatePr(v) => v.clone().into_any_element(),
            Self::Commands(v) => v.clone().into_any_element(),
        }
    }
}

/// One document tab.
pub struct DocTab {
    pub key: DocKey,
    pub view: Option<DocView>,
    _sub: Option<Subscription>,
}

impl DocTab {
    pub fn placeholder(key: DocKey) -> Self {
        Self {
            key,
            view: None,
            _sub: None,
        }
    }

    pub fn is_dirty(&self, cx: &App) -> bool {
        matches!(&self.view, Some(DocView::Editor(e)) if e.read(cx).is_dirty())
    }

    pub fn title(&self, cx: &App) -> String {
        match (&self.view, &self.key) {
            (Some(DocView::Editor(e)), _) => e.read(cx).title(),
            (Some(DocView::Diff(d)), DocKey::GitDiff { .. } | DocKey::Compare { .. }) => {
                d.read(cx).title()
            }
            (_, key) => key_title(key),
        }
    }
}

/// A tab's title before its view exists.
pub fn key_title(key: &DocKey) -> String {
    let file_name = |p: &std::path::Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| p.display().to_string())
    };
    match key {
        DocKey::File { path } => file_name(path),
        DocKey::GitDiff { path, .. } => {
            format!("{} (diff)", path.rsplit('/').next().unwrap_or(path))
        }
        DocKey::Commit { title, sha, .. } => {
            if title.is_empty() {
                format!("Commit {}", &sha[..sha.len().min(8)])
            } else {
                title.clone()
            }
        }
        DocKey::PullRequest { number, title, .. } => format!("#{number} {title}"),
        DocKey::Run { title, .. } => format!("Run: {title}"),
        DocKey::RunLog { title, .. } => format!("Log: {title}"),
        DocKey::CreatePullRequest { .. } => "New Pull Request".into(),
        DocKey::Commands => "Toolbar Commands".into(),
        DocKey::Compare { path } => format!("{} ↔ disk", file_name(path)),
    }
}

impl ProjectWindow {
    pub fn doc_index(&self, key: &DocKey) -> Option<usize> {
        self.docs.iter().position(|d| &d.key == key)
    }

    /// Open (or show) a document tab.
    pub fn open_doc(&mut self, key: DocKey, window: &mut Window, cx: &mut Context<Self>) {
        let i = match self.doc_index(&key) {
            Some(i) => i,
            None => {
                self.docs.push(DocTab::placeholder(key));
                self.docs.len() - 1
            }
        };
        self.show_doc(i, window, cx);
    }

    /// Open a file, optionally at a line.
    pub fn open_file(
        &mut self,
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let existing = self.docs.iter().position(|d| {
            matches!(&d.key, DocKey::File { path: p } if crate::editor_view::same_file(p, &path))
        });
        let i = match existing {
            Some(i) => i,
            None => {
                self.docs.push(DocTab::placeholder(DocKey::File { path }));
                self.docs.len() - 1
            }
        };
        self.show_doc(i, window, cx);
        if let (Some(line), Some(DocView::Editor(e))) = (line, &self.docs[i].view) {
            e.update(cx, |e, cx| e.jump_to(line, column, window, cx));
        }
    }

    pub fn show_doc(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.docs.len() {
            return;
        }
        self.ensure_doc_view(i, window, cx);
        self.tab = Tab::Doc(i);
        self.show_current(window, cx);
    }

    /// Create a document's view the first time it is shown.
    pub fn ensure_doc_view(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(i) else {
            return;
        };
        if doc.view.is_some() {
            return;
        }
        let key = doc.key.clone();
        let project = self.project_id;
        let roots = self.roots(cx);
        let view = match &key {
            DocKey::File { path } => {
                let path = path.clone();
                DocView::Editor(cx.new(|cx| EditorView::new(path, project, roots, window, cx)))
            }
            DocKey::GitDiff { repo, path, diff } => {
                let source = DiffSource::GitFile {
                    repo: repo.clone(),
                    path: path.clone(),
                    kind: *diff,
                };
                DocView::Diff(cx.new(|cx| DiffView::new(source, window, cx)))
            }
            DocKey::Commit { repo, sha, .. } => {
                let source = DiffSource::Commit {
                    repo: repo.clone(),
                    sha: sha.clone(),
                };
                DocView::Diff(cx.new(|cx| DiffView::new(source, window, cx)))
            }
            DocKey::Compare { path } => {
                let mine = self
                    .docs
                    .iter()
                    .find_map(|d| match (&d.key, &d.view) {
                        (DocKey::File { path: p }, Some(DocView::Editor(e))) if p == path => {
                            Some(e.read(cx).text(cx))
                        }
                        _ => None,
                    })
                    .unwrap_or_default();
                let relative = anveg_core::files::relative_to_roots(path, &roots)
                    .unwrap_or_else(|| path.clone())
                    .display()
                    .to_string();
                let source = DiffSource::Compare {
                    path: path.clone(),
                    relative,
                    mine,
                };
                DocView::Diff(cx.new(|cx| DiffView::new(source, window, cx)))
            }
            DocKey::PullRequest { repo, number, .. } => {
                let (repo, number) = (repo.clone(), *number);
                DocView::Pr(cx.new(|cx| PrView::new(repo, number, window, cx)))
            }
            DocKey::Run { repo, id, title } => {
                let (repo, id, title) = (repo.clone(), *id, title.clone());
                DocView::Run(cx.new(|cx| RunView::new(repo, id, title, window, cx)))
            }
            DocKey::RunLog { repo, id, title } => {
                let (repo, id, title) = (repo.clone(), *id, title.clone());
                DocView::Log(cx.new(|cx| LogView::new(repo, id, title, window, cx)))
            }
            DocKey::Commands => {
                DocView::Commands(cx.new(|cx| CommandsView::new(project, window, cx)))
            }
            DocKey::CreatePullRequest { repo } => {
                let repo = repo.clone();
                DocView::CreatePr(cx.new(|cx| CreatePrView::new(repo, window, cx)))
            }
        };
        let sub = match &view {
            DocView::Editor(v) => cx.subscribe_in(v, window, Self::on_workspace_event),
            DocView::Diff(v) => cx.subscribe_in(v, window, Self::on_workspace_event),
            DocView::Pr(v) => cx.subscribe_in(v, window, Self::on_workspace_event),
            DocView::Run(v) => cx.subscribe_in(v, window, Self::on_workspace_event),
            DocView::Log(v) => cx.subscribe_in(v, window, Self::on_workspace_event),
            DocView::CreatePr(v) => cx.subscribe_in(v, window, Self::on_workspace_event),
            DocView::Commands(v) => cx.subscribe_in(v, window, Self::on_workspace_event),
        };
        if let Some(doc) = self.docs.get_mut(i) {
            doc.view = Some(view);
            doc._sub = Some(sub);
        }
    }

    /// Close document tab `i`, asking first when it has unsaved changes.
    pub fn close_doc(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.docs.get(i) else {
            return;
        };
        if doc.is_dirty(cx) {
            let title = doc.title(cx);
            let key = doc.key.clone();
            let weak = cx.entity().downgrade();
            let weak2 = weak.clone();
            let key2 = key.clone();
            let prompt = Prompt::new(
                format!("Save changes to {title}?"),
                "Your changes are lost if you don't save them.",
            )
            .cancel()
            .button("Don't Save", ButtonStyle::Outline, move |_, window, cx| {
                let _ = weak.update(cx, |this, cx| {
                    if let Some(i) = this.doc_index(&key) {
                        this.remove_doc(i, window, cx);
                    }
                });
                Ok(())
            })
            .button("Save", ButtonStyle::Primary, move |_, window, cx| {
                let _ = weak2.update(cx, |this, cx| {
                    if let Some(i) = this.doc_index(&key2)
                        && let Some(DocView::Editor(e)) = this.docs[i].view.clone()
                    {
                        e.update(cx, |e, cx| e.save(window, cx));
                        this.remove_doc(i, window, cx);
                    }
                });
                Ok(())
            });
            self.show_prompt(prompt, window, cx);
            return;
        }
        self.remove_doc(i, window, cx);
    }

    pub fn remove_doc(&mut self, i: usize, window: &mut Window, cx: &mut Context<Self>) {
        if i >= self.docs.len() {
            return;
        }
        let doc = self.docs.remove(i);
        if let Some(DocView::Editor(e)) = &doc.view {
            e.update(cx, |e, cx| e.closed(cx));
        }
        if matches!(&self.split, Some(PaneContent::Doc { doc: k }) if *k == doc.key) {
            self.split = None;
        }
        self.tab = match self.tab {
            Tab::Doc(t) if t == i => {
                if self.docs.is_empty() {
                    Tab::Claude
                } else {
                    Tab::Doc(i.min(self.docs.len() - 1))
                }
            }
            Tab::Doc(t) if t > i => Tab::Doc(t - 1),
            other => other,
        };
        if let Tab::Doc(t) = self.tab {
            self.ensure_doc_view(t, window, cx);
        }
        self.show_current(window, cx);
    }

    /// ⌘S on a window whose editor is not focused, and File ▸ Save All.
    pub fn save_all(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for doc in &self.docs {
            if let Some(DocView::Editor(e)) = &doc.view
                && e.read(cx).is_dirty()
            {
                e.update(cx, |e, cx| e.save(window, cx));
            }
        }
    }

    pub fn dirty_doc_names(&self, cx: &App) -> Vec<String> {
        self.docs
            .iter()
            .filter(|d| d.is_dirty(cx))
            .map(|d| d.title(cx))
            .collect()
    }

    /// Refresh git-based documents after a git operation.
    pub fn reload_git_docs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for doc in &self.docs {
            if let Some(DocView::Diff(d)) = &doc.view
                && matches!(d.read(cx).source, DiffSource::GitFile { .. })
            {
                d.update(cx, |d, cx| d.reload(window, cx));
            }
        }
    }

    /// Everything a panel or document asks for.
    pub fn on_workspace_event<E>(
        &mut self,
        _: &Entity<E>,
        event: &WorkspaceEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.clone() {
            WorkspaceEvent::OpenFile { path, line, column } => {
                self.open_file(path, line, column, window, cx)
            }
            WorkspaceEvent::OpenDoc(key) => {
                // A compare is rebuilt each time with the latest buffer.
                if matches!(key, DocKey::Compare { .. })
                    && let Some(i) = self.doc_index(&key)
                {
                    self.docs[i] = DocTab::placeholder(key.clone());
                }
                self.open_doc(key, window, cx)
            }
            WorkspaceEvent::CloseDoc(key) => {
                if let Some(i) = self.doc_index(&key) {
                    self.remove_doc(i, window, cx);
                }
            }
            WorkspaceEvent::TitleChanged => {
                self.save_soon(cx);
                cx.notify();
            }
            WorkspaceEvent::TypeIntoClaude(text) => self.type_into_claude(&text, window, cx),
            WorkspaceEvent::RunInTerminal(args) => self.run_in_terminal(args, window, cx),
            WorkspaceEvent::Toast(text) => self.toast(text, cx),
            WorkspaceEvent::Prompt(prompt) => self.show_prompt(prompt, window, cx),
            // The store refreshes, and then the git diff tabs reload.
            WorkspaceEvent::GitChanged => self.git_changed(cx),
            WorkspaceEvent::RunCommand(id) => self.run_project_command(&id, window, cx),
        }
    }

    /// Type `text` into the shown Claude session's prompt (never Enter).
    pub fn type_into_claude(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        let target = self
            .selected
            .clone()
            .filter(|id| self.running.get(id).is_some_and(|r| r.is_live()));
        let Some(id) = target else {
            self.toast(
                SharedString::from("Select a running Claude session first, then try again."),
                cx,
            );
            return;
        };
        if let Some(r) = self.running.get(&id) {
            let view = r.view.clone();
            view.update(cx, |v, cx| v.paste_text(text.to_owned(), cx));
        }
        self.tab = Tab::Claude;
        self.show_current(window, cx);
    }
}
