//! GitHub through `gh` (next phases, Phase 6): the GitHub tab, pull request
//! and CI run tabs, failed logs, and the new pull request form.
//!
//! Refresh rules (6.8): when a GitHub view is shown, when the window gains
//! focus, on ⟳, and every 60 s only while the GitHub tab is visible and its
//! window has focus. Rate limits back off. Nothing runs while hidden.

use std::path::PathBuf;
use std::time::Duration;

use anveg_core::config::DocKey;
use anveg_core::gh::{
    self, Check, ChecksSummary, GhState, MergeMethod, NewPr, PrAction, PrDetail, PrFilter,
    PrSummary, RateLimited, RepoInfo, Run, RunDetail, RunState,
};
use anveg_core::git::NetworkOp;
use anveg_core::process::CancelToken;
use gpui_cn::prelude::*;
use gpui_cn::{Input, InputState, Switch, Textarea, TextareaState};
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::component::text::TextView;
use gpui_kit::{
    AnyElement, App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    SharedString, StatefulInteractiveElement as _, Styled as _, Task, Window, div, hsla, px,
};

use crate::diff_view::{DiffSource, DiffView};
use crate::git_panel::GitStore;
use crate::model::{ModelEvent, model};
use crate::ui::Colors;
use crate::workspace::{ButtonStyle, Prompt, WorkspaceEvent};

/// Auto-refresh period while visible and focused.
pub const REFRESH: Duration = Duration::from_secs(60);

fn green() -> Hsla {
    hsla(0.33, 0.6, 0.42, 1.0)
}

fn amber() -> Hsla {
    hsla(0.11, 0.85, 0.5, 1.0)
}

fn run_badge(state: RunState, c: &Colors) -> (&'static str, Hsla) {
    match state {
        RunState::Success => ("✓", green()),
        RunState::Failure => ("✗", c.destructive),
        RunState::Pending => ("●", amber()),
        RunState::Cancelled => ("⊘", c.muted_foreground),
        RunState::Skipped => ("–", c.muted_foreground),
    }
}

fn checks_badge(s: ChecksSummary, c: &Colors) -> Option<(&'static str, Hsla)> {
    match s {
        ChecksSummary::None => None,
        ChecksSummary::Passing => Some(("✓", green())),
        ChecksSummary::Failing => Some(("✗", c.destructive)),
        ChecksSummary::Pending => Some(("●", amber())),
    }
}

/// `gh` once detection has finished (after launch it takes a moment).
fn gh_ready(cx: &App) -> Option<gh::Gh> {
    let m = model(cx).read(cx);
    matches!(m.gh_state, Some(GhState::Ready { .. }))
        .then(|| m.gh())
        .flatten()
}

/// Call `f` when `gh` becomes ready (for tabs restored at launch).
fn on_gh_ready<T: 'static>(
    window: &mut Window,
    cx: &mut Context<T>,
    f: impl Fn(&mut T, &mut Window, &mut Context<T>) + 'static,
) -> gpui_kit::Subscription {
    let m = model(cx);
    cx.subscribe_in(&m, window, move |this, _, event, window, cx| {
        if matches!(event, ModelEvent::GhChanged) && gh_ready(cx).is_some() {
            f(this, window, cx);
        }
    })
}

fn gh_error(e: &anyhow::Error) -> String {
    if e.downcast_ref::<RateLimited>().is_some() {
        format!("GitHub is rate-limiting requests; trying again later. ({e})")
    } else {
        format!("{e:#}")
    }
}

/// The setup steps shown when `gh` is missing or signed out.
fn setup(state: Option<&GhState>, cx: &mut Context<impl EventEmitter<WorkspaceEvent>>) -> AnyElement {
    let c = Colors::of(cx);
    let (title, steps): (&str, Vec<&str>) = match state {
        None => ("Looking for the GitHub CLI…", vec![]),
        Some(GhState::Missing) => (
            "The GitHub tab uses the GitHub CLI (gh).",
            vec!["Install it:  brew install gh", "Sign in:  gh auth login"],
        ),
        Some(GhState::SignedOut { .. }) => (
            "The GitHub CLI isn't signed in.",
            vec!["Sign in:  gh auth login"],
        ),
        Some(GhState::Ready { .. }) => ("", vec![]),
    };
    div()
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .text_sm()
        .child(title)
        .children(steps.into_iter().map(|s| {
            div()
                .font_family("Menlo")
                .text_xs()
                .text_color(c.muted_foreground)
                .child(s)
        }))
        .when(state.is_some(), |el| {
            el.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        Button::new("gh-login")
                            .outline()
                            .size(ButtonSize::Sm)
                            .label("Run gh auth login in a Terminal")
                            .disabled(matches!(state, Some(GhState::Missing)))
                            .on_click(cx.listener(|_, _, _, cx| {
                                cx.emit(WorkspaceEvent::RunInTerminal(vec![
                                    "gh".into(),
                                    "auth".into(),
                                    "login".into(),
                                ]))
                            })),
                    )
                    .child(
                        Button::new("gh-recheck")
                            .primary()
                            .size(ButtonSize::Sm)
                            .label("Check Again")
                            .on_click(|_, _, cx| {
                                let m = model(cx);
                                m.update(cx, |m, cx| m.check_gh(true, cx));
                            }),
                    ),
            )
        })
        .into_any_element()
}

// ---------------------------------------------------------------------
// The GitHub tab
// ---------------------------------------------------------------------

