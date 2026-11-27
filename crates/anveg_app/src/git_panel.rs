//! The Git tab (next phases, Phase 5): status, staging, commit, branches,
//! remotes, history and stash, all through the `git` command-line tool.
//!
//! [`GitStore`] holds each repository's status for the window. It only
//! watches files and runs `git` while a panel that needs it (Files, Git or
//! GitHub) is visible; it never polls.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anveg_core::config::DocKey;
use anveg_core::fswatch;
use anveg_core::gh::{Run, RunState};
use anveg_core::git_activity::{self, ActivityFile, Entry as Activity, RefChange};
use anveg_core::git::{
    self, Branch, Change, Commit, DeleteError, DiffKind, NetworkOp, Section, Stash, StashOp,
    Status, StatusEntry, Worktree,
};
use anveg_core::process::CancelToken;
use anveg_core::time::{now_millis, relative};
use futures::StreamExt as _;
use gpui_cn::prelude::*;
use gpui_cn::{Switch, Textarea, TextareaState};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};
use uuid::Uuid;

use crate::actions::{Commit as CommitAction, CommitAndPush, GIT_PANEL_CONTEXT};
use crate::model::model;
use crate::ui::Colors;
use crate::workspace::{ButtonStyle, Prompt, WorkspaceEvent};

// ---------------------------------------------------------------------
// The store
// ---------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct RepoState {
    pub root: PathBuf,
    pub git_dir: PathBuf,
    pub status: Option<Status>,
    pub worktrees: Vec<Worktree>,
    pub error: Option<String>,
    /// The latest CI run of the current branch (filled by the GitHub tab).
    pub latest_run: Option<Run>,
}

pub struct GitStore {
    project: Uuid,
    pub repos: Vec<RepoState>,
    pub discovered: bool,
    active: bool,
    refreshing: bool,
    again: bool,
    /// The last refresh, in ms (for the acceptance check in the log).
    pub last_refresh_ms: u128,
    watches: Vec<fswatch::Subscription>,
    debounce: Option<Task<()>>,
    /// The Git activity log (pushes, pulls, fetches), oldest first.
    pub activity: Vec<Activity>,
    _tasks: Vec<Task<()>>,
}

impl GitStore {
    pub fn new(project: Uuid, cx: &mut Context<Self>) -> Self {
        let path = model(cx).read(cx).paths.activity_file(&project.to_string());
        let load = cx.background_spawn(async move { ActivityFile::load(&path) });
        let loaded = cx.spawn(async move |this, cx| {
            let file = load.await;
            let _ = this.update(cx, |s, cx| {
                // Entries added while it loaded are newer.
                let mut all = file.entries;
                all.append(&mut s.activity);
                s.activity = all;
                cx.notify();
            });
        });
        Self {
            project,
            repos: Vec::new(),
            discovered: false,
            active: false,
            refreshing: false,
            again: false,
            last_refresh_ms: 0,
            watches: Vec::new(),
            debounce: None,
            activity: Vec::new(),
            _tasks: vec![loaded],
        }
    }

    /// Add a push, pull or fetch to the activity log and save it.
    pub fn add_activity(&mut self, entry: Activity, cx: &mut Context<Self>) {
        self.activity.push(entry);
        let extra = self.activity.len().saturating_sub(git_activity::MAX_ENTRIES);
        self.activity.drain(..extra);
        let file = ActivityFile {
            entries: self.activity.clone(),
        };
        let path = model(cx).read(cx).paths.activity_file(&self.project.to_string());
        cx.background_spawn(async move {
            if let Err(error) = file.save(&path) {
                tracing::warn!(%error, "could not save the git activity log");
            }
        })
        .detach();
        cx.notify();
    }

    /// The project's main folder (where Initialize creates a repository).
    pub fn main_folder(&self, cx: &App) -> Option<PathBuf> {
        self.roots(cx).into_iter().next()
    }

    /// Look for repositories again (one was created, or an inner one was
    /// made part of its parent).
    pub fn rediscover(&mut self, cx: &mut Context<Self>) {
        self.discover(cx);
    }

    fn roots(&self, cx: &App) -> Vec<PathBuf> {
        model(cx)
            .read(cx)
            .projects
            .get(self.project)
            .map(|p| std::iter::once(p.main_folder.clone()).chain(p.extra_folders.clone()).collect())
            .unwrap_or_default()
    }

    /// A panel that needs git became visible (or every one was hidden).
    pub fn set_active(&mut self, active: bool, cx: &mut Context<Self>) {
        if self.active == active {
            return;
        }
        self.active = active;
        if active {
            if self.discovered {
                self.watch(cx);
                self.refresh(cx);
            } else {
                self.discover(cx);
            }
        } else {
            self.watches.clear();
            self.debounce = None;
        }
    }

    pub fn refresh_if_active(&mut self, cx: &mut Context<Self>) {
        if self.active {
            self.refresh(cx);
        }
    }

    fn discover(&mut self, cx: &mut Context<Self>) {
        let roots = self.roots(cx);
        let git = model(cx).read(cx).git();
        let task = cx.background_spawn(async move {
            git.find_repos_with_inner(&roots)
                .into_iter()
                .map(|root| {
                    let git_dir = git.git_dir(&root).unwrap_or_else(|_| root.join(".git"));
                    (root, git_dir)
                })
                .collect::<Vec<_>>()
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let found = task.await;
            let _ = this.update(cx, |this, cx| {
                this.repos = found
                    .into_iter()
                    .map(|(root, git_dir)| RepoState {
                        root,
                        git_dir,
                        status: None,
                        worktrees: Vec::new(),
                        error: None,
                        latest_run: None,
                    })
                    .collect();
                this.discovered = true;
                // (refresh would look for repositories again when none.)
                if this.active && !this.repos.is_empty() {
                    this.watch(cx);
                    this.refresh(cx);
                }
                cx.notify();
            });
        }));
    }

    /// Watch `.git` (index, HEAD, refs) and the working trees.
    fn watch(&mut self, cx: &mut Context<Self>) {
        let Some(service) = model(cx).read(cx).fswatch.clone() else {
            return;
        };
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        let mut watches = Vec::new();
        for repo in &self.repos {
            let git_dir = repo.git_dir.clone();
            let t = tx.clone();
            let gd = git_dir.clone();
            watches.push(service.subscribe(&repo.root, true, move |paths| {
                if paths.iter().any(|p| matters(p, &gd)) {
                    let _ = t.unbounded_send(());
                }
            }));
            // A linked worktree's git dir lives outside its root.
            if !git_dir.starts_with(&repo.root) {
                let t = tx.clone();
                let gd = git_dir.clone();
                watches.push(service.subscribe(&git_dir, true, move |paths| {
                    if paths.iter().any(|p| matters(p, &gd)) {
                        let _ = t.unbounded_send(());
                    }
                }));
            }
        }
        self.watches = watches;
        self._tasks.push(cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                if this.update(cx, |this, cx| this.schedule(cx)).is_err() {
                    break;
                }
            }
        }));
    }

    /// Refresh 300 ms after the last change of a burst (5.3).
    fn schedule(&mut self, cx: &mut Context<Self>) {
        if !self.active {
            return;
        }
        self.debounce = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(300))
                .await;
            let _ = this.update(cx, |this, cx| this.refresh(cx));
        }));
    }

    /// Run `git status` (and `git worktree list`) for every repository.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        // No repository yet: look again (Publish to GitHub may just have
        // run git init).
        if !self.discovered || self.repos.is_empty() {
            self.discover(cx);
            return;
        }
        if self.refreshing {
            self.again = true;
            return;
        }
        self.refreshing = true;
        let git = model(cx).read(cx).git();
        let roots: Vec<PathBuf> = self.repos.iter().map(|r| r.root.clone()).collect();
        let task = cx.background_spawn(async move {
            let started = std::time::Instant::now();
            let results: Vec<(Result<Status, String>, Vec<Worktree>)> = roots
                .iter()
                .map(|root| {
                    (
                        git.status(root).map_err(|e| format!("{e:#}")),
                        git.worktrees(root).unwrap_or_default(),
                    )
                })
                .collect();
            (results, started.elapsed().as_millis())
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let (results, ms) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.refreshing = false;
                this.last_refresh_ms = ms;
                tracing::debug!(ms, "git status refreshed");
                for (repo, (status, worktrees)) in this.repos.iter_mut().zip(results) {
                    match status {
                        Ok(s) => {
                            repo.status = Some(s);
                            repo.error = None;
                        }
                        Err(e) => repo.error = Some(e),
                    }
                    repo.worktrees = worktrees;
                }
                cx.notify();
                // A folder became a repository of its own (git init in
                // it, a clone): list it too.
                let unknown = this.repos.iter().any(|r| {
                    r.status.as_ref().is_some_and(|s| {
                        s.inner_repos().any(|p| {
                            let path = r.root.join(p);
                            path.join(".git").exists() && !this.repos.iter().any(|o| o.root == path)
                        })
                    })
                });
                if unknown {
                    this.discover(cx);
                    return;
                }
                if std::mem::take(&mut this.again) {
                    this.refresh(cx);
                }
            });
        }));
    }

    /// The change of a file, for the file tree's colours.
    pub fn file_change(&self, path: &Path) -> Option<Change> {
        let repo = self
            .repos
            .iter()
            .filter(|r| path.starts_with(&r.root))
            .max_by_key(|r| r.root.as_os_str().len())?;
        let rel = path.strip_prefix(&repo.root).ok()?.to_string_lossy();
        repo.status.as_ref()?.file_change(&rel)
    }

    /// The innermost repository `path` is in, and `path` relative to it
    /// ("." for the root itself).
    fn repo_and_rel(&self, path: &Path) -> Option<(&RepoState, String)> {
        let repo = self
            .repos
            .iter()
            .filter(|r| path.starts_with(&r.root))
            .max_by_key(|r| r.root.as_os_str().len())?;
        let rel = path.strip_prefix(&repo.root).ok()?.to_string_lossy().into_owned();
        Some((repo, if rel.is_empty() { ".".into() } else { rel }))
    }

    /// The change a folder is coloured by (the most pressing under it).
    pub fn dir_change(&self, dir: &Path) -> Option<Change> {
        let (repo, rel) = self.repo_and_rel(dir)?;
        repo.status.as_ref()?.folder_change(&rel)
    }

    /// What Add to Git would do for a file or folder; `None` while the
    /// repository's status is not known yet.
    pub fn addable(&self, path: &Path) -> Option<anveg_core::git::Addable> {
        let (repo, rel) = self.repo_and_rel(path)?;
        Some(repo.status.as_ref()?.addable(&rel))
    }
}

