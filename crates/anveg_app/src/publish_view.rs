//! Publish to GitHub (File ▸ Publish to GitHub…): the current project
//! becomes a new GitHub repository. Name (the folder's by default),
//! description, private or public, and owner (you or one of your
//! organizations). Then: create it, `git init` if needed, a first commit if
//! there is none (asked first), add `origin`, push the branch.

use std::path::PathBuf;

use anveg_core::github_api::NewRepo;
use anveg_core::github_publish::{self, Precheck};
use anveg_core::process::CancelToken;
use gpui_cn::prelude::*;
use gpui_cn::{Input, InputState, Switch};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Task, Window, div, px,
};

use crate::model::{ModelEvent, model};
use crate::ui::Colors;
use crate::workspace::{ButtonStyle, Prompt, WorkspaceEvent};

enum Stage {
    Form,
    Working(String),
    Done { url: String },
}

pub struct PublishView {
    root: PathBuf,
    name: Entity<InputState>,
    description: Entity<InputState>,
    private: bool,
    /// You first, then your organizations.
    owners: Vec<String>,
    owner: usize,
    stage: Stage,
    error: Option<String>,
    focus: FocusHandle,
    _tasks: Vec<Task<()>>,
    _subs: Vec<gpui_kit::Subscription>,
}

impl EventEmitter<WorkspaceEvent> for PublishView {}

impl Focusable for PublishView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl PublishView {
    pub fn new(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let folder = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let default = github_publish::repo_name_from_folder(&folder);
        let name = cx.new(|cx| InputState::new(window, cx).default_value(default));
        let description = cx.new(|cx| InputState::new(window, cx).placeholder("Optional"));
        let m = model(cx);
        let subs = vec![cx.subscribe(&m, |this, _, event, cx| {
            if matches!(event, ModelEvent::GitHubChanged) {
                this.load_owners(cx);
                cx.notify();
            }
        })];
        let mut this = Self {
            root,
            name,
            description,
            private: true,
            owners: Vec::new(),
            owner: 0,
            stage: Stage::Form,
            error: None,
            focus: cx.focus_handle(),
            _tasks: Vec::new(),
            _subs: subs,
        };
        this.load_owners(cx);
        this
    }

    fn load_owners(&mut self, cx: &mut Context<Self>) {
        let Some(gh) = model(cx).read(cx).github() else {
            return;
        };
        let task = cx.background_spawn(async move {
            let me = gh.user()?.login;
            let orgs = gh.orgs().unwrap_or_default();
            Ok::<_, anyhow::Error>(std::iter::once(me).chain(orgs.into_iter().map(|o| o.login)).collect::<Vec<_>>())
        });
        self._tasks.push(cx.spawn(async move |this, cx| {
            let owners = task.await;
            let _ = this.update(cx, |this, cx| {
                match owners {
                    Ok(o) => {
                        this.owners = o;
                        this.owner = this.owner.min(this.owners.len().saturating_sub(1));
                    }
                    Err(e) => this.error = Some(format!("Could not list your accounts: {e:#}")),
                }
                cx.notify();
            });
        }));
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_owned();
        if let Some(problem) = github_publish::check_repo_name(&name) {
            self.error = Some(problem.into());
            cx.notify();
            return;
        }
        self.error = None;
        let git = model(cx).read(cx).git();
        let root = self.root.clone();
        self.stage = Stage::Working("Checking the project…".into());
        cx.notify();
        let task = cx.background_spawn(async move { github_publish::precheck(&git, &root) });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let pre = task.await;
            let _ = this.update_in(cx, |this, window, cx| this.after_precheck(pre, window, cx));
        }));
    }

    fn after_precheck(&mut self, pre: Precheck, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(outer) = &pre.inside {
            self.fail(
                format!(
                    "This folder is inside another git repository ({}). Publish that repository, or move the folder out of it.",
                    outer.display()
                ),
                cx,
            );
            return;
        }
        if let Some(url) = &pre.origin {
            self.fail(
                format!(
                    "This project already has a remote named origin ({url}), so publishing stopped and nothing changed. \
                     To publish anyway, rename it first in the Terminal: git remote rename origin old-origin"
                ),
                cx,
            );
            return;
        }
        if pre.has_head {
            self.publish(pre, false, window, cx);
            return;
        }
        // No commits yet: ask before committing everything.
        let weak = cx.entity().downgrade();
        let pre2 = pre.clone();
        let body = if pre.is_repo {
            "This repository has no commits yet. Publishing makes a first commit (“Initial commit”) of all the project's files, except those .gitignore leaves out, then pushes it."
        } else {
            "This folder is not a git repository yet. Publishing runs git init, makes a first commit (“Initial commit”) of all its files, except those .gitignore leaves out, then pushes it."
        };
        let prompt = Prompt::new("Make the first commit?", body)
            .button("Cancel", ButtonStyle::Outline, {
                let weak = weak.clone();
                move |_, _, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.stage = Stage::Form;
                        cx.notify();
                    });
                    Ok(())
                }
            })
            .button("Commit and Publish", ButtonStyle::Primary, move |_, window, cx| {
                let _ = weak.update(cx, |this, cx| this.publish(pre2.clone(), true, window, cx));
                Ok(())
            });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn publish(&mut self, pre: Precheck, commit_first: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(gh) = model(cx).read(cx).github() else {
            self.fail("Sign in to GitHub first.".into(), cx);
            return;
        };
        let org = (self.owner > 0).then(|| self.owners.get(self.owner).cloned()).flatten();
        let new = NewRepo {
            org,
            name: self.name.read(cx).value().trim().to_owned(),
            description: self.description.read(cx).value().trim().to_owned(),
            private: self.private,
        };
        self.stage = Stage::Working(format!("Creating {} on GitHub and pushing…", new.name));
        cx.notify();
        let task = cx.background_spawn(async move {
            github_publish::publish(&gh, &pre, &new, commit_first, &CancelToken::new())
        });
        self._tasks.push(cx.spawn_in(window, async move |this, cx| {
            let (result, entry) = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Some(entry) = entry {
                    cx.emit(WorkspaceEvent::GitActivity(Box::new(entry)));
                }
                cx.emit(WorkspaceEvent::GitChanged);
                match result {
                    Ok(created) => {
                        cx.emit(WorkspaceEvent::Toast(format!("Published {}.", created.full_name).into()));
                        this.stage = Stage::Done {
                            url: created.html_url,
                        };
                    }
                    Err(e) => this.fail(format!("{e:#}"), cx),
                }
                cx.notify();
            });
        }));
    }

    fn fail(&mut self, message: String, cx: &mut Context<Self>) {
        self.stage = Stage::Form;
        self.error = Some(message);
        cx.notify();
    }
}

