//! The window's modal dialogs.

use anveg_core::launcher;
use gpui_cn::prelude::*;
use gpui_cn::{Dialog, Input, InputEvent, InputState};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Styled as _, Window, div, px,
};

use crate::project_window::{DialogKind, ProjectWindow};
use crate::ui::Colors;

impl ProjectWindow {
    /// ⌘N starts a session at once, with no questions: its name comes from
    /// Claude Code (a custom or generated title) or, until then, from its
    /// first prompt. New Session in Worktree… asks only for the worktree.
    pub fn begin_new_session(
        &mut self,
        in_worktree: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !in_worktree {
            self.new_session(None, None, window, cx);
            return;
        }
        if !self.is_git_repo(cx) {
            self.show_message(
                "Not a git repository",
                "Worktree sessions need the project's main folder to be a git repository.",
                cx,
            );
            return;
        }
        let worktree =
            cx.new(|cx| InputState::new(window, cx).placeholder("Worktree name, e.g. feature-x"));
        let sub = cx.subscribe_in(&worktree, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.commit_new_session(window, cx);
            }
        });
        self.push_subscription(sub);
        self.dialog = Some(DialogKind::NewWorktreeSession {
            worktree,
            error: None,
        });
        cx.notify();
    }

    pub fn commit_new_session(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(DialogKind::NewWorktreeSession { worktree, .. }) = &self.dialog else {
            return;
        };
        let worktree = match launcher::normalize_worktree_name(&worktree.read(cx).value()) {
            Ok(w) => w,
            Err(message) => return self.set_dialog_error(message, cx),
        };
        self.dialog = None;
        self.new_session(None, Some(worktree), window, cx);
    }

    fn set_dialog_error(&mut self, message: &str, cx: &mut Context<Self>) {
        match &mut self.dialog {
            Some(DialogKind::NewWorktreeSession { error, .. })
            | Some(DialogKind::Rename { error, .. })
            | Some(DialogKind::ProjectSettings { error, .. })
            | Some(DialogKind::Prompt { error, .. }) => {
                *error = Some(SharedString::from(message.to_owned()))
            }
            _ => {}
        }
        cx.notify();
    }

    pub fn begin_project_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.project_name(cx);
        let name = cx.new(|cx| InputState::new(window, cx).default_value(current));
        let sub = cx.subscribe_in(&name, window, |this, _, event, window, cx| {
            if let InputEvent::PressEnter { .. } = event {
                this.commit_project_settings(window, cx);
            }
        });
        self.push_subscription(sub);
        self.dialog = Some(DialogKind::ProjectSettings { name, error: None });
        cx.notify();
    }

    fn commit_project_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(DialogKind::ProjectSettings { name, .. }) = &self.dialog else {
            return;
        };
        let value = name.read(cx).value().trim().to_owned();
        if value.is_empty() {
            return self.set_dialog_error("The name can't be empty.", cx);
        }
        let id = self.project_id;
        self.model.update(cx, |m, cx| {
            m.update_projects(cx, |p| {
                if let Some(project) = p.get_mut(id) {
                    project.name = value.clone();
                }
            })
        });
        self.close_dialog(window, cx);
    }

    fn add_extra_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: false,
            directories: true,
            multiple: true,
            prompt: Some("Add".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let id = this.project_id;
                this.model.update(cx, |m, cx| {
                    m.update_projects(cx, |p| {
                        if let Some(project) = p.get_mut(id) {
                            for path in paths {
                                let path = std::fs::canonicalize(&path).unwrap_or(path);
                                if path != project.main_folder
                                    && !project.extra_folders.contains(&path)
                                {
                                    project.extra_folders.push(path);
                                }
                            }
                        }
                    })
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn remove_extra_folder(&mut self, index: usize, cx: &mut Context<Self>) {
        let id = self.project_id;
        self.model.update(cx, |m, cx| {
            m.update_projects(cx, |p| {
                if let Some(project) = p.get_mut(id)
                    && index < project.extra_folders.len()
                {
                    project.extra_folders.remove(index);
                }
            })
        });
        cx.notify();
    }

    /// Enter in a dialog: gpui-base's dialog turns it into `Confirm` (and
    /// would just close); route it to the dialog's own action instead.
    fn confirm_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match &self.dialog {
            Some(DialogKind::NewWorktreeSession { .. }) => self.commit_new_session(window, cx),
            Some(DialogKind::Rename { .. }) => self.commit_rename(window, cx),
            Some(DialogKind::ProjectSettings { .. }) => self.commit_project_settings(window, cx),
            Some(DialogKind::Message { .. }) => self.close_dialog(window, cx),
            Some(DialogKind::Prompt { .. }) => self.confirm_prompt(None, window, cx),
            // Destructive or two-way choices need a click.
            _ => {}
        }
    }

    pub fn render_dialog(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        if matches!(self.dialog, Some(DialogKind::Prompt { .. })) {
            return self.render_prompt(cx);
        }
        let c = Colors::of(cx);
        let form = |cx: &mut Context<Self>| {
            div().flex().flex_col().gap_2().on_action(cx.listener(
                |this, _: &gpui_kit::base::actions::Confirm, window, cx| {
                    this.confirm_dialog(window, cx)
                },
            ))
        };
        let dialog = self.dialog.as_ref()?;
        let close = cx.listener(|this, _, window, cx| this.close_dialog(window, cx));
        let weak = cx.entity().downgrade();
        let on_open_change = move |open: bool, window: &mut Window, cx: &mut gpui_kit::App| {
            if !open {
                let _ = weak.update(cx, |this, cx| this.close_dialog(window, cx));
            }
        };
        let error_line = |error: &Option<SharedString>| {
            error
                .clone()
                .map(|e| div().text_sm().text_color(c.destructive).child(e))
        };
        let names_list = |names: &[String]| {
            let shown: Vec<_> = names.iter().take(12).cloned().collect();
            let more = names.len().saturating_sub(shown.len());
            div()
                .flex()
                .flex_col()
                .gap_0p5()
                .text_sm()
                .children(
                    shown
                        .into_iter()
                        .map(|n| div().truncate().child(format!("• {n}"))),
                )
                .when(more > 0, |el| {
                    el.child(
                        div()
                            .text_color(c.muted_foreground)
                            .child(format!("and {more} more")),
                    )
                })
        };
        let el: AnyElement = match dialog {
            DialogKind::NewWorktreeSession { worktree, error } => Dialog::new("dlg-new")
                .open(true)
                .on_open_change(on_open_change)
                .title("New Session in Worktree")
                .description(
                    "Runs `claude --worktree <name>`, which creates <project>/.claude/worktrees/<name>. \
                     The session is named after its first prompt.",
                )
                .track_focus(&worktree.read(cx).focus_handle(cx))
                .child(form(cx).child(Input::new(worktree)).children(error_line(error)))
                .footer(
                    div()
                        .flex()
                        .gap_2()
                        .child(Button::new("dlg-new-cancel").outline().label("Cancel").on_click(close))
                        .child(
                            Button::new("dlg-new-ok")
                                .primary()
                                .label("Start")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.commit_new_session(window, cx)
                                })),
                        ),
                )
                .into_any_element(),
            DialogKind::Rename { input, error, .. } => Dialog::new("dlg-rename")
                .open(true)
                .on_open_change(on_open_change)
                .title("Rename Session")
                .description(
                    "Saved in Anveg Code at once. If the session is running, `/rename` is also sent to Claude Code; \
                     text you haven't sent yet in Claude's prompt would be added to it.",
                )
                .track_focus(&input.read(cx).focus_handle(cx))
                .child(form(cx).child(Input::new(input)).children(error_line(error)))
                .footer(
                    div()
                        .flex()
                        .gap_2()
                        .child(Button::new("dlg-rename-cancel").outline().label("Cancel").on_click(close))
                        .child(
                            Button::new("dlg-rename-ok")
                                .primary()
                                .label("Rename")
                                .on_click(cx.listener(|this, _, window, cx| this.commit_rename(window, cx))),
                        ),
                )
                .into_any_element(),
            DialogKind::Delete { names, .. } => Dialog::new("dlg-delete")
                .open(true)
                .on_open_change(on_open_change)
                .title(format!(
                    "Delete {} session{}?",
                    names.len(),
                    if names.len() == 1 { "" } else { "s" }
                ))
                .description("Their transcripts will be moved to the Trash. Running sessions are stopped first.")
                .child(names_list(names))
                .footer(
                    div()
                        .flex()
                        .gap_2()
                        .child(Button::new("dlg-delete-cancel").outline().label("Cancel").on_click(close))
                        .child(
                            Button::new("dlg-delete-ok")
                                .destructive()
                                .label("Move to Trash")
                                .on_click(cx.listener(|this, _, window, cx| this.confirm_delete(window, cx))),
                        ),
                )
                .into_any_element(),
            DialogKind::ConfirmClose { names, quit, dont_ask } => {
                let quit = *quit;
                let dont_ask = *dont_ask;
                Dialog::new("dlg-close")
                    .open(true)
                    .on_open_change(on_open_change)
                    .title(format!(
                        "Stop {} running session{}?",
                        names.len(),
                        if names.len() == 1 { "" } else { "s" }
                    ))
                    .description("You can resume them later.")
                    .child(names_list(names))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_sm()
                            .child(
                                Switch::new("dlg-close-dont-ask")
                                    .checked(dont_ask)
                                    .accessibility_label("Don't show this again")
                                    .on_change(cx.listener(|this, checked: &bool, _, cx| {
                                        if let Some(DialogKind::ConfirmClose { dont_ask, .. }) = &mut this.dialog {
                                            *dont_ask = *checked;
                                        }
                                        cx.notify();
                                    })),
                            )
                            .child("Don't show this again"),
                    )
                    .footer(
                        div()
                            .flex()
                            .gap_2()
                            .child(Button::new("dlg-close-cancel").outline().label("Cancel").on_click(close))
                            .child(
                                Button::new("dlg-close-ok")
                                    .destructive()
                                    .label(if quit { "Stop and Quit" } else { "Stop and Close" })
                                    .on_click(cx.listener(|this, _, window, cx| this.confirm_close(window, cx))),
                            ),
                    )
                    .into_any_element()
            }
            DialogKind::OpenElsewhere {
                session_id,
                other,
                stopping,
                error,
            } => {
                use anveg_core::procinfo::Host;
                let id = session_id.clone();
                let stopping = *stopping;
                let busy = other.status.as_deref() == Some("busy");
                let background = other.kind.as_deref() == Some("background");
                let (where_, hint) = match &other.host {
                    Some(Host::App(app)) => (
                        format!("Claude Code is running it in {app}."),
                        format!("Look for a window or tab in {app} running Claude Code, and quit it there (/exit)."),
                    ),
                    Some(Host::Program(program)) => (
                        format!("Claude Code is running it under {program}, with no app window around it."),
                        format!("Look where you run {program}."),
                    ),
                    Some(Host::ThisApp) if other.shown_in.is_some() => (
                        "It is running in another project tab of this app.".to_owned(),
                        "Show It goes there.".to_owned(),
                    ),
                    Some(Host::ThisApp) => (
                        "This app started it, but no tab shows it: a process left over from earlier."
                            .to_owned(),
                        "Nothing will be lost if you stop it: the conversation is saved as it goes."
                            .to_owned(),
                    ),
                    Some(Host::Detached) => (
                        "It is running with no app or window around it.".to_owned(),
                        "It was started in the background, or the window it ran in was closed and it kept running."
                            .to_owned(),
                    ),
                    None => (
                        "Claude Code reports it as running, but not in which process.".to_owned(),
                        "Quit Claude Code wherever it runs, then open the session again.".to_owned(),
                    ),
                };
                let mut details: Vec<(&str, String)> = Vec::new();
                if let Some(folder) = &other.folder {
                    details.push(("Folder", crate::ui::tilde(folder)));
                }
                let status = match other.status.as_deref() {
                    Some("busy") => Some("Working".to_owned()),
                    Some("idle") => Some("Waiting for input".to_owned()),
                    Some(other) => Some(other.to_owned()),
                    None => None,
                };
                if let Some(status) = status {
                    let status = if background {
                        format!("{status} (a background agent)")
                    } else {
                        status
                    };
                    details.push(("Status", status));
                }
                if let Some(at) = other.started_at {
                    details.push((
                        "Started",
                        anveg_core::time::relative(at, anveg_core::time::now_millis()),
                    ));
                }
                if let Some(pid) = other.pid {
                    details.push(("Process", pid.to_string()));
                }
                let grid = div().flex().flex_col().gap_1().text_sm().children(details.into_iter().map(
                    |(k, v)| {
                        div()
                            .flex()
                            .gap_3()
                            .child(div().w(px(64.)).flex_shrink_0().text_color(c.muted_foreground).child(k))
                            .child(div().flex_1().min_w_0().truncate().child(v))
                    },
                ));
                let shown_in = other.shown_in;
                let can_stop = other.pid.is_some() && shown_in.is_none();
                Dialog::new("dlg-elsewhere")
                    .open(true)
                    .on_open_change(on_open_change)
                    .title("This session is open somewhere else")
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .text_sm()
                            .child(div().child(where_))
                            .child(grid)
                            .child(div().text_color(c.muted_foreground).child(hint))
                            .child(div().text_color(c.muted_foreground).child(
                                "Opening it here as well would mix both conversations into one transcript. \
                                 You can stop the other one first, or open a copy (a fork).",
                            ))
                            .when(busy && can_stop, |el| {
                                el.child(
                                    div()
                                        .text_color(c.destructive)
                                        .child("It is working right now: stopping it interrupts that."),
                                )
                            })
                            .children(error.clone().map(|e| div().text_color(c.destructive).child(e))),
                    )
                    .footer(
                        div()
                            .flex()
                            .gap_2()
                            .child(Button::new("dlg-else-cancel").outline().label("Cancel").on_click(close))
                            .when(can_stop, |el| {
                                el.child(
                                    Button::new("dlg-else-stop")
                                        .outline()
                                        .label(if stopping { "Stopping…" } else { "Stop It and Open Here" })
                                        .disabled(stopping)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.stop_elsewhere_and_open(window, cx)
                                        })),
                                )
                            })
                            .when_some(shown_in, |el, project| {
                                let id = id.clone();
                                el.child(Button::new("dlg-else-show").outline().label("Show It").on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        this.dialog = None;
                                        let id = id.clone();
                                        cx.defer(move |cx| crate::windows::reveal_session(project, id, cx));
                                        cx.notify();
                                    }),
                                ))
                            })
                            .child(
                                Button::new("dlg-else-fork")
                                    .primary()
                                    .label("Open a Copy (Fork)")
                                    .disabled(stopping)
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.dialog = None;
                                        this.fork_session(&id, window, cx);
                                    })),
                            ),
                    )
                    .into_any_element()
            }
            DialogKind::ProjectSettings { name, error } => {
                let project = self.project(cx);
                let main = project
                    .as_ref()
                    .map(|p| p.main_folder.display().to_string())
                    .unwrap_or_default();
                let extras: Vec<String> = project
                    .map(|p| p.extra_folders.iter().map(|f| f.display().to_string()).collect())
                    .unwrap_or_default();
                Dialog::new("dlg-project")
                    .open(true)
                    .on_open_change(on_open_change)
                    .title("Project Settings")
                    .track_focus(&name.read(cx).focus_handle(cx))
                    .child(div().text_sm().text_color(c.muted_foreground).child("Name"))
                    .child(form(cx).child(Input::new(name)).children(error_line(error)))
                    .child(div().pt_2().text_sm().text_color(c.muted_foreground).child("Main folder"))
                    .child(div().text_sm().truncate().child(main))
                    .child(div().pt_2().text_sm().text_color(c.muted_foreground).child("Extra folders (--add-dir)"))
                    .children(extras.into_iter().enumerate().map(|(i, folder)| {
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(div().flex_1().min_w_0().truncate().text_sm().child(folder))
                            .child(
                                Button::new(SharedString::from(format!("rm-folder-{i}")))
                                    .ghost()
                                    .size(ButtonSize::Sm)
                                    .label("Remove")
                                    .on_click(cx.listener(move |this, _, _, cx| this.remove_extra_folder(i, cx))),
                            )
                    }))
                    .child(
                        div().child(
                            Button::new("add-folder")
                                .outline()
                                .size(ButtonSize::Sm)
                                .label("Add Folder…")
                                .on_click(cx.listener(|this, _, window, cx| this.add_extra_folder(window, cx))),
                        ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child("Folder changes apply to sessions started or resumed afterwards."),
                    )
                    .footer(
                        div()
                            .flex()
                            .gap_2()
                            .child(Button::new("dlg-project-cancel").outline().label("Cancel").on_click(close))
                            .child(
                                Button::new("dlg-project-ok")
                                    .primary()
                                    .label("Save")
                                    .on_click(cx.listener(|this, _, window, cx| this.commit_project_settings(window, cx))),
                            ),
                    )
                    .into_any_element()
            }
            DialogKind::Message { title, body } => Dialog::new("dlg-message")
                .open(true)
                .on_open_change(on_open_change)
                .title(title.clone())
                .description(body.clone())
                .footer(Button::new("dlg-message-ok").primary().label("OK").on_click(close))
                .into_any_element(),
            DialogKind::Prompt { .. } => return None,
        };
        Some(el)
    }
}