/// Whether a changed path can change `git status`: working-tree files, and
/// in the git dir only the index, HEAD and refs (not objects or logs).
fn matters(path: &Path, git_dir: &Path) -> bool {
    match path.strip_prefix(git_dir) {
        Ok(rel) => {
            let s = rel.to_string_lossy();
            s == "index"
                || s == "HEAD"
                || s == "packed-refs"
                || s == "MERGE_HEAD"
                || s.starts_with("refs")
        }
        Err(_) => true,
    }
}

// ---------------------------------------------------------------------
// The panel
// ---------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Changes,
    Branches,
    History,
    Stash,
    Activity,
}

struct NetworkJob {
    label: String,
    progress: String,
    cancel: CancelToken,
}

/// Untracked files git ignores (Changes ▸ Ignored ▸ Show).
struct IgnoredList {
    paths: Vec<String>,
    total: usize,
    error: Option<String>,
}

/// How many ignored entries are listed.
const IGNORED_SHOWN: usize = 300;

pub struct GitPanel {
    store: Entity<GitStore>,
    /// `None` while hidden; loaded when shown and after each change.
    ignored: Option<IgnoredList>,
    show_ignored: bool,
    repo: usize,
    view: View,
    /// The commit message: one field, its first line the summary.
    message: Entity<TextareaState>,
    /// The repository whose draft the message field holds.
    draft_repo: Option<PathBuf>,
    /// Filling the field from code (a draft, Recent, Amend), not typing.
    filling: bool,
    recent_menu: Entity<gpui_cn::MenuState>,
    /// Rows left out of the next commit (unchecked), by [`row_key`]. Every
    /// row is checked unless listed here.
    excluded: std::collections::HashSet<String>,
    amend: bool,
    busy: Option<String>,
    network: Option<NetworkJob>,
    branches: Vec<Branch>,
    commits: Vec<Commit>,
    stashes: Vec<Stash>,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
    _subs: Vec<gpui_kit::Subscription>,
}

impl EventEmitter<WorkspaceEvent> for GitPanel {}