impl Render for PublishView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let signed_in = model(cx).read(cx).is_signed_in();
        let mut page = div()
            .id("publish-view")
            .track_focus(&self.focus)
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .max_w(px(640.))
            .child(div().text_lg().child("Publish to GitHub"))
            .child(
                div()
                    .text_sm()
                    .text_color(c.muted_foreground)
                    .child(format!("Creates a new GitHub repository for {} and pushes it.", self.root.display())),
            );
        if !signed_in {
            return page
                .child(crate::github::sign_in_panel("publish", cx))
                .into_any_element();
        }
        if let Stage::Done { url } = &self.stage {
            let url = url.clone();
            return page
                .child(div().text_sm().child(format!("Published: {url}")))
                .child(
                    div().flex().gap_2().child(
                        Button::new("pub-open")
                            .primary()
                            .label("Open on GitHub")
                            .on_click(move |_, _, cx| cx.open_url(&url)),
                    ),
                )
                .into_any_element();
        }
        let working = matches!(self.stage, Stage::Working(_));
        let label = |t: &'static str| {
            div()
                .w(px(110.))
                .flex_shrink_0()
                .text_sm()
                .text_color(c.muted_foreground)
                .child(t)
        };
        let mut owners = div().flex().flex_wrap().gap_1();
        if self.owners.is_empty() {
            owners = owners.child(div().text_sm().text_color(c.muted_foreground).child("Loading…"));
        }
        for (i, o) in self.owners.iter().enumerate() {
            owners = owners.child(
                Button::new(SharedString::from(format!("owner-{i}")))
                    .ghost()
                    .size(ButtonSize::Sm)
                    .label(if i == 0 { format!("{o} (you)") } else { o.clone() })
                    .toggled(self.owner == i)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.owner = i;
                        cx.notify();
                    })),
            );
        }
        page = page
            .child(div().flex().items_center().gap_2().child(label("Owner")).child(owners))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(label("Name"))
                    .child(div().flex_1().child(Input::new(&self.name))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(label("Description"))
                    .child(div().flex_1().child(Input::new(&self.description))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(label("Visibility"))
                    .child(
                        Switch::new("pub-private")
                            .checked(self.private)
                            .on_change(cx.listener(|this, v: &bool, _, cx| {
                                this.private = *v;
                                cx.notify();
                            })),
                    )
                    .child(div().text_sm().child(if self.private {
                        "Private: only you and the people you invite"
                    } else {
                        "Public: anyone can see it"
                    })),
            );
        if let Some(e) = &self.error {
            page = page.child(
                div()
                    .p_3()
                    .rounded_md()
                    .bg(c.destructive.opacity(0.08))
                    .text_sm()
                    .text_color(c.destructive)
                    .child(e.clone()),
            );
        }
        page.child(
            div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    Button::new("pub-go")
                        .primary()
                        .label("Publish")
                        .disabled(working || self.owners.is_empty())
                        .on_click(cx.listener(|this, _, window, cx| this.start(window, cx))),
                )
                .children(match &self.stage {
                    Stage::Working(s) => Some(div().text_sm().text_color(c.muted_foreground).child(s.clone())),
                    _ => None,
                }),
        )
        .into_any_element()
    }
}