pub struct GitHubPanel {
    store: Entity<GitStore>,
    repo: usize,
    filter: PrFilter,
    info: Option<RepoInfo>,
    prs: Vec<PrSummary>,
    runs: Vec<Run>,
    branch: Option<String>,
    loading: bool,
    again: bool,
    error: Option<String>,
    visible: bool,
    focused: bool,
    backoff: u32,
    timer: Option<Task<()>>,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
    _subs: Vec<gpui_kit::Subscription>,
}

impl EventEmitter<WorkspaceEvent> for GitHubPanel {}

impl Focusable for GitHubPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl GitHubPanel {
    pub fn new(store: Entity<GitStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let m = model(cx);
        let subs = vec![
            cx.observe(&store, |this, store, cx| {
                // The branch changed: runs are per branch.
                let branch = this.current_branch(store.read(cx));
                if branch != this.branch && this.visible {
                    this.refresh(cx);
                }
                cx.notify();
            }),
            cx.subscribe_in(&m, window, |this, _, event, _, cx| {
                if matches!(event, ModelEvent::GhChanged) {
                    if this.visible {
                        this.refresh(cx);
                    }
                    cx.notify();
                }
            }),
        ];
        Self {
            store,
            repo: 0,
            filter: PrFilter::default(),
            info: None,
            prs: Vec::new(),
            runs: Vec::new(),
            branch: None,
            loading: false,
            again: false,
            error: None,
            visible: false,
            focused: false,
            backoff: 0,
            timer: None,
            focus: cx.focus_handle(),
            _tasks: Vec::new(),
            _subs: subs,
        }
    }

    fn current_branch(&self, store: &GitStore) -> Option<String> {
        store
            .repos
            .get(self.repo)?
            .status
            .as_ref()?
            .branch
            .head
            .clone()
    }

    fn repo_root(&self, cx: &App) -> Option<PathBuf> {
        self.store.read(cx).repos.get(self.repo).map(|r| r.root.clone())
    }

    /// Shown or hidden, and whether the window has focus.
    pub fn set_visible(&mut self, visible: bool, focused: bool, cx: &mut Context<Self>) {
        let was = self.visible && self.focused;
        self.visible = visible;
        self.focused = focused;
        if visible && (!was || self.prs.is_empty()) {
            self.refresh(cx);
        }
        if !(visible && focused) {
            self.timer = None;
        } else if self.timer.is_none() {
            self.schedule(cx);
        }
    }