impl Focusable for GitPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl GitPanel {
    pub fn new(store: Entity<GitStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let message = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Commit message\n\nThe first line is the summary.")
                .auto_grow(3, 12)
        });
        let recent_menu = cx.new(gpui_cn::MenuState::new);
        // The shown repository's unfinished message, once the panel exists.
        cx.defer_in(window, |this: &mut Self, window, cx| this.sync_draft(window, cx));
        let subs = vec![
            cx.observe_in(&store, window, |this, _, window, cx| {
                this.sync_draft(window, cx);
                this.prune_excluded(cx);
                cx.notify();
            }),
            cx.subscribe_in(&message, window, |this, state, event, window, cx| match event {
                // ⌘↩ commits, ⇧⌘↩ commits and pushes (the field takes them first).
                gpui_cn::InputEvent::PressEnter { secondary: true, shift } => this.commit(*shift, window, cx),
                gpui_cn::InputEvent::Change => {
                    if !this.filling
                        && let Some(repo) = this.draft_repo.clone()
                    {
                        let text = state.read(cx).value().to_string();
                        model(cx).update(cx, |m, cx| m.set_commit_draft(&repo, &text, cx));
                    }
                    cx.notify();
                }
                _ => {}
            }),
        ];
        Self {
            store,
            ignored: None,
            show_ignored: false,
            repo: 0,
            view: View::Changes,
            message,
            draft_repo: None,
            filling: false,
            recent_menu,
            excluded: std::collections::HashSet::new(),
            amend: false,
            busy: None,
            network: None,
            branches: Vec::new(),
            commits: Vec::new(),
            stashes: Vec::new(),
            focus: cx.focus_handle(),
            _tasks: Vec::new(),
            _subs: subs,
        }
    }

    fn repo_state(&self, cx: &App) -> Option<RepoState> {
        self.store.read(cx).repos.get(self.repo).cloned()
    }

    fn repo_root(&self, cx: &App) -> Option<PathBuf> {
        self.repo_state(cx).map(|r| r.root)
    }

    /// Run a git command in the background, then refresh.
    fn run(
        &mut self,
        label: &str,
        op: impl FnOnce(git::Git, PathBuf) -> anyhow::Result<()> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(root) = self.repo_root(cx) else {
            return;
        };
        let git = model(cx).read(cx).git();
        self.busy = Some(label.to_owned());
        cx.notify();
        let task = cx.background_spawn(async move { op(git, root) });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                if let Err(e) = result {
                    this.report(e, cx);
                }
                this.after_change(window, cx);
            });
        }));
    }

    fn report(&mut self, e: anyhow::Error, cx: &mut Context<Self>) {
        cx.emit(WorkspaceEvent::Prompt(Prompt::message(
            "Git reported a problem",
            format!("{e:#}"),
        )));
    }

    fn after_change(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        cx.emit(WorkspaceEvent::GitChanged);
        self.load_view(window, cx);
        if self.show_ignored {
            self.load_ignored(cx);
        }
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Ignored files
    // -----------------------------------------------------------------

    fn toggle_ignored(&mut self, cx: &mut Context<Self>) {
        self.show_ignored = !self.show_ignored;
        if self.show_ignored {
            self.load_ignored(cx);
        } else {
            self.ignored = None;
        }
        cx.notify();
    }

    fn load_ignored(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root(cx) else {
            return;
        };
        let git = model(cx).read(cx).git();
        let task = cx.background_spawn(async move { git.ignored(&root, IGNORED_SHOWN) });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if !this.show_ignored {
                    return;
                }
                this.ignored = Some(match result {
                    Ok((paths, total)) => IgnoredList {
                        paths,
                        total,
                        error: None,
                    },
                    Err(e) => IgnoredList {
                        paths: Vec::new(),
                        total: 0,
                        error: Some(format!("{e:#}")),
                    },
                });
                cx.notify();
            });
        }));
    }

    /// Stage an ignored path anyway (`git add -f`); a folder asks first.
    fn add_ignored(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if !path.ends_with('/') {
            self.run(
                "Adding…",
                move |git, root| git.stage_force(&root, &[&path]),
                window,
                cx,
            );
            return;
        }
        let weak = cx.entity().downgrade();
        let prompt = Prompt::new(
            format!("Add the folder {path} to git?"),
            format!(
                "{path} and everything in it is ignored by .gitignore, so this adds every file inside \
                 (folders like build/ or node_modules/ can hold thousands). You can unstage them again \
                 before committing."
            ),
        )
        .cancel()
        .button("Add Anyway", ButtonStyle::Primary, move |_, window, cx| {
            let p = path.clone();
            let _ = weak.update(cx, |this, cx| {
                this.run("Adding…", move |git, root| git.stage_force(&root, &[&p]), window, cx)
            });
            Ok(())
        });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn render_ignored(&mut self, root: &std::path::Path, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let mut section = div().flex().flex_col().child(
            div()
                .flex()
                .items_center()
                .px_2()
                .pt_1()
                .text_xs()
                .text_color(c.muted_foreground)
                .child(div().flex_1().child(match &self.ignored {
                    Some(list) if list.error.is_none() => format!("Ignored by .gitignore ({})", list.total),
                    _ => "Ignored by .gitignore".to_owned(),
                }))
                .child(
                    Button::new("ignored-toggle")
                        .ghost()
                        .size(ButtonSize::Sm)
                        .label(if self.show_ignored { "Hide" } else { "Show" })
                        .tooltip("New files that .gitignore ignores: they are never listed above")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_ignored(cx))),
                ),
        );
        if !self.show_ignored {
            return section.into_any_element();
        }
        let Some(list) = &self.ignored else {
            return section
                .child(div().px_2().text_xs().text_color(c.muted_foreground).child("Looking…"))
                .into_any_element();
        };
        if let Some(e) = &list.error {
            return section
                .child(div().px_2().text_xs().text_color(c.destructive).child(e.clone()))
                .into_any_element();
        }
        if list.paths.is_empty() {
            return section
                .child(div().px_2().text_xs().text_color(c.muted_foreground).child("Nothing is ignored."))
                .into_any_element();
        }
        for (i, path) in list.paths.clone().into_iter().enumerate() {
            let folder = path.ends_with('/');
            let trimmed = path.trim_end_matches('/');
            let (dir, name) = match trimmed.rsplit_once('/') {
                Some((d, n)) => (d.to_owned(), n.to_owned()),
                None => (String::new(), trimmed.to_owned()),
            };
            let name = if folder { format!("{name}/") } else { name };
            let add = path.clone();
            let open = root.join(trimmed);
            section = section.child(
                div()
                    .id(SharedString::from(format!("ign-{i}")))
                    .h(px(26.))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_1()
                    .when(!folder, |el| el.cursor_pointer())
                    .hover(|el| el.bg(c.accent.opacity(0.5)))
                    .child(div().w(px(14.)).text_xs().text_color(IGNORED_COLOR).child("I"))
                    .child(div().text_sm().text_color(IGNORED_COLOR).child(name))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child(dir),
                    )
                    .child(
                        Button::new(SharedString::from(format!("ign-{i}-add")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .icon(Icon::from(gpui_kit::assets::IconName::Plus))
                            .tooltip("Add Anyway (git add -f)")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                this.add_ignored(add.clone(), window, cx)
                            })),
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if !folder {
                            cx.emit(WorkspaceEvent::OpenFile {
                                path: open.clone(),
                                line: None,
                                column: None,
                            });
                        }
                    })),
            );
        }
        if list.total > list.paths.len() {
            section = section.child(
                div()
                    .px_2()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(format!("and {} more", list.total - list.paths.len())),
            );
        }
        section.into_any_element()
    }

    // -----------------------------------------------------------------
    // Changes
    // -----------------------------------------------------------------

    fn open_diff(&self, entry: &StatusEntry, cx: &mut Context<Self>) {
        let Some(repo) = self.repo_root(cx) else {
            return;
        };
        let diff = match entry.section {
            Section::Staged => DiffKind::Staged,
            Section::Untracked => DiffKind::Untracked,
            Section::Changes => DiffKind::Unstaged,
            Section::Conflicts => {
                cx.emit(WorkspaceEvent::OpenFile {
                    path: repo.join(&entry.path),
                    line: None,
                    column: None,
                });
                return;
            }
        };
        cx.emit(WorkspaceEvent::OpenDoc(DocKey::GitDiff {
            repo,
            path: entry.path.clone(),
            diff,
        }));
    }

    fn stage(&mut self, paths: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.run(
            "Staging…",
            move |git, root| {
                let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
                git.stage(&root, &refs)
            },
            window,
            cx,
        );
    }

    fn unstage(&mut self, paths: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.run(
            "Unstaging…",
            move |git, root| {
                let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
                git.unstage(&root, &refs)
            },
            window,
            cx,
        );
    }

    fn discard(&mut self, entry: StatusEntry, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let untracked = entry.section == Section::Untracked;
        let body = if untracked {
            format!("{} is moved to the Trash.", entry.path)
        } else {
            format!(
                "Your changes to {} are thrown away. A copy of the current file goes to the Trash first, so you can get it back.",
                entry.path
            )
        };
        let prompt = Prompt::new("Discard changes?", body).cancel().button(
            "Discard",
            ButtonStyle::Destructive,
            move |_, window, cx| {
                let entry = entry.clone();
                let _ = weak.update(cx, |this, cx| {
                    this.run(
                        "Discarding…",
                        move |git, root| {
                            let path = root.join(&entry.path);
                            if entry.section == Section::Untracked {
                                trash::delete(&path)?;
                                Ok(())
                            } else {
                                git::copy_to_trash(&path)?;
                                git.restore_worktree(&root, &[entry.path.as_str()])
                            }
                        },
                        window,
                        cx,
                    )
                });
                Ok(())
            },
        );
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    /// Put `text` in the message field (not as typing: the draft is
    /// saved separately).
    fn fill_message(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.filling = true;
        let text = text.to_owned();
        self.message.update(cx, |m, cx| m.set_value(text, window, cx));
        self.filling = false;
    }

    /// When the shown repository changed, show its unfinished message.
    fn sync_draft(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let root = self.repo_root(cx);
        if root == self.draft_repo {
            return;
        }
        let draft = root
            .as_deref()
            .map(|r| model(cx).read(cx).commit_messages.draft(r).to_owned())
            .unwrap_or_default();
        self.draft_repo = root;
        self.fill_message(&draft, window, cx);
    }

    /// Recent ▸ a message: put it in the field (and keep it as the draft).
    fn use_recent(&mut self, text: String, window: &mut Window, cx: &mut Context<Self>) {
        self.fill_message(&text, window, cx);
        if let Some(repo) = self.draft_repo.clone() {
            model(cx).update(cx, |m, cx| m.set_commit_draft(&repo, &text, cx));
        }
        cx.notify();
    }

    fn is_checked(&self, section: Section, path: &str) -> bool {
        !self.excluded.contains(&row_key(section, path))
    }

    fn set_checked(&mut self, section: Section, paths: &[String], on: bool, cx: &mut Context<Self>) {
        for p in paths {
            let key = row_key(section, p);
            if on {
                self.excluded.remove(&key);
            } else {
                self.excluded.insert(key);
            }
        }
        cx.notify();
    }

    /// Forget unchecked rows that are gone (committed, discarded, …).
    fn prune_excluded(&mut self, cx: &App) {
        if self.excluded.is_empty() {
            return;
        }
        let Some(status) = self.repo_state(cx).and_then(|r| r.status) else {
            return;
        };
        let present: std::collections::HashSet<String> =
            status.entries.iter().map(|e| row_key(e.section, &e.path)).collect();
        self.excluded.retain(|k| present.contains(k));
    }

    /// What Commit would do with the checked rows.
    fn selection(&self, status: &Status) -> Selection {
        let rows = |section| status.section(section).filter(|e| !e.is_inner_repo());
        let mut sel = Selection::default();
        let mut checked = std::collections::BTreeSet::new();
        let mut checked_unstaged = std::collections::HashSet::new();
        for section in [Section::Changes, Section::Untracked] {
            for e in rows(section) {
                if self.is_checked(section, &e.path) {
                    sel.stage = true;
                    checked_unstaged.insert(e.path.clone());
                    checked.insert(e.path.clone());
                } else {
                    sel.skip.push(e.path.clone());
                }
            }
        }
        for e in rows(Section::Staged) {
            if self.is_checked(Section::Staged, &e.path) {
                checked.insert(e.path.clone());
            } else if !checked_unstaged.contains(&e.path) {
                // Its unstaged part is checked: the whole file goes in.
                sel.leave_out.push(e.path.clone());
                sel.leave_out.extend(e.orig_path.clone());
            }
        }
        sel.count = checked.len();
        sel
    }

    /// Commit the checked files; with `push`, push once the commit is made.
    fn commit(&mut self, push: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(status) = self.repo_state(cx).and_then(|r| r.status) else {
            return;
        };
        let selection = self.selection(&status);
        if selection.count == 0 && !self.amend {
            cx.emit(WorkspaceEvent::Toast("Check the files to commit first.".into()));
            return;
        }
        let text = self.message.read(cx).value().to_string();
        if anveg_core::commit_messages::summary_line(&text).is_empty() {
            cx.emit(WorkspaceEvent::Toast(
                "Write a commit message first (its first line is the summary).".into(),
            ));
            return;
        }
        // Trailing blank lines and spaces go; the rest is kept as typed.
        let message = text.trim().to_owned();
        let amend = self.amend;
        let Some(root) = self.repo_root(cx) else {
            return;
        };
        let committed_root = root.clone();
        let committed_message = message.clone();
        let git = model(cx).read(cx).git();
        self.busy = Some("Committing…".into());
        cx.notify();
        let task = cx.background_spawn(async move {
            git.commit_selected(
                &root,
                &message,
                amend,
                selection.stage,
                &selection.skip,
                &selection.leave_out,
            )
        });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(_) => {
                        model(cx).update(cx, |m, cx| m.commit_recorded(&committed_root, &committed_message, cx));
                        if this.draft_repo.as_deref() == Some(committed_root.as_path()) {
                            this.fill_message("", window, cx);
                        }
                        this.amend = false;
                        if push {
                            cx.emit(WorkspaceEvent::Toast("Committed. Pushing…".into()));
                            this.network_op(NetworkOp::Push { set_upstream: None }, window, cx);
                        } else {
                            cx.emit(WorkspaceEvent::Toast("Committed.".into()));
                        }
                    }
                    // Hook output (pre-commit and friends) is in the error.
                    Err(e) => cx.emit(WorkspaceEvent::Prompt(Prompt::message(
                        "The commit failed",
                        format!("{e:#}"),
                    ))),
                }
                this.after_change(window, cx);
            });
        }));
    }

    fn toggle_amend(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.amend = on;
        if on && self.message.read(cx).value().trim().is_empty() {
            let Some(root) = self.repo_root(cx) else {
                return;
            };
            let git = model(cx).read(cx).git();
            let task = cx.background_spawn(async move { git.last_commit_message(&root) });
            self._tasks.push(cx.spawn_in(window, async move |this, cx| {
                if let Ok(message) = task.await {
                    let _ = this.update_in(cx, |this, window, cx| {
                        let message = message.trim_end().to_owned();
                        this.use_recent(message, window, cx);
                    });
                }
            }));
        }
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Network (5.7)
    // -----------------------------------------------------------------

    /// Update Project: pull the branch's upstream (with the pull setting:
    /// fast-forward only, merge or rebase); with no upstream there is
    /// nothing to pull into, so only fetch.
    fn update_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let has_upstream = self
            .repo_state(cx)
            .and_then(|r| r.status)
            .is_some_and(|s| s.branch.upstream.is_some());
        let op = if has_upstream {
            NetworkOp::Pull {
                mode: model(cx).read(cx).settings.pull_mode,
            }
        } else {
            NetworkOp::Fetch
        };
        self.network_op(op, window, cx);
    }

    fn network_op(&mut self, op: NetworkOp, window: &mut Window, cx: &mut Context<Self>) {
        let Some(repo) = self.repo_state(cx) else {
            return;
        };
        if self.network.is_some() {
            return;
        }
        let git = model(cx).read(cx).git();
        let label = match &op {
            NetworkOp::Fetch | NetworkOp::Pull { .. } => "Updating",
            NetworkOp::Push { .. } => "Pushing",
        }
        .to_owned();
        let done = match &op {
            NetworkOp::Fetch | NetworkOp::Pull { .. } => "Project updated",
            NetworkOp::Push { .. } => "Pushed",
        };
        // The first push publishes the branch.
        let op = match op {
            NetworkOp::Push { .. } => {
                let has_upstream = repo
                    .status
                    .as_ref()
                    .is_some_and(|s| s.branch.upstream.is_some());
                if has_upstream {
                    NetworkOp::Push { set_upstream: None }
                } else {
                    let remotes = git.remotes(&repo.root).unwrap_or_default();
                    let remote = remotes
                        .iter()
                        .find(|r| *r == "origin")
                        .or(remotes.first())
                        .cloned();
                    match remote {
                        Some(r) => NetworkOp::Push {
                            set_upstream: Some(r),
                        },
                        None => {
                            cx.emit(WorkspaceEvent::Prompt(Prompt::message(
                                "No remote",
                                "This repository has no remote to push to.",
                            )));
                            return;
                        }
                    }
                }
            }
            other => other,
        };
        let cancel = CancelToken::new();
        self.network = Some(NetworkJob {
            label: format!("{label}…"),
            progress: String::new(),
            cancel: cancel.clone(),
        });
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
        let progress: anveg_core::process::Progress = Arc::new(move |line: &str| {
            let _ = tx.unbounded_send(line.to_owned());
        });
        let root = repo.root.clone();
        let task = cx.background_spawn(async move {
            git_activity::run_logged(&git, &root, &op, &cancel, Some(progress))
        });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            while let Some(line) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    if let Some(job) = &mut this.network {
                        job.progress = line;
                        cx.notify();
                    }
                });
            }
        }));
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let (result, entry) = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.network = None;
                let summary = activity_summary(&entry);
                this.store.update(cx, |s, cx| s.add_activity(entry, cx));
                match result {
                    Ok(_) => cx.emit(WorkspaceEvent::Toast(
                        format!("{done}: {summary}. See Git ▸ Activity.").into(),
                    )),
                    Err(e) if anveg_core::process::is_cancelled(&e) => {
                        cx.emit(WorkspaceEvent::Toast(format!("{label} cancelled.").into()))
                    }
                    Err(e) => cx.emit(WorkspaceEvent::Prompt(Prompt::message(
                        match label.as_str() {
                            "Updating" => "Updating the project failed".to_owned(),
                            _ => format!("{label} failed"),
                        },
                        format!("{e:#}"),
                    ))),
                }
                this.after_change(window, cx);
            });
        }));
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Branches, history, stash
    // -----------------------------------------------------------------

    fn show_view(&mut self, view: View, window: &mut Window, cx: &mut Context<Self>) {
        self.view = view;
        self.load_view(window, cx);
        cx.notify();
    }

    fn load_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root(cx) else {
            return;
        };
        let git = model(cx).read(cx).git();
        match self.view {
            View::Changes => {}
            View::Branches => {
                let task = cx.background_spawn(async move { git.branches(&root) });
                self._tasks.push(cx.spawn_in(window, async move |this, cx| {
                    let r = task.await;
                    let _ = this.update(cx, |this, cx| {
                        this.branches = r.unwrap_or_default();
                        cx.notify();
                    });
                }));
            }
            View::History => {
                let task = cx.background_spawn(async move { git.log(&root, 200) });
                self._tasks.push(cx.spawn_in(window, async move |this, cx| {
                    let r = task.await;
                    let _ = this.update(cx, |this, cx| {
                        this.commits = r.unwrap_or_default();
                        cx.notify();
                    });
                }));
            }
            View::Activity => {}
            View::Stash => {
                let task = cx.background_spawn(async move { git.stashes(&root) });
                self._tasks.push(cx.spawn_in(window, async move |this, cx| {
                    let r = task.await;
                    let _ = this.update(cx, |this, cx| {
                        this.stashes = r.unwrap_or_default();
                        cx.notify();
                    });
                }));
            }
        }
    }

    fn checkout(&mut self, branch: Branch, window: &mut Window, cx: &mut Context<Self>) {
        // git's own message explains what blocks a checkout.
        self.run("Switching…", move |git, root| git.switch(&root, &branch), window, cx);
    }

    fn new_branch(&mut self, start: Option<String>, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let from = start.clone().unwrap_or_else(|| "the current branch".into());
        let prompt = Prompt::new("New Branch", format!("Creates a branch from {from} and switches to it."))
            .input("Branch name", "")
            .cancel()
            .button("Create", ButtonStyle::Primary, move |value, window, cx| {
                let name = value.unwrap_or_default().trim().to_owned();
                if name.is_empty() {
                    return Err("Enter a name.".into());
                }
                let start = start.clone();
                let ok = weak
                    .update(cx, |this, cx| {
                        let valid = this.repo_root(cx).is_some_and(|root| {
                            model(cx).read(cx).git().check_branch_name(&root, &name)
                        });
                        if valid {
                            this.run(
                                "Creating branch…",
                                move |git, root| git.create_branch(&root, &name, start.as_deref()),
                                window,
                                cx,
                            );
                        }
                        valid
                    })
                    .unwrap_or(false);
                if ok {
                    Ok(())
                } else {
                    Err("That isn't a valid branch name.".into())
                }
            });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn rename_branch(&mut self, branch: Branch, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let prompt = Prompt::new("Rename Branch", format!("Renames {}.", branch.name))
            .input("New name", branch.name.clone())
            .cancel()
            .button("Rename", ButtonStyle::Primary, move |value, window, cx| {
                let to = value.unwrap_or_default().trim().to_owned();
                if to.is_empty() {
                    return Err("Enter a name.".into());
                }
                let from = branch.name.clone();
                let _ = weak.update(cx, |this, cx| {
                    this.run(
                        "Renaming…",
                        move |git, root| git.rename_branch(&root, &from, &to),
                        window,
                        cx,
                    )
                });
                Ok(())
            });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn delete_branch(&mut self, branch: Branch, force: bool, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let (title, body, label) = if force {
            (
                format!("Force-delete {}?", branch.name),
                "This branch has commits that are not merged anywhere. Deleting it loses them (unless you have them elsewhere).".to_owned(),
                "Delete Anyway",
            )
        } else {
            (
                format!("Delete {}?", branch.name),
                "The branch is deleted. Its commits stay if another branch has them.".to_owned(),
                "Delete",
            )
        };
        let prompt = Prompt::new(title, body).cancel().button(
            label,
            ButtonStyle::Destructive,
            move |_, window, cx| {
                let branch = branch.clone();
                let _ = weak.update(cx, |this, cx| this.do_delete(branch, force, window, cx));
                Ok(())
            },
        );
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn do_delete(&mut self, branch: Branch, force: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root(cx) else {
            return;
        };
        let git = model(cx).read(cx).git();
        let name = branch.name.clone();
        let task = cx.background_spawn(async move { git.delete_branch(&root, &name, force) });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                match result {
                    Ok(()) => {}
                    // Unmerged: ask again, explicitly (5.7).
                    Err(e) if e.downcast_ref::<DeleteError>().is_some() => {
                        this.delete_branch(branch, true, cx)
                    }
                    Err(e) => this.report(e, cx),
                }
                this.after_change(window, cx);
            });
        }));
    }

    fn stash_op(&mut self, stash: Stash, op: StashOp, cx: &mut Context<Self>, window: &mut Window) {
        if op == StashOp::Drop {
            let weak = cx.entity().downgrade();
            let prompt = Prompt::new(
                format!("Drop {}?", stash.name),
                format!("“{}” is deleted.", stash.message),
            )
            .cancel()
            .button("Drop", ButtonStyle::Destructive, move |_, window, cx| {
                let name = stash.name.clone();
                let _ = weak.update(cx, |this, cx| {
                    this.run(
                        "Dropping…",
                        move |git, root| git.stash_op(&root, StashOp::Drop, &name),
                        window,
                        cx,
                    )
                });
                Ok(())
            });
            cx.emit(WorkspaceEvent::Prompt(prompt));
            return;
        }
        let name = stash.name.clone();
        self.run("Applying stash…", move |git, root| git.stash_op(&root, op, &name), window, cx);
    }

    fn stash_push(&mut self, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let prompt = Prompt::new(
            "Stash Changes",
            "Saves your changes (including untracked files) and cleans the working tree.",
        )
        .input("Message (optional)", "")
        .cancel()
        .button("Stash", ButtonStyle::Primary, move |value, window, cx| {
            let message = value.unwrap_or_default();
            let _ = weak.update(cx, |this, cx| {
                this.run(
                    "Stashing…",
                    move |git, root| git.stash_push(&root, Some(&message)),
                    window,
                    cx,
                )
            });
            Ok(())
        });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    // -----------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------

    fn render_header(&mut self, repo: &RepoState, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let name = repo
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let branch = repo.status.as_ref().map(|s| &s.branch);
        let head = branch
            .and_then(|b| b.head.clone())
            .unwrap_or_else(|| "(detached)".into());
        let ab = branch
            .map(|b| {
                let mut parts = Vec::new();
                if b.ahead > 0 {
                    parts.push(format!("↑{}", b.ahead));
                }
                if b.behind > 0 {
                    parts.push(format!("↓{}", b.behind));
                }
                if b.upstream.is_none() {
                    parts.push("not published".into());
                }
                parts.join(" ")
            })
            .unwrap_or_default();
        let ci = repo.latest_run.as_ref().map(|r| {
            let (text, color) = match r.state() {
                RunState::Success => ("CI ✓", gpui_kit::hsla(0.33, 0.6, 0.45, 1.0)),
                RunState::Failure => ("CI ✗", c.destructive),
                RunState::Pending => ("CI …", c.muted_foreground),
                RunState::Cancelled | RunState::Skipped => ("CI –", c.muted_foreground),
            };
            div()
                .text_xs()
                .text_color(color)
                .child(text)
        });
        let repos = self.store.read(cx).repos.len();
        let busy = self.busy.is_some() || self.network.is_some();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(c.border)
            .when(repos > 1, |el| {
                let mut row = div().flex().flex_wrap().gap_1();
                for (i, r) in self.store.read(cx).repos.iter().enumerate() {
                    let label = r
                        .root
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    row = row.child(
                        Button::new(SharedString::from(format!("repo-{i}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .label(label)
                            .toggled(i == self.repo).selected(i == self.repo)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.repo = i;
                                this.load_view(window, cx);
                                if this.show_ignored {
                                    this.ignored = None;
                                    this.load_ignored(cx);
                                }
                                cx.notify();
                            })),
                    );
                }
                el.child(row)
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .child(div().min_w_0().truncate().child(format!("{name} · {head}")))
                    .child(div().text_xs().text_color(c.muted_foreground).child(ab))
                    .children(ci),
            )
            .child(
                div()
                    .flex()
                    .gap_1()
                    .child(
                        Button::new("git-update")
                            .ghost()
                            .size(ButtonSize::Sm)
                            .icon(Icon::from(gpui_kit::assets::IconName::ArrowDown))
                            .label("Update Project")
                            .tooltip("Get the latest changes from the remote (git pull; only fetch when the branch has no upstream)")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.update_project(window, cx))),
                    )
                    .child(
                        Button::new("git-push")
                            .ghost()
                            .size(ButtonSize::Sm)
                            .icon(Icon::from(gpui_kit::assets::IconName::ArrowUp))
                            .label("Push")
                            .tooltip("Send your commits to the remote (git push)")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.network_op(NetworkOp::Push { set_upstream: None }, window, cx)
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("git-refresh")
                            .ghost()
                            .size(ButtonSize::Sm)
                            .icon(Icon::from(gpui_kit::assets::IconName::RefreshCw))
                            .tooltip("Refresh")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.store.update(cx, |s, cx| s.refresh(cx));
                                this.load_view(window, cx);
                            })),
                    ),
            )
            .children(self.network.as_ref().map(|job| {
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .child(format!("{} {}", job.label, job.progress)),
                    )
                    .child(
                        Button::new("net-cancel")
                            .outline()
                            .size(ButtonSize::Sm)
                            .label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(job) = &this.network {
                                    job.cancel.cancel();
                                }
                                cx.notify();
                            })),
                    )
            }))
            .children(self.busy.clone().map(|b| {
                div().text_xs().text_color(c.muted_foreground).child(b)
            }))
            .into_any_element()
    }

    fn render_views(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut row = div().flex().flex_wrap().gap_0p5().px_1().py_0p5();
        for (view, label) in [
            (View::Changes, "Changes"),
            (View::Branches, "Branches"),
            (View::History, "History"),
            (View::Stash, "Stash"),
            (View::Activity, "Activity"),
        ] {
            row = row.child(
                Button::new(SharedString::from(format!("gv-{label}")))
                    .ghost()
                    .size(ButtonSize::Sm)
                    .label(label)
                    .toggled(self.view == view).selected(self.view == view)
                    .on_click(cx.listener(move |this, _, window, cx| this.show_view(view, window, cx))),
            );
        }
        row.into_any_element()
    }

    fn render_commit_box(&mut self, status: &Status, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let text = self.message.read(cx).value().to_string();
        let summary_len = anveg_core::commit_messages::summary_line(&text).chars().count();
        let recent: Vec<String> = self
            .draft_repo
            .as_deref()
            .map(|r| model(cx).read(cx).commit_messages.recent(r).to_vec())
            .unwrap_or_default();
        let weak = cx.entity().downgrade();
        let selection = self.selection(status);
        let busy = self.busy.is_some();
        div()
            .flex()
            .flex_col()
            .gap_1()
            .px_2()
            .py_1()
            .border_b_1()
            .border_color(c.border)
            .child(Textarea::new(&self.message))
            .when(summary_len > 72, |el| {
                el.child(
                    div()
                        .text_xs()
                        .text_color(c.destructive)
                        .child(format!("The first line is {summary_len} characters; 72 or fewer reads best.")),
                )
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Switch::new("amend")
                            .checked(self.amend)
                            .on_change(cx.listener(|this, on: &bool, window, cx| {
                                this.toggle_amend(*on, window, cx)
                            })),
                    )
                    .child(div().text_xs().child("Amend"))
                    .child(
                        gpui_cn::DropdownMenu::new("recent-messages", &self.recent_menu)
                            .trigger(
                                Button::new("recent-trigger")
                                    .ghost()
                                    .size(ButtonSize::Xs)
                                    .icon(Icon::from(gpui_kit::assets::IconName::ChevronDown))
                                    .label("Recent")
                                    .tooltip("Use a message you committed before")
                                    .disabled(recent.is_empty()),
                            )
                            .items(move |_, _| {
                                recent
                                    .iter()
                                    .enumerate()
                                    .map(|(i, m)| {
                                        let weak = weak.clone();
                                        let full = m.clone();
                                        let mut label: String =
                                            anveg_core::commit_messages::summary_line(m).chars().take(60).collect();
                                        if anveg_core::commit_messages::summary_line(m).chars().count() > 60 {
                                            label.push('…');
                                        }
                                        gpui_cn::MenuEntry::Item(
                                            gpui_cn::MenuItem::new(format!("recent-{i}"), label).on_select(
                                                move |window, cx| {
                                                    let full = full.clone();
                                                    let _ = weak.update(cx, |this, cx| this.use_recent(full, window, cx));
                                                },
                                            ),
                                        )
                                    })
                                    .collect()
                            }),
                    )
            )
            // Their own line, side by side: fits a narrow sidebar.
            .child({
                let n = selection.count;
                let files = if n == 1 { "1 File".to_owned() } else { format!("{n} Files") };
                let label = match (self.amend, n) {
                    (true, 0) => "Amend".to_owned(),
                    (true, _) => format!("Amend with {files}"),
                    (false, _) => format!("Commit {files}"),
                };
                let nothing = busy || (n == 0 && !self.amend);
                let pushing = self.network.is_some();
                div()
                    .flex()
                    .gap_1()
                    .child(
                        Button::new("commit")
                            .primary()
                            .size(ButtonSize::Sm)
                            .flex_1()
                            .label(label)
                            .tooltip("Commit the checked files (⌘↩)")
                            .disabled(nothing)
                            .on_click(cx.listener(|this, _, window, cx| this.commit(false, window, cx))),
                    )
                    .child(
                        Button::new("commit-push")
                            .outline()
                            .size(ButtonSize::Sm)
                            .flex_1()
                            .label(if self.amend { "Amend and Push" } else { "Commit and Push" })
                            .tooltip(if self.amend {
                                "Amend, then push (⇧⌘↩). An amended commit that was already pushed needs a force push, which the app doesn't do."
                            } else {
                                "Commit the checked files, then push them (⇧⌘↩)"
                            })
                            .disabled(nothing || pushing)
                            .on_click(cx.listener(|this, _, window, cx| this.commit(true, window, cx))),
                    )
            })
            .into_any_element()
    }

    fn render_changes(&mut self, repo: &RepoState, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let Some(status) = repo.status.clone() else {
            return div()
                .p_2()
                .text_sm()
                .text_color(c.muted_foreground)
                .child(repo.error.clone().unwrap_or_else(|| "Reading status…".into()))
                .into_any_element();
        };
        let commit_box = self.render_commit_box(&status, cx);
        let mut list = div().flex().flex_col();
        for (section, label) in [
            (Section::Conflicts, "Conflicts"),
            (Section::Staged, "Staged"),
            (Section::Changes, "Changes"),
            (Section::Untracked, "Untracked"),
        ] {
            let entries: Vec<StatusEntry> = status.section(section).cloned().collect();
            if entries.is_empty() {
                continue;
            }
            // Inner repositories are not staged with the rest (Include…).
            let paths: Vec<String> = entries
                .iter()
                .filter(|e| !e.is_inner_repo())
                .map(|e| e.path.clone())
                .collect();
            let check_paths = paths.clone();
            let header_button = match section {
                Section::Staged => Some(
                    Button::new(SharedString::from(format!("all-{label}")))
                        .ghost()
                        .size(ButtonSize::Sm)
                        .label("Unstage All")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.unstage(paths.clone(), window, cx)
                        })),
                ),
                Section::Changes | Section::Untracked if !paths.is_empty() => Some(
                    Button::new(SharedString::from(format!("all-{label}")))
                        .ghost()
                        .size(ButtonSize::Sm)
                        .label("Stage All")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.stage(paths.clone(), window, cx)
                        })),
                ),
                Section::Changes | Section::Untracked | Section::Conflicts => None,
            };
            // Check or uncheck the whole section (not conflicts: those are
            // resolved first).
            let header_check = (section != Section::Conflicts && !check_paths.is_empty()).then(|| {
                let all = check_paths.iter().all(|p| self.is_checked(section, p));
                let weak = cx.entity().downgrade();
                let paths = check_paths.clone();
                Checkbox::new(SharedString::from(format!("check-all-{label}")))
                    .checked(all)
                    .tooltip(if all { "Leave all out of the commit" } else { "Include all in the commit" })
                    .on_click(move |on, _, cx| {
                        let _ = weak.update(cx, |this, cx| this.set_checked(section, &paths, *on, cx));
                    })
            });
            list = list.child(
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .px_2()
                    .pt_1()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .children(header_check)
                    .child(div().flex_1().child(format!("{label} ({})", entries.len())))
                    .children(header_button),
            );
            let total = entries.len();
            for (i, entry) in entries.into_iter().take(500).enumerate() {
                list = list.child(self.render_entry(section, i, entry, cx));
            }
            if total > 500 {
                list = list.child(
                    div()
                        .px_2()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(format!("and {} more", total - 500)),
                );
            }
        }
        if status.is_clean() {
            list = list.child(
                div()
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .text_sm()
                    .text_color(c.muted_foreground)
                    .child("No changes.")
                    .child(div().text_xs().child(format!(
                        "Git sees nothing new or modified in {}. New files appear here under Untracked; \
                         files that .gitignore ignores never do (see below).",
                        repo.root.display()
                    ))),
            );
        }
        let ignored = self.render_ignored(&repo.root, cx);
        let worktrees = (repo.worktrees.len() > 1).then(|| {
            div()
                .flex()
                .flex_col()
                .px_2()
                .pt_2()
                .text_xs()
                .text_color(c.muted_foreground)
                .child("Worktrees")
                .children(repo.worktrees.iter().map(|w| {
                    div().truncate().child(format!(
                        "{} — {}",
                        w.path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        w.branch.clone().unwrap_or_else(|| "(detached)".into())
                    ))
                }))
        });
        div()
            .flex()
            .flex_col()
            .child(commit_box)
            .child(list)
            .child(ignored)
            .children(worktrees)
            .into_any_element()
    }

    // -----------------------------------------------------------------
    // Repositories inside the repository, and none at all
    // -----------------------------------------------------------------

    /// An untracked folder with its own `.git`: git sees one entry, never
    /// its files, so it gets Include… and Open instead of Stage.
    fn render_inner_repo(&mut self, i: usize, entry: StatusEntry, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let rel = entry.path.trim_end_matches('/').to_owned();
        let abs = self.repo_root(cx).map(|r| r.join(&rel));
        let known = abs
            .as_ref()
            .is_some_and(|a| self.store.read(cx).repos.iter().any(|r| &r.root == a));
        let id = SharedString::from(format!("inner-{i}"));
        let (include, open) = (rel.clone(), abs.clone());
        div()
            .id(id.clone())
            .h(px(26.))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .hover(|el| el.bg(c.accent.opacity(0.5)))
            .child(div().w(px(14.)).text_xs().text_color(c.muted_foreground).child("R"))
            .child(div().text_sm().child(format!("{rel}/")))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child("separate git repository"),
            )
            .when(known, |el| {
                el.child(
                    Button::new(SharedString::from(format!("{id}-open")))
                        .ghost()
                        .size(ButtonSize::Sm)
                        .label("Open")
                        .tooltip("Show this repository's own changes")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            if let Some(path) = &open {
                                this.select_repo_at(path, window, cx);
                            }
                        })),
                )
            })
            .child(
                Button::new(SharedString::from(format!("{id}-include")))
                    .ghost()
                    .size(ButtonSize::Sm)
                    .label("Include…")
                    .tooltip("Make its files part of this repository")
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.ask_include_inner(include.clone(), window, cx)
                    })),
            )
            .into_any_element()
    }

    /// Switch the panel to the repository at `root`.
    fn select_repo_at(&mut self, root: &std::path::Path, window: &mut Window, cx: &mut Context<Self>) {
        let Some(i) = self.store.read(cx).repos.iter().position(|r| r.root == root) else {
            return;
        };
        self.repo = i;
        self.sync_draft(window, cx);
        self.view = View::Changes;
        self.load_view(window, cx);
        if self.show_ignored {
            self.ignored = None;
            self.load_ignored(cx);
        }
        cx.notify();
    }

    fn ask_include_inner(&mut self, rel: String, _: &mut Window, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root(cx) else {
            return;
        };
        let outer = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.display().to_string());
        let weak = cx.entity().downgrade();
        let prompt = Prompt::new(
            format!("Include {rel}/ in {outer}?"),
            format!(
                "{rel}/ is a separate git repository (it has its own .git folder), so {outer} can't see its \
                 files. Including it moves {rel}/.git to the Trash; its files then show here as new files to \
                 add and commit. Its own commits and branches go with that .git folder; you can put it back \
                 from the Trash."
            ),
        )
        .cancel()
        .button("Include Files", ButtonStyle::Primary, move |_, window, cx| {
            let (root, rel) = (root.clone(), rel.clone());
            let _ = weak.update(cx, |this, cx| this.include_inner(root, rel, window, cx));
            Ok(())
        });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn include_inner(&mut self, root: PathBuf, rel: String, window: &mut Window, cx: &mut Context<Self>) {
        let git = model(cx).read(cx).git();
        self.busy = Some(format!("Including {rel}/…"));
        cx.notify();
        let task = cx.background_spawn(async move {
            git.absorb_inner_repo(&root, &rel, |dot_git| {
                trash::delete(dot_git).map_err(|e| anyhow::anyhow!("Could not move {} to the Trash: {e}", dot_git.display()))
            })
        });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                if let Err(e) = result {
                    this.report(e, cx);
                }
                // The inner repository is gone from the list.
                this.store.update(cx, |s, cx| s.rediscover(cx));
                this.after_change(window, cx);
            });
        }));
    }

    /// No repository yet: `git init` in the project's main folder, and
    /// optionally a first commit of everything (not what .gitignore
    /// ignores, nor inner repositories). Local only: nothing goes to GitHub.
    fn initialize(&mut self, commit: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(folder) = self.store.read(cx).main_folder(cx) else {
            return;
        };
        let git = model(cx).read(cx).git();
        self.busy = Some("Initializing…".into());
        cx.notify();
        let task = cx.background_spawn(async move {
            git.init(&folder)?;
            if commit {
                git.initial_commit(&folder, "Initial commit").map_err(|e| {
                    anyhow::anyhow!(
                        "The repository was created, but the first commit failed: {e:#}\n\
                         (Is git's user.name and user.email set? Is the folder empty?)"
                    )
                })?;
            }
            Ok::<_, anyhow::Error>(())
        });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                if let Err(e) = result {
                    this.report(e, cx);
                }
                this.store.update(cx, |s, cx| s.rediscover(cx));
                this.after_change(window, cx);
            });
        }));
    }

    fn render_entry(
        &mut self,
        section: Section,
        i: usize,
        entry: StatusEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if entry.is_inner_repo() {
            return self.render_inner_repo(i, entry, cx);
        }
        let c = Colors::of(cx);
        let (dir, name) = match entry.path.rsplit_once('/') {
            Some((d, n)) => (d.to_owned(), n.to_owned()),
            None => (String::new(), entry.path.clone()),
        };
        let color = change_color(entry.change, &c);
        let id = SharedString::from(format!("e-{section:?}-{i}"));
        let check_id = id.clone();
        let open = entry.clone();
        let mut buttons = div().flex().gap_0p5();
        match section {
            Section::Staged => {
                let p = entry.path.clone();
                buttons = buttons.child(
                    Button::new(SharedString::from(format!("{id}-u")))
                        .ghost()
                        .size(ButtonSize::Sm)
                        .icon(Icon::from(gpui_kit::assets::IconName::Minus))
                        .tooltip("Unstage")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            // Not also the row's click (which opens the diff).
                            cx.stop_propagation();
                            this.unstage(vec![p.clone()], window, cx)
                        })),
                );
            }
            Section::Changes | Section::Untracked | Section::Conflicts => {
                let p = entry.path.clone();
                let d = entry.clone();
                if section != Section::Conflicts {
                    buttons = buttons.child(
                        Button::new(SharedString::from(format!("{id}-d")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .icon(Icon::from(gpui_kit::assets::IconName::Undo2))
                            .tooltip("Discard…")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                cx.stop_propagation();
                                this.discard(d.clone(), cx)
                            })),
                    );
                }
                buttons = buttons.child(
                    Button::new(SharedString::from(format!("{id}-s")))
                        .ghost()
                        .size(ButtonSize::Sm)
                        .icon(Icon::from(gpui_kit::assets::IconName::Plus))
                        .tooltip(if section == Section::Conflicts {
                            "Mark Resolved (stage)"
                        } else {
                            "Stage"
                        })
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.stage(vec![p.clone()], window, cx)
                        })),
                );
            }
        }
        div()
            .id(id)
            .h(px(26.))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .cursor_pointer()
            .hover(|el| el.bg(c.accent.opacity(0.5)))
            .when(section != Section::Conflicts, |el| {
                let checked = self.is_checked(section, &entry.path);
                let weak = cx.entity().downgrade();
                let path = vec![entry.path.clone()];
                el.child(
                    Checkbox::new(SharedString::from(format!("{check_id}-c")))
                        .checked(checked)
                        .tooltip(if checked { "In the next commit" } else { "Left out of the next commit" })
                        .on_click(move |on, _, cx| {
                            // Not also the row's click (which opens the diff).
                            cx.stop_propagation();
                            let _ = weak.update(cx, |this, cx| this.set_checked(section, &path, *on, cx));
                        }),
                )
            })
            .child(div().w(px(14.)).text_xs().text_color(color).child(entry.change.letter()))
            .child(div().text_sm().text_color(color).child(name))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(dir),
            )
            .child(buttons)
            .on_click(cx.listener(move |this, _, _, cx| this.open_diff(&open, cx)))
            .into_any_element()
    }

    fn render_branches(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let mut list = div().flex().flex_col().child(
            div().flex().px_2().py_1().child(
                Button::new("new-branch")
                    .outline()
                    .size(ButtonSize::Sm)
                    .label("New Branch…")
                    .on_click(cx.listener(|this, _, _, cx| this.new_branch(None, cx))),
            ),
        );
        let now = now_millis();
        for remote in [false, true] {
            let branches: Vec<Branch> = self
                .branches
                .iter()
                .filter(|b| b.remote == remote)
                .cloned()
                .collect();
            if branches.is_empty() {
                continue;
            }
            list = list.child(
                div()
                    .px_2()
                    .pt_1()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(if remote { "Remote" } else { "Local" }),
            );
            for (i, b) in branches.into_iter().enumerate() {
                let id = SharedString::from(format!("br-{remote}-{i}"));
                let checkout = b.clone();
                let from = b.clone();
                let rename = b.clone();
                let delete = b.clone();
                let mut actions = div().flex().gap_0p5().child(
                    Button::new(SharedString::from(format!("{id}-new")))
                        .ghost()
                        .size(ButtonSize::Sm)
                        .label("Branch from")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.new_branch(Some(from.name.clone()), cx)
                        })),
                );
                if !remote {
                    actions = actions
                        .child(
                            Button::new(SharedString::from(format!("{id}-ren")))
                                .ghost()
                                .size(ButtonSize::Sm)
                                .label("Rename")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.rename_branch(rename.clone(), cx)
                                })),
                        )
                        .when(!b.current, |el| {
                            el.child(
                                Button::new(SharedString::from(format!("{id}-del")))
                                    .ghost()
                                    .size(ButtonSize::Sm)
                                    .label("Delete")
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.delete_branch(delete.clone(), false, cx)
                                    })),
                            )
                        });
                }
                list = list.child(
                    div()
                        .id(id)
                        .px_2()
                        .py_0p5()
                        .flex()
                        .flex_col()
                        .cursor_pointer()
                        .hover(|el| el.bg(c.accent.opacity(0.5)))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1()
                                .text_sm()
                                .child(if b.current { "● " } else { "" })
                                .child(div().flex_1().min_w_0().truncate().child(b.name.clone()))
                                .child(actions),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .truncate()
                                .child(format!("{} · {}", relative(b.time * 1000, now), b.subject)),
                        )
                        .when(!b.current, |el| {
                            el.on_click(cx.listener(move |this, _, window, cx| {
                                this.checkout(checkout.clone(), window, cx)
                            }))
                        }),
                );
            }
        }
        list.into_any_element()
    }

    fn render_history(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let now = now_millis();
        let root = self.repo_root(cx);
        div()
            .flex()
            .flex_col()
            .children(self.commits.iter().enumerate().map(|(i, commit)| {
                let key = root.clone().map(|repo| DocKey::Commit {
                    repo,
                    sha: commit.sha.clone(),
                    title: commit.subject.clone(),
                });
                div()
                    .id(SharedString::from(format!("c-{i}")))
                    .px_2()
                    .py_0p5()
                    .cursor_pointer()
                    .hover(|el| el.bg(c.accent.opacity(0.5)))
                    .child(div().text_sm().truncate().child(commit.subject.clone()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child(format!(
                                "{} · {} · {}",
                                commit.short,
                                commit.author,
                                relative(commit.time * 1000, now)
                            )),
                    )
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if let Some(key) = key.clone() {
                            cx.emit(WorkspaceEvent::OpenDoc(key));
                        }
                    }))
            }))
            .into_any_element()
    }

    fn render_activity(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let now = now_millis();
        let entries: Vec<Activity> = self.store.read(cx).activity.iter().rev().cloned().collect();
        let mut list = div().flex().flex_col();
        if entries.is_empty() {
            return list
                .child(
                    div()
                        .px_2()
                        .py_1()
                        .text_sm()
                        .text_color(c.muted_foreground)
                        .child("Pushes, pulls and fetches made here appear here, with what they sent and received."),
                )
                .into_any_element();
        }
        for (i, e) in entries.into_iter().enumerate() {
            let arrow = match e.op {
                git_activity::Op::Push => "↑",
                git_activity::Op::Pull => "↓",
                git_activity::Op::Fetch => "⟳",
            };
            let repo_name = e
                .repo
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut card = div()
                .id(SharedString::from(format!("act-{i}")))
                .flex()
                .flex_col()
                .gap_0p5()
                .px_2()
                .py_1p5()
                .border_b_1()
                .border_color(c.border)
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_sm()
                        .child(div().w(px(14.)).child(arrow))
                        .child(e.op.label())
                        .child(
                            div()
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .truncate()
                                .child(match &e.branch {
                                    Some(b) => format!("{repo_name} · {b}"),
                                    None => repo_name.clone(),
                                }),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .text_xs()
                                .text_color(if e.ok { c.muted_foreground } else { c.destructive })
                                .child(if e.ok {
                                    relative(e.time, now)
                                } else {
                                    format!("failed · {}", relative(e.time, now))
                                }),
                        ),
                );
            // A pull that brought the branch to where the remote branch
            // moved shows once: "Pulled … from origin/x into x".
            let pulled_from = e.local.as_ref().and_then(|l| {
                e.received
                    .iter()
                    .find(|r| r.old == l.old && r.new == l.new)
                    .map(|r| r.name.clone())
            });
            let mut changes: Vec<(String, &RefChange)> = Vec::new();
            for r in &e.sent {
                changes.push((ref_change_line("Sent", r, None), r));
            }
            for r in &e.received {
                if pulled_from.as_deref() == Some(r.name.as_str()) {
                    continue;
                }
                changes.push((ref_change_line("Received", r, None), r));
            }
            if let Some(l) = &e.local {
                changes.push((ref_change_line("Updated", l, pulled_from.as_deref()), l));
            }
            if changes.is_empty() && e.ok {
                card = card.child(
                    div()
                        .pl(px(18.))
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(match e.op {
                            git_activity::Op::Push => "Nothing to send: the remote was up to date.",
                            _ => "Nothing new: already up to date.",
                        }),
                );
            }
            for (j, (line, change)) in changes.into_iter().enumerate() {
                card = card.child(self.render_ref_change(&e.repo, line, change, (i, j), cx));
            }
            let details = DocKey::ActivityDetails {
                id: e.id.clone(),
                title: format!(
                    "{} {}",
                    e.op.label(),
                    if e.ok { "details" } else { "failed" }
                ),
            };
            if !e.ok {
                // The failure in plain words; click for everything.
                let x = git_activity::explain(&e.message, e.op);
                let key = details.clone();
                card = card.child(
                    div()
                        .id(SharedString::from(format!("act-err-{i}")))
                        .ml(px(18.))
                        .mt_0p5()
                        .px_2()
                        .py_1()
                        .rounded_md()
                        .bg(c.destructive.opacity(0.08))
                        .cursor_pointer()
                        .hover(|el| el.bg(c.destructive.opacity(0.14)))
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .child(div().text_xs().text_color(c.destructive).child(x.title))
                        .children(x.hint.map(|h| div().text_xs().child(h)))
                        .child(
                            div()
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child("Details: git's full output…"),
                        )
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(WorkspaceEvent::OpenDoc(key.clone()))
                        })),
                );
            } else {
                let key = details.clone();
                card = card.child(
                    div()
                        .id(SharedString::from(format!("act-details-{i}")))
                        .pl(px(18.))
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .cursor_pointer()
                        .hover(|el| el.text_color(c.foreground))
                        .child("Details…")
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.emit(WorkspaceEvent::OpenDoc(key.clone()))
                        })),
                );
            }
            list = list.child(card);
        }
        list.into_any_element()
    }

    fn render_ref_change(
        &self,
        repo: &Path,
        line: String,
        r: &RefChange,
        ids: (usize, usize),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = Colors::of(cx);
        let n = r.commits.len();
        let short = |s: &Option<String>| s.as_deref().map(|s| s[..s.len().min(7)].to_owned());
        let range = match (short(&r.old), short(&r.new)) {
            (Some(a), Some(b)) => format!("{a}..{b}"),
            (None, Some(b)) => b,
            (Some(a), None) => a,
            (None, None) => String::new(),
        };
        let diff_key = match (&r.old, &r.new) {
            (Some(from), Some(to)) => Some(DocKey::RangeDiff {
                repo: repo.to_path_buf(),
                from: from.clone(),
                to: to.clone(),
                title: format!("{} {range}", r.name),
            }),
            _ => None,
        };
        let (i, j) = ids;
        let mut block = div()
            .flex()
            .flex_col()
            .pl(px(18.))
            .child(div().text_xs().child(line))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_xs()
                    .child(div().text_color(c.muted_foreground).child(range))
                    .child(div().flex_1())
                    .when_some(diff_key.clone(), |el, key| {
                        el.child(
                            Button::new(SharedString::from(format!("act-diff-{i}-{j}")))
                                .ghost()
                                .size(ButtonSize::Xs)
                                .label("Changes")
                                .tooltip("Before → after, side by side")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(WorkspaceEvent::OpenDoc(key.clone()))
                                })),
                        )
                    }),
            );
        for (k, commit) in r.commits.iter().take(5).enumerate() {
            let key = DocKey::Commit {
                repo: repo.to_path_buf(),
                sha: commit.sha.clone(),
                title: commit.summary.clone(),
            };
            block = block.child(
                div()
                    .id(SharedString::from(format!("act-c-{i}-{j}-{k}")))
                    .flex()
                    .gap_1()
                    .text_xs()
                    .cursor_pointer()
                    .rounded_sm()
                    .hover(|el| el.bg(c.accent.opacity(0.5)))
                    .child(
                        div()
                            .text_color(c.muted_foreground)
                            .child(commit.sha[..commit.sha.len().min(7)].to_owned()),
                    )
                    .child(div().truncate().child(commit.summary.clone()))
                    .on_click(cx.listener(move |_, _, _, cx| {
                        cx.emit(WorkspaceEvent::OpenDoc(key.clone()))
                    })),
            );
        }
        if n > 5 || r.more_commits > 0 {
            block = block.child(
                div()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(format!("and {} more", if r.more_commits > 0 { "many".to_owned() } else { (n - 5).to_string() })),
            );
        }
        if !r.files.is_empty() {
            let shown: Vec<String> = r
                .files
                .iter()
                .take(6)
                .map(|f| format!("{} {}", f.status, f.path))
                .collect();
            let more = r.files.len().saturating_sub(6) + r.more_files;
            let mut files = div().flex().flex_col().pt_0p5();
            for (k, text) in shown.into_iter().enumerate() {
                let key = diff_key.clone();
                files = files.child(
                    div()
                        .id(SharedString::from(format!("act-f-{i}-{j}-{k}")))
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .truncate()
                        .cursor_pointer()
                        .hover(|el| el.text_color(c.foreground))
                        .child(text)
                        .on_click(cx.listener(move |_, _, _, cx| {
                            if let Some(key) = key.clone() {
                                cx.emit(WorkspaceEvent::OpenDoc(key));
                            }
                        })),
                );
            }
            if more > 0 {
                files = files.child(
                    div()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(format!("and {more} more files")),
                );
            }
            block = block.child(files);
        }
        block.into_any_element()
    }

    fn render_stash(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let now = now_millis();
        let mut list = div().flex().flex_col().child(
            div().px_2().py_1().child(
                Button::new("stash-push")
                    .outline()
                    .size(ButtonSize::Sm)
                    .label("Stash Changes…")
                    .on_click(cx.listener(|this, _, _, cx| this.stash_push(cx))),
            ),
        );
        if self.stashes.is_empty() {
            list = list.child(
                div()
                    .px_2()
                    .text_sm()
                    .text_color(c.muted_foreground)
                    .child("No stashes."),
            );
        }
        for (i, s) in self.stashes.clone().into_iter().enumerate() {
            let (a, p, d) = (s.clone(), s.clone(), s.clone());
            list = list.child(
                div()
                    .px_2()
                    .py_0p5()
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_sm().truncate().child(s.message.clone()))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(c.muted_foreground)
                                    .child(format!("{} · {}", s.name, relative(s.time * 1000, now))),
                            ),
                    )
                    .child(
                        Button::new(SharedString::from(format!("st-a-{i}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .label("Apply")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.stash_op(a.clone(), StashOp::Apply, cx, window)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("st-p-{i}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .label("Pop")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.stash_op(p.clone(), StashOp::Pop, cx, window)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("st-d-{i}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .label("Drop")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.stash_op(d.clone(), StashOp::Drop, cx, window)
                            })),
                    ),
            );
        }
        list.into_any_element()
    }
}

