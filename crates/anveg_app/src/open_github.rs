//! Open from GitHub (File ▸ Open from GitHub…, and the welcome window):
//! pick one of your repositories (most recently updated first, with a
//! filter) or paste any owner/repo or URL, choose a parent folder, and the
//! repository is cloned into `<parent>/<name>` and opened as a project.
//! Cloning shows git's progress and can be cancelled.

use std::sync::Arc;

use anveg_core::github_api::{self, RepoItem};
use anveg_core::github_publish;
use anveg_core::process::CancelToken;
use futures::StreamExt as _;
use gpui_cn::prelude::*;
use gpui_cn::{Input, InputEvent, InputState, TitleBar};
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Task, Window, WindowBounds, WindowOptions, div, px, size,
};

use crate::actions::CloseWindow;
use crate::model::{ModelEvent, model};
use crate::ui::Colors;

struct CloneJob {
    progress: String,
    cancel: CancelToken,
}

pub struct OpenFromGitHub {
    filter: Entity<InputState>,
    slug: Entity<InputState>,
    repos: Vec<RepoItem>,
    loading: bool,
    selected: Option<String>,
    error: Option<String>,
    job: Option<CloneJob>,
    log_view: Entity<crate::op_log_view::OpLogView>,
    _tasks: Vec<Task<()>>,
    _subs: Vec<gpui_kit::Subscription>,
}

/// Open the window (or bring it forward).
pub fn open(cx: &mut App) {
    let m = model(cx);
    if let Some(handle) = m.read(cx).github_window
        && handle.update(cx, |_, w, _| w.activate_window()).is_ok()
    {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(620.), px(640.)), cx))),
        ..TitleBar::window_options(cx)
    };
    match gpui_kit::open_window(options, cx, |window, cx| {
        window.set_window_title("Open from GitHub");
        cx.new(|cx| OpenFromGitHub::new(window, cx))
    }) {
        Ok((handle, view)) => {
            m.update(cx, |m, _| m.github_window = Some(handle));
            let m2 = m.clone();
            view.update(cx, |_, cx| {
                cx.on_release(move |_, cx| m2.update(cx, |m, _| m.github_window = None))
                    .detach();
            });
        }
        Err(error) => tracing::error!(%error, "could not open the Open from GitHub window"),
    }
}

impl OpenFromGitHub {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let filter = cx.new(|cx| InputState::new(window, cx).placeholder("Filter your repositories"));
        let slug = cx.new(|cx| InputState::new(window, cx).placeholder("or paste owner/repo or a GitHub URL"));
        let m = model(cx);
        let subs = vec![
            cx.subscribe(&filter, |_, _, _: &InputEvent, cx| cx.notify()),
            cx.subscribe_in(&slug, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    this.selected = None;
                    cx.notify();
                }
                InputEvent::PressEnter { .. } => this.choose_folder(window, cx),
                _ => {}
            }),
            cx.subscribe(&m, |this, _, event, cx| {
                if matches!(event, ModelEvent::GitHubChanged) {
                    this.load(cx);
                }
                cx.notify();
            }),
        ];
        let mut this = Self {
            filter,
            slug,
            repos: Vec::new(),
            loading: false,
            selected: None,
            error: None,
            job: None,
            log_view: cx.new(|cx| {
                let log = model(cx).read(cx).github_log.clone();
                crate::op_log_view::OpLogView::standalone("open-github-log", "GitHub log", log, false, 160., window, cx)
            }),
            _tasks: Vec::new(),
            _subs: subs,
        };
        this.load(cx);
        this
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(gh) = model(cx).read(cx).github() else {
            return;
        };
        self.loading = true;
        let task = cx.background_spawn(async move { gh.user_repos(500) });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let r = task.await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match r {
                    Ok(repos) => this.repos = repos,
                    Err(e) => this.error = Some(format!("Could not list your repositories: {e:#}")),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// What to clone: (name for the folder, clone URL).
    fn target(&self, cx: &App) -> Option<(String, String)> {
        if let Some(full) = &self.selected
            && let Some(r) = self.repos.iter().find(|r| &r.full_name == full)
        {
            return Some((r.name.clone(), r.clone_url.clone()));
        }
        let text = self.slug.read(cx).value().to_string();
        let (owner, name) = github_api::parse_slug(&text)?;
        Some((name.clone(), format!("https://github.com/{owner}/{name}.git")))
    }

    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.job.is_some() {
            return;
        }
        let Some((name, url)) = self.target(cx) else {
            self.error = Some("Choose a repository, or paste owner/repo or a GitHub URL.".into());
            cx.notify();
            return;
        };
        self.error = None;
        // Development aid: ANVEG_PICK_FOLDER skips the folder picker in
        // debug builds (Xvfb has no native dialog).
        if cfg!(debug_assertions)
            && let Some(parent) = std::env::var_os("ANVEG_PICK_FOLDER").filter(|v| !v.is_empty())
        {
            match github_publish::clone_target(std::path::Path::new(&parent), &name) {
                Ok(dest) => self.clone_into(url, dest, window, cx),
                Err(e) => self.error = Some(format!("{e:#}")),
            }
            cx.notify();
            return;
        }
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Clone Here".into()),
        });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(parent) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                match github_publish::clone_target(&parent, &name) {
                    Ok(dest) => this.clone_into(url, dest, window, cx),
                    Err(e) => {
                        this.error = Some(format!("{e:#}"));
                        cx.notify();
                    }
                }
            });
        }));
    }

    fn clone_into(&mut self, url: String, dest: std::path::PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let (git, log) = {
            let m = model(cx).read(cx);
            (m.git(), m.github_log.clone())
        };
        let cancel = CancelToken::new();
        self.job = Some(CloneJob {
            progress: format!("Cloning into {}…", dest.display()),
            cancel: cancel.clone(),
        });
        cx.notify();
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<String>();
        let progress: anveg_core::process::Progress = Arc::new(move |line: &str| {
            let _ = tx.unbounded_send(line.to_owned());
        });
        let d = dest.clone();
        let task = cx.background_spawn(async move {
            let label = format!("Cloning {url} into {}", d.display());
            log.op(&label, || git.clone_repo(&url, &d, &cancel, Some(progress)))
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            while let Some(line) = rx.next().await {
                let _ = this.update(cx, |this, cx| {
                    if let Some(job) = &mut this.job {
                        job.progress = line;
                        cx.notify();
                    }
                });
            }
        }));
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let result = task.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.job = None;
                match result {
                    Ok(()) => {
                        let dest = dest.clone();
                        cx.defer(move |cx| crate::windows::open_folder(dest, cx));
                        window.remove_window();
                    }
                    Err(e) if anveg_core::process::is_cancelled(&e) => {
                        // A cancelled clone leaves a partial folder: remove it.
                        let _ = std::fs::remove_dir_all(&dest);
                        this.error = Some("Clone cancelled.".into());
                    }
                    Err(e) => {
                        let message = format!("{e:#}");
                        let first = message
                            .lines()
                            .find(|l| l.starts_with("fatal:") || l.starts_with("error:"))
                            .unwrap_or(&message)
                            .trim_start_matches("fatal:")
                            .trim()
                            .to_owned();
                        this.error = Some(format!("Could not clone: {first}"));
                    }
                }
                cx.notify();
            });
        }));
    }
}