    fn schedule(&mut self, cx: &mut Context<Self>) {
        let delay = REFRESH * 2u32.pow(self.backoff.min(4));
        self.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                this.timer = None;
                if this.visible && this.focused {
                    this.refresh(cx);
                    this.schedule(cx);
                }
            });
        }));
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let ready = matches!(model(cx).read(cx).gh_state, Some(GhState::Ready { .. }));
        if !ready {
            return;
        }
        if self.loading {
            self.again = true;
            return;
        }
        let Some(root) = self.repo_root(cx) else {
            return;
        };
        let Some(gh) = model(cx).read(cx).gh() else {
            return;
        };
        let branch = self.current_branch(self.store.read(cx));
        self.branch = branch.clone();
        let filter = self.filter;
        let need_info = self.info.is_none();
        self.loading = true;
        cx.notify();
        let task = cx.background_spawn(async move {
            let info = if need_info {
                Some(gh.repo(&root))
            } else {
                None
            };
            let prs = gh.pr_list(&root, filter);
            let runs = match &branch {
                Some(b) => gh.run_list(&root, b),
                None => Ok(Vec::new()),
            };
            (info, prs, runs)
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let (info, prs, runs) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                let mut error = None;
                let mut limited = false;
                if let Some(info) = info {
                    match info {
                        Ok(i) => this.info = Some(i),
                        Err(e) => error = Some(gh_error(&e)),
                    }
                }
                match prs {
                    Ok(p) => this.prs = p,
                    Err(e) => {
                        limited |= e.downcast_ref::<RateLimited>().is_some();
                        error.get_or_insert(gh_error(&e));
                    }
                }
                match runs {
                    Ok(r) => {
                        let latest = r.first().cloned();
                        let repo = this.repo;
                        this.store.update(cx, |s, _| {
                            if let Some(rs) = s.repos.get_mut(repo) {
                                rs.latest_run = latest;
                            }
                        });
                        this.runs = r;
                    }
                    Err(e) => {
                        limited |= e.downcast_ref::<RateLimited>().is_some();
                        error.get_or_insert(gh_error(&e));
                    }
                }
                this.backoff = if limited { this.backoff + 1 } else { 0 };
                this.error = error;
                if std::mem::take(&mut this.again) {
                    this.refresh(cx);
                }
                cx.notify();
            });
        }));
    }

    fn render_lists(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let root = self.repo_root(cx);
        let mut filters = div().flex().gap_0p5().px_1();
        for f in [PrFilter::Mine, PrFilter::ReviewRequested, PrFilter::AllOpen] {
            filters = filters.child(
                Button::new(SharedString::from(format!("prf-{}", f.label())))
                    .ghost()
                    .size(ButtonSize::Sm)
                    .label(f.label())
                    .toggled(self.filter == f)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.filter = f;
                        this.prs.clear();
                        this.refresh(cx);
                    })),
            );
        }
        let prs = self.prs.iter().enumerate().map(|(i, pr)| {
            let key = root.clone().map(|repo| DocKey::PullRequest {
                repo,
                number: pr.number,
                title: pr.title.clone(),
            });
            let badge = checks_badge(pr.checks(), &c);
            let review = gh::review_label(pr.review_decision.as_deref());
            div()
                .id(SharedString::from(format!("pr-{i}")))
                .px_2()
                .py_1()
                .cursor_pointer()
                .hover(|el| el.bg(c.accent.opacity(0.5)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .text_sm()
                        .children(badge.map(|(t, col)| div().text_color(col).child(t)))
                        .child(div().flex_1().min_w_0().truncate().child(format!("#{} {}", pr.number, pr.title))),
                )
                .child(
                    div()
                        .flex()
                        .gap_2()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(pr.author.as_ref().map(|a| a.login.clone()).unwrap_or_default())
                        .child(div().truncate().child(pr.head_ref_name.clone()))
                        .when(pr.is_draft, |el| el.child("Draft"))
                        .children(review),
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    if let Some(key) = key.clone() {
                        cx.emit(WorkspaceEvent::OpenDoc(key));
                    }
                }))
        });
        let runs = self.runs.iter().enumerate().map(|(i, run)| {
            let (badge, col) = run_badge(run.state(), &c);
            let key = root.clone().map(|repo| DocKey::Run {
                repo,
                id: run.database_id,
                title: format!("{} · {}", run.workflow_name, run.display_title),
            });
            div()
                .id(SharedString::from(format!("run-{i}")))
                .px_2()
                .py_0p5()
                .flex()
                .gap_1()
                .items_center()
                .cursor_pointer()
                .hover(|el| el.bg(c.accent.opacity(0.5)))
                .child(div().text_color(col).child(badge))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(format!("{} · {}", run.workflow_name, run.display_title)),
                )
                .on_click(cx.listener(move |_, _, _, cx| {
                    if let Some(key) = key.clone() {
                        cx.emit(WorkspaceEvent::OpenDoc(key));
                    }
                }))
        });
        let name = self
            .info
            .as_ref()
            .map(|i| i.name_with_owner.clone())
            .unwrap_or_default();
        let branch = self.branch.clone().unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .py_1()
                    .border_b_1()
                    .border_color(c.border)
                    .child(div().flex_1().min_w_0().truncate().text_sm().child(name))
                    .child(
                        Button::new("new-pr")
                            .outline()
                            .size(ButtonSize::Sm)
                            .label("New Pull Request")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(repo) = this.repo_root(cx) {
                                    cx.emit(WorkspaceEvent::OpenDoc(DocKey::CreatePullRequest { repo }));
                                }
                            })),
                    )
                    .child(
                        Button::new("gh-refresh")
                            .ghost()
                            .size(ButtonSize::Sm)
                            .icon(Icon::from(gpui_kit::assets::IconName::RefreshCw))
                            .tooltip("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
            .children(self.error.clone().map(|e| {
                div().px_2().py_1().text_xs().text_color(c.destructive).child(e)
            }))
            .child(div().px_2().pt_1().text_xs().text_color(c.muted_foreground).child("Pull requests"))
            .child(filters)
            .children(prs)
            .when(self.prs.is_empty() && !self.loading, |el| {
                el.child(div().px_2().text_sm().text_color(c.muted_foreground).child("None."))
            })
            .child(
                div()
                    .px_2()
                    .pt_2()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child(format!("CI runs on {branch}")),
            )
            .children(runs)
            .when(self.runs.is_empty() && !self.loading, |el| {
                el.child(div().px_2().text_sm().text_color(c.muted_foreground).child("None."))
            })
            .when(self.loading, |el| {
                el.child(div().px_2().py_1().text_xs().text_color(c.muted_foreground).child("Loading…"))
            })
            .into_any_element()
    }
}

impl Render for GitHubPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let state = model(cx).read(cx).gh_state.clone();
        let body = match &state {
            Some(GhState::Ready { .. }) => {
                if self.store.read(cx).repos.is_empty() {
                    div()
                        .p_2()
                        .text_sm()
                        .text_color(c.muted_foreground)
                        .child(if self.store.read(cx).discovered {
                            "This project isn't in a git repository."
                        } else {
                            "Looking for git repositories…"
                        })
                        .into_any_element()
                } else {
                    self.render_lists(cx)
                }
            }
            other => setup(other.as_ref(), cx),
        };
        div()
            .id("github-panel")
            .track_focus(&self.focus)
            .size_full()
            .overflow_y_scroll()
            .child(body)
    }
}

// ---------------------------------------------------------------------
// Pull request tab
// ---------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum PrTab {
    Overview,
    Files,
}

pub struct PrView {
    repo: PathBuf,
    number: u64,
    detail: Option<PrDetail>,
    checks: Vec<Check>,
    diff: Entity<DiffView>,
    tab: PrTab,
    loading: bool,
    busy: Option<String>,
    error: Option<String>,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
    _gh: gpui_kit::Subscription,
}

impl EventEmitter<WorkspaceEvent> for PrView {}