/// A row of the Changes view, for its checkbox.
fn row_key(section: Section, path: &str) -> String {
    format!("{section:?}\u{0}{path}")
}

/// What Commit does with the checked rows (`GitPanel::selection`).
#[derive(Default)]
struct Selection {
    /// Stage the checked new and changed files first.
    stage: bool,
    /// Unchecked new and changed files: not staged.
    skip: Vec<String>,
    /// Unchecked staged files: left out of this commit, still staged.
    leave_out: Vec<String>,
    /// Checked files.
    count: usize,
}

/// File colours, here and in the Files tab: not in git red, added green,
/// edited blue, renamed teal, deleted grey, conflicted purple; ignored
/// files are orange ([`IGNORED_COLOR`]).
pub fn change_color(change: Change, c: &Colors) -> gpui_kit::Hsla {
    match change {
        Change::Untracked => gpui_kit::hsla(0.0, 0.75, 0.6, 1.0),
        Change::Added => gpui_kit::hsla(0.33, 0.55, 0.45, 1.0),
        Change::Deleted => c.muted_foreground,
        Change::Conflicted => gpui_kit::hsla(0.8, 0.6, 0.62, 1.0),
        Change::Modified | Change::TypeChanged => gpui_kit::hsla(0.59, 0.75, 0.6, 1.0),
        Change::Renamed | Change::Copied => gpui_kit::hsla(0.5, 0.55, 0.48, 1.0),
    }
}