impl Render for OpenFromGitHub {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let signed_in = model(cx).read(cx).is_signed_in();
        let filter = self.filter.read(cx).value().to_lowercase();
        let mut list = div()
            .id("gh-repos")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .border_1()
            .border_color(c.border)
            .rounded_md()
            .flex()
            .flex_col();
        if !signed_in {
            list = list.child(crate::github::sign_in_panel("open-github", cx));
        } else if self.loading && self.repos.is_empty() {
            list = list.child(div().p_3().text_sm().text_color(c.muted_foreground).child("Loading your repositories…"));
        }
        let shown: Vec<&RepoItem> = self
            .repos
            .iter()
            .filter(|r| {
                filter.is_empty()
                    || r.full_name.to_lowercase().contains(&filter)
                    || r.description.as_deref().unwrap_or("").to_lowercase().contains(&filter)
            })
            .take(300)
            .collect();
        for (i, r) in shown.into_iter().enumerate() {
            let full = r.full_name.clone();
            let selected = self.selected.as_deref() == Some(full.as_str());
            let mut tags = Vec::new();
            if r.private {
                tags.push("private");
            }
            if r.fork {
                tags.push("fork");
            }
            list = list.child(
                div()
                    .id(SharedString::from(format!("repo-{i}")))
                    .px_3()
                    .py_1p5()
                    .cursor_pointer()
                    .border_b_1()
                    .border_color(c.border)
                    .when(selected, |el| el.bg(c.selected_row))
                    .when(!selected, |el| el.hover(|s| s.bg(c.accent.opacity(0.5))))
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .text_sm()
                            .child(full.clone())
                            .child(div().text_xs().text_color(c.muted_foreground).child(tags.join(" · "))),
                    )
                    .children(r.description.clone().filter(|d| !d.is_empty()).map(|d| {
                        div().text_xs().text_color(c.muted_foreground).truncate().child(d)
                    }))
                    .on_click(cx.listener(move |this, e: &gpui_kit::ClickEvent, window, cx| {
                        this.selected = Some(full.clone());
                        cx.notify();
                        if e.click_count() == 2 {
                            this.choose_folder(window, cx);
                        }
                    })),
            );
        }
        let busy = self.job.is_some();
        div()
            .id("open-github")
            .size_full()
            .flex()
            .flex_col()
            .bg(c.background)
            .text_color(c.foreground)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .child(TitleBar::new())
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .px_6()
                    .pb_5()
                    .child(div().text_lg().child("Open from GitHub"))
                    .child(div().text_sm().text_color(c.muted_foreground).child(
                        "Clone a repository into a folder you choose and open it as a project.",
                    ))
                    .child(Input::new(&self.filter))
                    .child(list)
                    .child(Input::new(&self.slug))
                    .children(self.error.clone().map(|e| div().text_sm().text_color(c.destructive).child(e)))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("clone-go")
                                    .primary()
                                    .label("Clone…")
                                    .disabled(busy || self.target(cx).is_none())
                                    .on_click(cx.listener(|this, _, window, cx| this.choose_folder(window, cx))),
                            )
                            .when_some(self.job.as_ref(), |el, job| {
                                let cancel = job.cancel.clone();
                                el.child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .text_color(c.muted_foreground)
                                        .truncate()
                                        .child(job.progress.clone()),
                                )
                                .child(
                                    Button::new("clone-cancel")
                                        .outline()
                                        .size(ButtonSize::Sm)
                                        .label("Cancel")
                                        .on_click(move |_, _, _| cancel.cancel()),
                                )
                            }),
                    ),
            )
            .child(self.log_view.clone())
    }
}