impl Focusable for PrView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl PrView {
    pub fn new(repo: PathBuf, number: u64, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let diff = cx.new(|cx| DiffView::new(DiffSource::Text(String::new()), window, cx));
        let mut this = Self {
            repo,
            number,
            detail: None,
            checks: Vec::new(),
            diff,
            tab: PrTab::Overview,
            loading: false,
            busy: None,
            error: None,
            focus: cx.focus_handle(),
            _tasks: Vec::new(),
            _gh: on_gh_ready(window, cx, |this: &mut Self, _, cx| {
                if this.detail.is_none() {
                    this.refresh(cx)
                }
            }),
        };
        this.refresh(cx);
        this
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(gh) = gh_ready(cx) else {
            if matches!(model(cx).read(cx).gh_state, Some(GhState::Missing | GhState::SignedOut { .. })) {
                self.error = Some("The GitHub CLI (gh) isn't available or isn't signed in.".into());
            }
            return;
        };
        if self.loading {
            return;
        }
        self.loading = true;
        cx.notify();
        let (repo, n) = (self.repo.clone(), self.number);
        let task = cx.background_spawn(async move {
            (gh.pr_view(&repo, n), gh.pr_checks(&repo, n), gh.pr_diff(&repo, n))
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let (detail, checks, diff) = task.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                this.error = None;
                match detail {
                    Ok(d) => this.detail = Some(d),
                    Err(e) => this.error = Some(gh_error(&e)),
                }
                this.checks = checks.unwrap_or_default();
                if let Ok(text) = diff {
                    this.diff.update(cx, |d, cx| d.set_text(&text, cx));
                }
                cx.emit(WorkspaceEvent::TitleChanged);
                cx.notify();
            });
        }));
    }

    fn action(&mut self, action: PrAction, cx: &mut Context<Self>) {
        let Some(gh) = model(cx).read(cx).gh() else {
            return;
        };
        let label = match &action {
            PrAction::Checkout => "Checking out…",
            PrAction::Ready => "Marking ready…",
            PrAction::Merge(_) => "Merging…",
            PrAction::Close => "Closing…",
        };
        self.busy = Some(label.into());
        cx.notify();
        let (repo, n) = (self.repo.clone(), self.number);
        let task = cx.background_spawn(async move { gh.pr_action(&repo, n, &action) });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(msg) => {
                        if !msg.is_empty() {
                            cx.emit(WorkspaceEvent::Toast(msg.lines().last().unwrap_or("").to_owned().into()));
                        }
                    }
                    // Branch protection and similar refusals: GitHub's words.
                    Err(e) => cx.emit(WorkspaceEvent::Prompt(Prompt::message(
                        "GitHub refused",
                        format!("{e:#}"),
                    ))),
                }
                cx.emit(WorkspaceEvent::GitChanged);
                this.refresh(cx);
            });
        }));
    }

    fn checkout(&mut self, cx: &mut Context<Self>) {
        let git = model(cx).read(cx).git();
        let repo = self.repo.clone();
        let task = cx.background_spawn(async move { git.status(&repo) });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let dirty = task.await.map(|s| !s.is_clean()).unwrap_or(false);
            let _ = this.update(cx, |this, cx| {
                if !dirty {
                    this.action(PrAction::Checkout, cx);
                    return;
                }
                let weak = cx.entity().downgrade();
                let prompt = Prompt::new(
                    "You have uncommitted changes",
                    "Checking out the pull request's branch may fail or carry your changes along. Check out anyway?",
                )
                .cancel()
                .button("Check Out", ButtonStyle::Primary, move |_, _, cx| {
                    let _ = weak.update(cx, |this, cx| this.action(PrAction::Checkout, cx));
                    Ok(())
                });
                cx.emit(WorkspaceEvent::Prompt(prompt));
            });
        }));
    }

    fn merge(&mut self, cx: &mut Context<Self>) {
        let mut prompt = Prompt::new(
            format!("Merge pull request #{}?", self.number),
            "Choose how to merge. GitHub's branch protection rules still apply.",
        )
        .cancel();
        for method in [MergeMethod::Squash, MergeMethod::Merge, MergeMethod::Rebase] {
            let weak = cx.entity().downgrade();
            prompt = prompt.button(method.label(), ButtonStyle::Primary, move |_, _, cx| {
                let _ = weak.update(cx, |this, cx| this.action(PrAction::Merge(method), cx));
                Ok(())
            });
        }
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let prompt = Prompt::new(
            format!("Close pull request #{}?", self.number),
            "It is closed without merging. You can reopen it on GitHub.",
        )
        .cancel()
        .button("Close Pull Request", ButtonStyle::Destructive, move |_, _, cx| {
            let _ = weak.update(cx, |this, cx| this.action(PrAction::Close, cx));
            Ok(())
        });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn render_overview(&mut self, detail: &PrDetail, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let repo = self.repo.clone();
        let number = self.number;
        let title = detail.title.clone();
        let checks = self.checks.iter().enumerate().map(|(i, check)| {
            let (badge, col) = match check.bucket.as_str() {
                "pass" => ("✓", green()),
                "fail" => ("✗", c.destructive),
                "pending" => ("●", amber()),
                _ => ("–", c.muted_foreground),
            };
            let run_id = check.run_id();
            let failed = check.failed();
            let name = check.name.clone();
            let (repo1, repo2) = (repo.clone(), repo.clone());
            let title = title.clone();
            div()
                .px_1()
                .flex()
                .items_center()
                .gap_1()
                .child(div().text_color(col).child(badge))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .child(format!("{} {}", check.workflow, check.name).trim().to_owned()),
                )
                .when(failed && run_id.is_some(), |el| {
                    let id = run_id.unwrap_or_default();
                    let n2 = name.clone();
                    el.child(
                        Button::new(SharedString::from(format!("chk-log-{i}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .label("Show Failed Logs")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(WorkspaceEvent::OpenDoc(DocKey::RunLog {
                                    repo: repo1.clone(),
                                    id,
                                    title: n2.clone(),
                                }))
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("chk-fix-{i}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .label("Ask Claude to Fix")
                            .on_click(cx.listener(move |_, _, _, cx| {
                                ask_claude_to_fix(
                                    repo2.clone(),
                                    id,
                                    format!("check \"{name}\" on pull request #{number} ({title})"),
                                    cx,
                                )
                            })),
                    )
                })
        });
        let reviews = detail.reviews.iter().filter(|r| r.state != "COMMENTED" || !r.body.is_empty()).map(|r| {
            div()
                .text_sm()
                .child(format!(
                    "{} — {}",
                    r.author.as_ref().map(|a| a.login.clone()).unwrap_or_default(),
                    r.state.to_lowercase().replace('_', " ")
                ))
                .when(!r.body.is_empty(), |el| {
                    el.child(div().text_xs().text_color(c.muted_foreground).child(r.body.clone()))
                })
        });
        let comments = detail.comments.iter().map(|cm| {
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .py_1()
                .border_t_1()
                .border_color(c.border)
                .child(
                    div()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(cm.author.as_ref().map(|a| a.login.clone()).unwrap_or_default()),
                )
                .child(div().text_sm().child(cm.body.clone()))
        });
        let body = if detail.body.trim().is_empty() {
            div()
                .text_sm()
                .text_color(c.muted_foreground)
                .child("No description.")
                .into_any_element()
        } else {
            TextView::markdown(
                SharedString::from(format!("pr-body-{}", self.number)),
                detail.body.clone(),
            )
            .into_any_element()
        };
        div()
            .id("pr-overview")
            .size_full()
            .overflow_y_scroll()
            .p_3()
            .flex()
            .flex_col()
            .gap_3()
            .child(body)
            .child(div().text_xs().text_color(c.muted_foreground).child(format!(
                "Checks{}",
                if self.checks.is_empty() { ": none reported" } else { "" }
            )))
            .children(checks)
            .child(div().text_xs().text_color(c.muted_foreground).child(format!(
                "Reviews — {}",
                gh::review_label(detail.review_decision.as_deref()).unwrap_or("no decision")
            )))
            .children(reviews)
            .when(!detail.comments.is_empty(), |el| {
                el.child(div().text_xs().text_color(c.muted_foreground).child("Comments"))
            })
            .children(comments)
            .into_any_element()
    }
}