/// Files and folders git ignores.
pub const IGNORED_COLOR: gpui_kit::Hsla = gpui_kit::Hsla {
    h: 0.075,
    s: 0.85,
    l: 0.55,
    a: 1.0,
};

impl Render for GitPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let store = self.store.read(cx);
        if !store.discovered {
            return div()
                .p_2()
                .text_sm()
                .text_color(c.muted_foreground)
                .child("Looking for git repositories…")
                .into_any_element();
        }
        if store.repos.is_empty() {
            let folder = store
                .main_folder(cx)
                .map(|f| f.display().to_string())
                .unwrap_or_default();
            let busy = self.busy.is_some();
            return div()
                .id("git-panel")
                .track_focus(&self.focus)
                .p_2()
                .flex()
                .flex_col()
                .gap_2()
                .text_sm()
                .text_color(c.muted_foreground)
                .child("This project isn't a git repository yet.")
                .child(div().text_xs().child(format!(
                    "Initialize creates one in {folder} (a .git folder), on this computer only: nothing \
                     goes to GitHub. Use File ▸ Publish to GitHub for that."
                )))
                .child(
                    Button::new("git-init-commit")
                        .primary()
                        .label("Initialize and Commit All Files")
                        .disabled(busy)
                        .tooltip("git init, then a first commit of every file (not what .gitignore ignores)")
                        .on_click(cx.listener(|this, _, window, cx| this.initialize(true, window, cx))),
                )
                .child(
                    Button::new("git-init")
                        .outline()
                        .label("Initialize Only")
                        .disabled(busy)
                        .tooltip("git init; choose what to commit afterwards")
                        .on_click(cx.listener(|this, _, window, cx| this.initialize(false, window, cx))),
                )
                .children(self.busy.clone().map(|b| div().text_xs().child(b)))
                .into_any_element();
        }
        self.repo = self.repo.min(store.repos.len() - 1);
        let Some(repo) = self.repo_state(cx) else {
            return div().into_any_element();
        };
        let header = self.render_header(&repo, cx);
        let views = self.render_views(cx);
        let body = match self.view {
            View::Changes => self.render_changes(&repo, cx),
            View::Branches => self.render_branches(cx),
            View::History => self.render_history(cx),
            View::Stash => self.render_stash(cx),
            View::Activity => self.render_activity(cx),
        };
        div()
            .id("git-panel")
            .key_context(GIT_PANEL_CONTEXT)
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &CommitAction, window, cx| this.commit(false, window, cx)))
            .on_action(cx.listener(|this, _: &CommitAndPush, window, cx| this.commit(true, window, cx)))
            .child(header)
            .child(views)
            .child(
                div()
                    .id("git-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
            .into_any_element()
    }
}

