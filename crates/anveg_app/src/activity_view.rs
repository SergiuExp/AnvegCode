//! The details of one Git activity log entry (Git ▸ Activity ▸ an entry's
//! error or "Details"): what ran, where, what it moved, the failure in plain
//! words, and git's complete output as selectable text.

use anveg_core::git_activity::{self, Entry};
use anveg_core::time::{format_utc, now_millis, relative};
use gpui_cn::prelude::*;
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::{
    App, AppContext as _, ClipboardItem, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, Styled as _,
    Window, div, px,
};

use crate::ui::Colors;
use crate::workspace::WorkspaceEvent;

pub struct ActivityView {
    entry: Option<Entry>,
    output: Option<Entity<EditorState>>,
    focus: FocusHandle,
}

impl EventEmitter<WorkspaceEvent> for ActivityView {}

impl Focusable for ActivityView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ActivityView {
    pub fn new(entry: Option<Entry>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let output = entry.as_ref().map(|e| {
            let text = if e.message.is_empty() {
                "(git printed nothing)".to_owned()
            } else {
                // Progress lines end in carriage returns; keep their last state.
                e.message
                    .lines()
                    .map(|l| l.rsplit('\r').next().unwrap_or(l))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let state = cx.new(|cx| {
                EditorState::new(window, cx)
                    .language("plaintext")
                    .line_number(false)
                    .searchable(true)
                    .soft_wrap(true)
                    .default_value(text)
            });
            state.update(cx, |s, cx| s.set_readonly(true, cx));
            state
        });
        Self {
            entry,
            output,
            focus: cx.focus_handle(),
        }
    }

    pub fn title(&self) -> String {
        match &self.entry {
            Some(e) if e.ok => format!("{} details", e.op.label()),
            Some(e) => format!("{} failed", e.op.label()),
            None => "Activity".to_owned(),
        }
    }
}

/// "Sent 2 commits to origin/main" lines for an entry.
fn moved_lines(e: &Entry) -> Vec<String> {
    let count = |r: &git_activity::RefChange| {
        let n = r.commits.len();
        if r.more_commits > 0 {
            format!("{n}+ commits")
        } else if n == 1 {
            "1 commit".to_owned()
        } else {
            format!("{n} commits")
        }
    };
    let mut out = Vec::new();
    for r in &e.sent {
        out.push(format!("Sent {} to {}", count(r), r.name));
    }
    for r in &e.received {
        out.push(format!("Received {} on {}", count(r), r.name));
    }
    if let Some(l) = &e.local {
        out.push(format!("Updated {} with {}", l.name, count(l)));
    }
    out
}

impl Render for ActivityView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let Some(e) = self.entry.clone() else {
            return div()
                .id("activity-view")
                .track_focus(&self.focus)
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(c.muted_foreground)
                .child("This entry is no longer in the activity log.")
                .into_any_element();
        };
        let now = now_millis();
        let row = |label: &'static str, value: String| {
            div()
                .flex()
                .gap_2()
                .text_sm()
                .child(
                    div()
                        .w(px(96.))
                        .flex_shrink_0()
                        .text_color(c.muted_foreground)
                        .child(label),
                )
                .child(div().flex_1().min_w_0().child(value))
        };
        let command = if e.command.is_empty() {
            format!("git {}", e.op.label().to_lowercase())
        } else {
            e.command.clone()
        };
        let explanation = (!e.ok).then(|| git_activity::explain(&e.message, e.op));
        let moved = moved_lines(&e);
        // Run against this entry's repository (a project can have several).
        let args: Vec<String> = match shell_words::split(&command) {
            Ok(words) if words.first().map(String::as_str) == Some("git") => {
                let mut a = vec![
                    "git".to_owned(),
                    "-C".to_owned(),
                    e.repo.display().to_string(),
                ];
                a.extend(words.into_iter().skip(1));
                a
            }
            _ => Vec::new(),
        };
        let output_text = e.message.clone();
        let mut head = div()
            .flex()
            .flex_col()
            .gap_1()
            .p_4()
            .flex_shrink_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().text_lg().child(if e.ok {
                        format!("{} succeeded", e.op.label())
                    } else {
                        format!("{} failed", e.op.label())
                    }))
                    .child(div().flex_1())
                    .when(!args.is_empty(), |el| {
                        el.child(
                            Button::new("act-run")
                                .outline()
                                .size(ButtonSize::Sm)
                                .label("Run in Terminal")
                                .tooltip("Run the same command in a terminal tab (e.g. to sign in)")
                                .on_click(cx.listener(move |_, _, _, cx| {
                                    cx.emit(WorkspaceEvent::RunInTerminal(args.clone()))
                                })),
                        )
                    })
                    .child(
                        Button::new("act-copy")
                            .outline()
                            .size(ButtonSize::Sm)
                            .label("Copy Output")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(output_text.clone()))
                            }),
                    ),
            );
        if let Some(x) = explanation {
            head = head.child(
                div()
                    .mt_2()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(c.destructive.opacity(0.4))
                    .bg(c.destructive.opacity(0.08))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().text_color(c.destructive).child(x.title))
                    .children(x.hint.map(|h| div().text_sm().child(h))),
            );
        }
        head = head
            .child(div().h(px(6.)))
            .child(row("Command", command))
            .child(row("Repository", e.repo.display().to_string()))
            .child(row("Branch", e.branch.clone().unwrap_or_else(|| "(detached)".into())))
            .child(row(
                "When",
                format!("{} ({})", format_utc(e.time), relative(e.time, now)),
            ))
            .child(row(
                "Result",
                if moved.is_empty() {
                    if e.ok {
                        "Nothing moved: already up to date.".to_owned()
                    } else {
                        "Nothing was sent or received.".to_owned()
                    }
                } else {
                    moved.join("; ")
                },
            ));
        div()
            .id("activity-view")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .child(head)
            .child(
                div()
                    .px_4()
                    .pb_1()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child("Git's output"),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .mx_4()
                    .mb_4()
                    .border_1()
                    .border_color(c.border)
                    .rounded_md()
                    .overflow_hidden()
                    .children(self.output.as_ref().map(|state| {
                        Editor::new(state).bordered(false).readonly(true).h_full()
                    })),
            )
            .into_any_element()
    }
}