/// Save the end of a failed log to a temporary file and type a prompt that
/// mentions it into the active Claude session (never presses Enter).
fn ask_claude_to_fix<T: EventEmitter<WorkspaceEvent>>(
    repo: PathBuf,
    run_id: u64,
    what: String,
    cx: &mut Context<T>,
) {
    let Some(gh) = model(cx).read(cx).gh() else {
        return;
    };
    let task = cx.background_spawn(async move {
        let log = gh.run_failed_log(&repo, run_id)?;
        let dir = std::env::temp_dir().join("anveg-ci-logs");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(format!("run-{run_id}-failed.log"));
        std::fs::write(&path, gh::log_tail(&log, 300))?;
        anyhow::Ok(path)
    });
    cx.spawn(async move |this, cx| {
        let result = task.await;
        let _ = this.update(cx, |_, cx| match result {
            Ok(path) => cx.emit(WorkspaceEvent::TypeIntoClaude(format!(
                "The CI {what} failed. The end of the failed log is in @{} — please find the cause and fix it. ",
                path.display()
            ))),
            Err(e) => cx.emit(WorkspaceEvent::Prompt(Prompt::message(
                "Could not get the log",
                format!("{e:#}"),
            ))),
        });
    })
    .detach();
}

impl Render for PrView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let Some(detail) = self.detail.clone() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(c.muted_foreground)
                .child(self.error.clone().unwrap_or_else(|| "Loading pull request…".into()))
                .into_any_element();
        };
        let url = detail.url.clone();
        let url2 = url.clone();
        let number = self.number;
        let busy = self.busy.is_some();
        let open = detail.state == "OPEN";
        let body = match self.tab {
            PrTab::Overview => self.render_overview(&detail, cx),
            PrTab::Files => self.diff.clone().into_any_element(),
        };
        div()
            .id("pr-view")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_3()
                    .py_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .border_b_1()
                    .border_color(c.border)
                    .child(div().text_lg().child(format!("#{} {}", detail.number, detail.title)))
                    .child(div().text_xs().text_color(c.muted_foreground).child(format!(
                        "{} wants to merge {} into {} · {}{} · +{} −{} in {} files",
                        detail.author.as_ref().map(|a| a.login.clone()).unwrap_or_default(),
                        detail.head_ref_name,
                        detail.base_ref_name,
                        detail.state.to_lowercase(),
                        if detail.is_draft { " · draft" } else { "" },
                        detail.additions,
                        detail.deletions,
                        detail.changed_files,
                    )))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_1()
                            .child(
                                Button::new("pr-browser")
                                    .outline()
                                    .size(ButtonSize::Sm)
                                    .label("Open in Browser")
                                    .on_click(move |_, _, cx| cx.open_url(&url)),
                            )
                            .child(
                                Button::new("pr-copy")
                                    .ghost()
                                    .size(ButtonSize::Sm)
                                    .label("Copy Link")
                                    .on_click(move |_, _, cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(url2.clone()))
                                    }),
                            )
                            .child(
                                Button::new("pr-checkout")
                                    .ghost()
                                    .size(ButtonSize::Sm)
                                    .label("Check Out")
                                    .disabled(busy)
                                    .on_click(cx.listener(|this, _, _, cx| this.checkout(cx))),
                            )
                            .when(detail.is_draft && open, |el| {
                                el.child(
                                    Button::new("pr-ready")
                                        .ghost()
                                        .size(ButtonSize::Sm)
                                        .label("Mark Ready for Review")
                                        .disabled(busy)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.action(PrAction::Ready, cx)
                                        })),
                                )
                            })
                            .when(open, |el| {
                                el.child(
                                    Button::new("pr-merge")
                                        .primary()
                                        .size(ButtonSize::Sm)
                                        .label("Merge…")
                                        .disabled(busy)
                                        .on_click(cx.listener(|this, _, _, cx| this.merge(cx))),
                                )
                                .child(
                                    Button::new("pr-close")
                                        .ghost()
                                        .size(ButtonSize::Sm)
                                        .label("Close…")
                                        .disabled(busy)
                                        .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                                )
                            })
                            .child(
                                Button::new("pr-sessions")
                                    .ghost()
                                    .size(ButtonSize::Sm)
                                    .label("Open Linked Claude Sessions")
                                    .tooltip("Runs `claude --from-pr` in a new terminal tab")
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        let claude = model(cx)
                                            .read(cx)
                                            .claude_path()
                                            .map(|p| p.display().to_string())
                                            .unwrap_or_else(|| "claude".into());
                                        cx.emit(WorkspaceEvent::RunInTerminal(vec![
                                            claude,
                                            "--from-pr".into(),
                                            number.to_string(),
                                        ]))
                                    })),
                            )
                            .child(
                                Button::new("pr-refresh")
                                    .ghost()
                                    .size(ButtonSize::Sm)
                                    .icon(Icon::from(gpui_kit::assets::IconName::RefreshCw))
                                    .tooltip("Refresh")
                                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                            ),
                    )
                    .children(self.busy.clone().map(|b| {
                        div().text_xs().text_color(c.muted_foreground).child(b)
                    }))
                    .child(
                        div()
                            .flex()
                            .gap_0p5()
                            .child(
                                Button::new("pr-tab-overview")
                                    .ghost()
                                    .size(ButtonSize::Sm)
                                    .label("Overview")
                                    .toggled(self.tab == PrTab::Overview)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.tab = PrTab::Overview;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                Button::new("pr-tab-files")
                                    .ghost()
                                    .size(ButtonSize::Sm)
                                    .label("Files Changed")
                                    .toggled(self.tab == PrTab::Files)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.tab = PrTab::Files;
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }
}