/// "Sent 2 commits to origin/main" and the like.
fn ref_change_line(verb: &str, r: &RefChange, pulled_from: Option<&str>) -> String {
    let n = r.commits.len();
    let commits = if r.more_commits > 0 {
        format!("{n}+ commits")
    } else if n == 1 {
        "1 commit".to_owned()
    } else {
        format!("{n} commits")
    };
    match (&r.old, &r.new, verb) {
        (_, None, _) => format!("Deleted {}", r.name),
        (None, Some(_), "Sent") => format!("Sent new branch {} ({commits})", r.name),
        (None, Some(_), _) => format!("New branch {} ({commits})", r.name),
        (Some(_), Some(_), "Sent") => format!("Sent {commits} to {}", r.name),
        (Some(_), Some(_), "Updated") => match pulled_from {
            Some(from) => format!("Pulled {commits} from {from} into {}", r.name),
            None => format!("Updated {} with {commits}", r.name),
        },
        (Some(_), Some(_), _) => format!("Received {commits} on {}", r.name),
    }
}

/// "sent 2 commits to origin/main", for the toast after a push or pull.
fn activity_summary(e: &Activity) -> String {
    let count = |r: &RefChange| {
        let n = r.commits.len();
        if r.more_commits > 0 {
            format!("{n}+ commits")
        } else if n == 1 {
            "1 commit".to_owned()
        } else {
            format!("{n} commits")
        }
    };
    if let Some(r) = e.sent.first() {
        return format!("sent {} to {}", count(r), r.name);
    }
    if let Some(r) = &e.local {
        return format!("{} updated with {}", r.name, count(r));
    }
    if let Some(r) = e.received.first() {
        let more = e.received.len() - 1;
        return if more > 0 {
            format!("received {} on {} (and {more} more branches)", count(r), r.name)
        } else {
            format!("received {} on {}", count(r), r.name)
        };
    }
    "nothing new".to_owned()
}