// ---------------------------------------------------------------------
// CI run tab
// ---------------------------------------------------------------------

pub struct RunView {
    repo: PathBuf,
    id: u64,
    title: String,
    detail: Option<RunDetail>,
    error: Option<String>,
    busy: bool,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
    _gh: gpui_kit::Subscription,
}

impl EventEmitter<WorkspaceEvent> for RunView {}

impl Focusable for RunView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl RunView {
    pub fn new(repo: PathBuf, id: u64, title: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            repo,
            id,
            title,
            detail: None,
            error: None,
            busy: false,
            focus: cx.focus_handle(),
            _tasks: Vec::new(),
            _gh: on_gh_ready(window, cx, |this: &mut Self, _, cx| {
                if this.detail.is_none() {
                    this.refresh(cx)
                }
            }),
        };
        this.refresh(cx);
        this
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let Some(gh) = gh_ready(cx) else {
            return;
        };
        let (repo, id) = (self.repo.clone(), self.id);
        let task = cx.background_spawn(async move { gh.run_view(&repo, id) });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                match r {
                    Ok(d) => {
                        this.detail = Some(d);
                        this.error = None;
                    }
                    Err(e) => this.error = Some(gh_error(&e)),
                }
                cx.notify();
            });
        }));
    }

    fn rerun(&mut self, cx: &mut Context<Self>) {
        let Some(gh) = model(cx).read(cx).gh() else {
            return;
        };
        self.busy = true;
        let (repo, id) = (self.repo.clone(), self.id);
        let task = cx.background_spawn(async move { gh.run_rerun_failed(&repo, id) });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = false;
                match r {
                    Ok(()) => cx.emit(WorkspaceEvent::Toast("Re-running the failed jobs.".into())),
                    Err(e) => cx.emit(WorkspaceEvent::Prompt(Prompt::message(
                        "Could not re-run",
                        gh_error(&e),
                    ))),
                }
                this.refresh(cx);
            });
        }));
        cx.notify();
    }
}

impl Render for RunView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let Some(detail) = self.detail.clone() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(c.muted_foreground)
                .child(self.error.clone().unwrap_or_else(|| "Loading run…".into()))
                .into_any_element();
        };
        let (badge, col) = run_badge(detail.state(), &c);
        let failed = detail.state() == RunState::Failure;
        let url = detail.url.clone();
        let (repo1, repo2) = (self.repo.clone(), self.repo.clone());
        let (id, title) = (self.id, self.title.clone());
        let title2 = title.clone();
        let branch = detail.head_branch.clone();
        div()
            .id("run-view")
            .track_focus(&self.focus)
            .size_full()
            .overflow_y_scroll()
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_lg()
                    .child(div().text_color(col).child(badge))
                    .child(format!("{} · {}", detail.workflow_name, detail.display_title)),
            )
            .child(div().text_xs().text_color(c.muted_foreground).child(format!(
                "{} · {} {}",
                detail.head_branch,
                detail.status,
                detail.conclusion
            )))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_1()
                    .child(
                        Button::new("run-browser")
                            .outline()
                            .size(ButtonSize::Sm)
                            .label("Open in Browser")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    )
                    .when(failed, |el| {
                        el.child(
                            Button::new("run-log")
                                .ghost()
                                .size(ButtonSize::Sm)
                                .label("Show Failed Logs")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(WorkspaceEvent::OpenDoc(DocKey::RunLog {
                                        repo: repo1.clone(),
                                        id,
                                        title: title.clone(),
                                    }))
                                })),
                        )
                        .child(
                            Button::new("run-rerun")
                                .ghost()
                                .size(ButtonSize::Sm)
                                .label("Re-run Failed Jobs")
                                .disabled(self.busy)
                                .on_click(cx.listener(|this, _, _, cx| this.rerun(cx))),
                        )
                        .child(
                            Button::new("run-fix")
                                .ghost()
                                .size(ButtonSize::Sm)
                                .label("Ask Claude to Fix")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    ask_claude_to_fix(
                                        repo2.clone(),
                                        id,
                                        format!("run \"{title2}\" on branch {branch}"),
                                        cx,
                                    )
                                })),
                        )
                    })
                    .child(
                        Button::new("run-refresh")
                            .ghost()
                            .size(ButtonSize::Sm)
                            .icon(Icon::from(gpui_kit::assets::IconName::RefreshCw))
                            .tooltip("Refresh")
                            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
                    ),
            )
            .child(div().text_xs().text_color(c.muted_foreground).child("Jobs"))
            .children(detail.jobs.iter().map(|job| {
                let (b, col) = run_badge(job.state(), &c);
                div()
                    .flex()
                    .gap_2()
                    .text_sm()
                    .child(div().text_color(col).child(b))
                    .child(job.name.clone())
            }))
            .into_any_element()
    }
}

// ---------------------------------------------------------------------
// Failed log tab
// ---------------------------------------------------------------------

pub struct LogView {
    state: Entity<EditorState>,
    repo: PathBuf,
    id: u64,
    loading: bool,
    started: bool,
    error: Option<String>,
    _task: Option<Task<()>>,
    _gh: Option<gpui_kit::Subscription>,
}

impl EventEmitter<WorkspaceEvent> for LogView {}

impl Focusable for LogView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.state.read(cx).focus_handle(cx)
    }
}

impl LogView {
    pub fn new(repo: PathBuf, id: u64, _title: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let state = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("plaintext")
                .line_number(true)
                .searchable(true)
        });
        state.update(cx, |s, cx| s.set_readonly(true, cx));
        let mut this = Self {
            state,
            repo,
            id,
            loading: true,
            started: false,
            error: None,
            _task: None,
            _gh: None,
        };
        this._gh = Some(on_gh_ready(window, cx, |this: &mut Self, window, cx| {
            this.load(window, cx)
        }));
        this.load(window, cx);
        this
    }

    fn load(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.started {
            return;
        }
        if let Some(gh) = gh_ready(cx) {
            self.started = true;
            let (repo, id) = (self.repo.clone(), self.id);
            let task = cx.background_spawn(async move { gh.run_failed_log(&repo, id) });
            self._task = Some(cx.spawn_in(window, async move |this, cx| {
                let r = task.await;
                let _ = this.update_in(cx, |this, window, cx| {
                    this.loading = false;
                    match r {
                        Ok(log) => {
                            let text = if log.trim().is_empty() {
                                "No failed jobs (or the log is no longer available).".to_owned()
                            } else {
                                log
                            };
                            this.state.update(cx, |s, cx| s.set_value(text, window, cx));
                        }
                        Err(e) => this.error = Some(gh_error(&e)),
                    }
                    cx.notify();
                });
            }));
        }
    }
}

impl Render for LogView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        if let Some(e) = &self.error {
            return div().p_3().text_color(c.destructive).child(e.clone()).into_any_element();
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .when(self.loading, |el| {
                el.child(div().p_2().text_xs().text_color(c.muted_foreground).child("Downloading the log…"))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(Editor::new(&self.state).bordered(false).readonly(true).h_full()),
            )
            .into_any_element()
    }
}

// ---------------------------------------------------------------------
// New pull request
// ---------------------------------------------------------------------

pub struct CreatePrView {
    repo: PathBuf,
    title: Entity<InputState>,
    body: Entity<TextareaState>,
    base: Entity<InputState>,
    draft: bool,
    branch: Option<String>,
    busy: Option<String>,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<WorkspaceEvent> for CreatePrView {}

impl Focusable for CreatePrView {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.title.read(cx).focus_handle(cx)
    }
}

impl CreatePrView {
    pub fn new(repo: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let title = cx.new(|cx| InputState::new(window, cx).placeholder("Title"));
        let body = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Description")
                .auto_grow(6, 20)
        });
        let base = cx.new(|cx| InputState::new(window, cx).placeholder("Base branch"));
        let mut this = Self {
            repo,
            title,
            body,
            base,
            draft: false,
            branch: None,
            busy: Some("Reading the branch…".into()),
            focus: cx.focus_handle(),
            _tasks: Vec::new(),
        };
        this.prefill(window, cx);
        this
    }

    /// Base: the repository's default branch. Title and body: the branch's
    /// commits.
    fn prefill(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (git, gh) = {
            let m = model(cx).read(cx);
            (m.git(), m.gh())
        };
        let repo = self.repo.clone();
        let task = cx.background_spawn(async move {
            let status = git.status(&repo).ok();
            let branch = status.and_then(|s| s.branch.head);
            let base = gh
                .and_then(|gh| gh.repo(&repo).ok())
                .and_then(|i| i.default_branch().map(str::to_owned))
                .unwrap_or_else(|| "main".into());
            let commits = git
                .commits_since(&repo, &format!("origin/{base}"))
                .or_else(|_| git.commits_since(&repo, &base))
                .unwrap_or_default();
            (branch, base, commits)
        });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let (branch, base, commits) = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                let (t, b) = gh::pr_text_from_commits(branch.as_deref().unwrap_or(""), &commits);
                this.title.update(cx, |s, cx| s.set_value(t, window, cx));
                this.body.update(cx, |s, cx| s.set_value(b, window, cx));
                this.base.update(cx, |s, cx| s.set_value(base, window, cx));
                this.branch = branch;
                cx.notify();
            });
        }));
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let new = NewPr {
            base: self.base.read(cx).value().trim().to_owned(),
            title: self.title.read(cx).value().trim().to_owned(),
            body: self.body.read(cx).value().to_string(),
            draft: self.draft,
        };
        if new.title.is_empty() || new.base.is_empty() {
            cx.emit(WorkspaceEvent::Toast("A title and a base branch are needed.".into()));
            return;
        }
        let (git, gh) = {
            let m = model(cx).read(cx);
            (m.git(), m.gh())
        };
        let Some(gh) = gh else {
            return;
        };
        let repo = self.repo.clone();
        self.busy = Some("Pushing the branch and creating the pull request…".into());
        cx.notify();
        // The push, if any, goes into the Git activity log.
        let pushed: std::sync::Arc<std::sync::Mutex<Option<anveg_core::git_activity::Entry>>> =
            Default::default();
        let pushed_in = pushed.clone();
        let task = cx.background_spawn(async move {
            // Push first when the branch is not published or has new commits.
            let status = git.status(&repo)?;
            let op = if status.branch.upstream.is_none() {
                let remotes = git.remotes(&repo)?;
                let remote = remotes
                    .iter()
                    .find(|r| *r == "origin")
                    .or(remotes.first())
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("This repository has no remote."))?;
                Some(NetworkOp::Push {
                    set_upstream: Some(remote),
                })
            } else if status.branch.ahead > 0 {
                Some(NetworkOp::Push { set_upstream: None })
            } else {
                None
            };
            if let Some(op) = op {
                let (result, entry) =
                    anveg_core::git_activity::run_logged(&git, &repo, &op, &CancelToken::new(), None);
                if let Ok(mut slot) = pushed_in.lock() {
                    *slot = Some(entry);
                }
                result?;
            }
            gh.pr_create(&repo, &new)
        });
        let repo = self.repo.clone();
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                if let Some(entry) = pushed.lock().ok().and_then(|mut e| e.take()) {
                    cx.emit(WorkspaceEvent::GitActivity(Box::new(entry)));
                }
                match result {
                    Ok(url) => {
                        cx.emit(WorkspaceEvent::Toast(format!("Created {url}").into()));
                        cx.emit(WorkspaceEvent::GitChanged);
                        let number = url
                            .trim_end_matches('/')
                            .rsplit('/')
                            .next()
                            .and_then(|n| n.parse::<u64>().ok());
                        if let Some(number) = number {
                            cx.emit(WorkspaceEvent::OpenDoc(DocKey::PullRequest {
                                repo: repo.clone(),
                                number,
                                title: String::new(),
                            }));
                        }
                        cx.emit(WorkspaceEvent::CloseDoc(DocKey::CreatePullRequest { repo }));
                    }
                    Err(e) => cx.emit(WorkspaceEvent::Prompt(Prompt::message(
                        "Could not create the pull request",
                        format!("{e:#}"),
                    ))),
                }
                cx.notify();
            });
        }));
    }
}

impl Render for CreatePrView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let busy = self.busy.is_some();
        div()
            .id("create-pr")
            .track_focus(&self.focus)
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_2()
            .max_w(px(760.))
            .child(div().text_lg().child("New Pull Request"))
            .child(div().text_sm().text_color(c.muted_foreground).child(format!(
                "From {}",
                self.branch.clone().unwrap_or_else(|| "the current branch".into())
            )))
            .child(div().text_xs().text_color(c.muted_foreground).child("Base branch"))
            .child(Input::new(&self.base))
            .child(div().text_xs().text_color(c.muted_foreground).child("Title"))
            .child(Input::new(&self.title))
            .child(div().text_xs().text_color(c.muted_foreground).child("Description"))
            .child(Textarea::new(&self.body))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        Switch::new("pr-draft")
                            .checked(self.draft)
                            .on_change(cx.listener(|this, on: &bool, _, cx| {
                                this.draft = *on;
                                cx.notify();
                            })),
                    )
                    .child(div().text_sm().child("Draft"))
                    .child(div().flex_1())
                    .child(
                        Button::new("pr-create")
                            .primary()
                            .label("Create Pull Request")
                            .disabled(busy)
                            .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
                    ),
            )
            .children(self.busy.clone().map(|b| {
                div().text_xs().text_color(c.muted_foreground).child(b)
            }))
    }
}
