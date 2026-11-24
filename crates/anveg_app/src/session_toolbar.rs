//! The bar above a Claude session: model, effort, how full the context is
//! next to Compact, Stop, Rewind, Clear, a menu of less used commands, and
//! the user's own buttons. Every button types a slash command (or the
//! button's text) into the session and presses Enter, as if typed.
//!
//! What is usable follows the session's status: everything while Claude
//! waits for the next prompt; only Stop, /btw and buttons marked “Usable
//! while Claude is working” during a turn; only Stop while a permission or
//! question dialog is open (typing would answer it).

use std::time::Duration;

use anveg_core::config::{DocKey, SessionButton};
use anveg_core::status::{self, ContextUse, SessionStatus};
use gpui_cn::prelude::*;
use gpui_cn::{DropdownMenu, MenuEntry, MenuItem as CnMenuItem};
use gpui_kit::assets::IconName;
use gpui_kit::{
    AnyElement, App, Context, Focusable as _, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    hsla, px,
};

use crate::model::model;
use crate::project_window::ProjectWindow;
use crate::ui::{Colors, LinesTooltip};
use crate::workspace::{ButtonStyle, Prompt};

/// What the session can take right now.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ready {
    /// Waiting for the next prompt: everything works.
    Idle,
    /// Working on a turn: Stop, /btw and “while working” buttons.
    Working,
    /// A dialog is open: only Stop.
    Dialog,
    /// Not running.
    Off,
}

/// The less used commands in the “⋯” menu: (command, label).
const MORE: &[(&str, &str)] = &[
    ("/context", "Context Usage"),
    ("/usage", "Plan Usage"),
    ("/status", "Status"),
    ("/memory", "Memory (CLAUDE.md)"),
    ("/init", "Create CLAUDE.md"),
    ("/recap", "Recap"),
    ("/export", "Export Conversation"),
    ("/plan", "Plan Mode"),
];

/// The context meter turns amber, then red.
const CONTEXT_WARN: u8 = 70;
const CONTEXT_HIGH: u8 = 85;

impl ProjectWindow {
    fn ready(&self, id: &str) -> Ready {
        match self.running.get(id) {
            Some(r) if r.is_live() => match r.status {
                SessionStatus::Idle => Ready::Idle,
                SessionStatus::Working => Ready::Working,
                SessionStatus::NeedsInput => Ready::Dialog,
                SessionStatus::Stopped => Ready::Off,
            },
            _ => Ready::Off,
        }
    }

    /// Type `text` into session `id` and press Enter: typed when it is one
    /// line, pasted when it has several (so its newlines do not send it
    /// early).
    pub fn send_to_session(&mut self, id: &str, text: &str, cx: &mut Context<Self>) {
        let Some(running) = self.running.get(id).filter(|r| r.is_live()) else {
            return;
        };
        let view = running.view.clone();
        let text = text.to_owned();
        view.update(cx, |v, cx| {
            if text.contains('\n') {
                v.paste_text(text, cx)
            } else {
                v.send_text(text, cx)
            }
        });
        // Claude Code reads a quick Enter right after the text as part of a
        // paste; wait a moment, as for /rename.
        cx.spawn(async move |_, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(150))
                .await;
            view.update(cx, |v, cx| v.send_text("\r", cx));
        })
        .detach();
    }

    /// Send, then put the keyboard in the session (for pickers such as
    /// /model).
    fn send_and_focus(&mut self, id: &str, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.send_to_session(id, text, cx);
        if let Some(r) = self.running.get(id) {
            let handle = r.view.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    /// Esc: interrupts a turn, or dismisses a dialog.
    fn interrupt_session(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(r) = self.running.get(id).filter(|r| r.is_live()) {
            let view = r.view.clone();
            view.update(cx, |v, cx| v.send_text("\u{1b}", cx));
            let handle = view.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
    }

    fn ask_clear(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let id = id.to_owned();
        let prompt = Prompt::new(
            "Clear this conversation?",
            "Claude starts over with an empty context (/clear). The conversation so far stays in \
             its transcript and in the session list, so it can still be resumed.",
        )
        .cancel()
        .button("Clear", ButtonStyle::Destructive, move |_, _, cx| {
            let _ = weak.update(cx, |this, cx| this.send_to_session(&id, "/clear", cx));
            Ok(())
        });
        self.show_prompt(prompt, window, cx);
    }

    fn ask_btw(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let id = id.to_owned();
        let prompt = Prompt::new(
            "Ask a side question",
            "Claude answers without adding it to the conversation (/btw). It works while Claude \
             is busy, too.",
        )
        .input("Your question", "")
        .cancel()
        .button("Ask", ButtonStyle::Primary, move |value, _, cx| {
            let question = value.unwrap_or_default().trim().replace('\n', " ");
            if question.is_empty() {
                return Err("Type a question.".into());
            }
            let _ = weak.update(cx, |this, cx| {
                this.send_to_session(&id, &format!("/btw {question}"), cx)
            });
            Ok(())
        });
        self.show_prompt(prompt, window, cx);
    }

    /// The “⋯” menu's rows, built as it opens so their state is current.
    fn more_menu(&self, id: &str, this: &gpui_kit::Entity<ProjectWindow>) -> Vec<MenuEntry> {
        let ready = self.ready(id);
        let mut entries: Vec<MenuEntry> = MORE
            .iter()
            .map(|&(command, label)| {
                let this = this.downgrade();
                let id = id.to_owned();
                MenuEntry::Item(
                    CnMenuItem::new(command, format!("{label}   {command}"))
                        .disabled(ready != Ready::Idle)
                        .on_select(move |window, cx| {
                            let _ = this.update(cx, |this, cx| {
                                this.send_and_focus(&id, command, window, cx)
                            });
                        }),
                )
            })
            .collect();
        entries.push(MenuEntry::Separator);
        let (weak, id2) = (this.downgrade(), id.to_owned());
        entries.push(MenuEntry::Item(
            CnMenuItem::new("btw", "Side Question…   /btw")
                .disabled(!matches!(ready, Ready::Idle | Ready::Working))
                .on_select(move |window, cx| {
                    let _ = weak.update(cx, |this, cx| this.ask_btw(&id2, window, cx));
                }),
        ));
        entries.push(MenuEntry::Separator);
        let weak = this.downgrade();
        entries.push(MenuEntry::Item(
            CnMenuItem::new("edit-buttons", "Edit Session Buttons…").on_select(move |window, cx| {
                let _ = weak.update(cx, |this, cx| this.open_doc(DocKey::SessionButtons, window, cx));
            }),
        ));
        entries
    }

    /// The model, as the status line names it, else from the transcript.
    fn session_model(&self, id: &str) -> Option<String> {
        let line = self.running.get(id).and_then(|r| r.line.as_ref());
        line.and_then(|l| l.model_name.clone())
            .or_else(|| {
                self.session(id)
                    .and_then(|s| s.model.as_deref())
                    .map(|m| m.strip_prefix("claude-").unwrap_or(m).to_owned())
            })
    }

    fn session_context(&self, id: &str) -> Option<ContextUse> {
        let line = self.running.get(id).and_then(|r| r.line.as_ref());
        status::context_use(line, self.session(id).and_then(|s| s.context_tokens))
    }

    /// The bar above session `id`'s terminal (`split` for the right pane).
    pub fn render_session_toolbar(&self, id: &str, split: bool, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let ready = self.ready(id);
        let idle = ready == Ready::Idle;
        let line = self.running.get(id).and_then(|r| r.line.clone());
        let pane = if split { "s" } else { "m" };
        let key = |name: &str| SharedString::from(format!("st-{pane}-{name}"));
        let small = |name: &str| Button::new(key(name)).ghost().size(ButtonSize::Sm);


        let model_label = self.session_model(id).unwrap_or_else(|| "Model".into());
        let effort_label = line
            .as_ref()
            .and_then(|l| l.effort.clone())
            .map(|e| format!("Effort: {e}"))
            .unwrap_or_else(|| "Effort".into());

        let mut row = div()
            .id(key("bar"))
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap_0p5()
            .h(px(32.))
            .px_1p5()
            .border_b_1()
            .border_color(c.border)
            .overflow_x_hidden()
            .child(
                small("model")
                    .icon(Icon::from(IconName::Cpu))
                    .label(model_label)
                    .tooltip("Choose the model (/model)")
                    .disabled(!idle)
                    .on_click(send_listener(id, "/model", cx)),
            )
            .child(
                small("effort")
                    .icon(Icon::from(IconName::Brain))
                    .label(effort_label)
                    .tooltip("Choose how hard Claude thinks (/effort)")
                    .disabled(!idle)
                    .on_click(send_listener(id, "/effort", cx)),
            )
            .child(separator(c.border))
            .child(self.context_meter(id, pane, ready, cx))
            .child(
                small("compact")
                    .icon(Icon::from(IconName::FoldVertical))
                    .label("Compact")
                    .tooltip("Summarise the conversation to free context (/compact)")
                    .disabled(!idle)
                    .on_click(send_listener(id, "/compact", cx)),
            )
            .child(separator(c.border));

        let id_stop = id.to_owned();
        row = row.child(
            small("stop")
                .icon(Icon::from(IconName::CircleStop))
                .tooltip(if ready == Ready::Dialog {
                    "Dismiss the dialog (Esc)"
                } else {
                    "Stop Claude (Esc)"
                })
                .disabled(!matches!(ready, Ready::Working | Ready::Dialog))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.interrupt_session(&id_stop, window, cx)
                })),
        );
        row = row.child(
            small("rewind")
                .icon(Icon::from(IconName::Undo2))
                .tooltip("Go back to an earlier message (/rewind)")
                .disabled(!idle)
                .on_click(send_listener(id, "/rewind", cx)),
        );
        let id_clear = id.to_owned();
        row = row.child(
            small("clear")
                .icon(Icon::from(IconName::Eraser))
                .tooltip("Clear the conversation (/clear)…")
                .disabled(!idle)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.ask_clear(&id_clear, window, cx)
                })),
        );

        let weak = cx.entity().downgrade();
        let id_menu = id.to_owned();
        let menu = &self.session_menus[usize::from(split)];
        row = row.child(
            DropdownMenu::new(key("more"), menu)
                .trigger(
                    Button::new(key("more-trigger"))
                        .ghost()
                        .size(ButtonSize::Sm)
                        .icon(Icon::from(IconName::Ellipsis))
                        .tooltip("More commands")
                        .disabled(ready == Ready::Off),
                )
                .items(move |_, cx| {
                    let Some(this) = weak.upgrade() else {
                        return Vec::new();
                    };
                    this.read(cx).more_menu(&id_menu, &this)
                }),
        );

        let buttons: Vec<SessionButton> = model(cx).read(cx).settings.session_buttons.clone();
        let buttons: Vec<SessionButton> = buttons
            .into_iter()
            .filter(|b| !b.text.trim().is_empty())
            .collect();
        if !buttons.is_empty() {
            row = row.child(separator(c.border));
        }
        for b in buttons {
            let usable = idle || (ready == Ready::Working && b.while_working);
            let label = if b.label.trim().is_empty() {
                b.text.clone()
            } else {
                b.label.clone()
            };
            let (id, text) = (id.to_owned(), b.text.clone());
            row = row.child(
                Button::new(SharedString::from(format!("st-{pane}-btn-{}", b.id)))
                    .ghost()
                    .size(ButtonSize::Sm)
                    .label(label)
                    .tooltip(b.text.clone())
                    .disabled(!usable)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.send_and_focus(&id, &text, window, cx)
                    })),
            );
        }

        row.child(div().flex_1())
            .child(
                small("edit")
                    .icon(Icon::from(IconName::Settings2))
                    .tooltip("Edit Session Buttons…")
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_doc(DocKey::SessionButtons, window, cx)
                    })),
            )
            .into_any_element()
    }

    /// “▮▮▯ 42%”, with the token counts in its tooltip.
    fn context_meter(&self, id: &str, pane: &str, ready: Ready, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let usage = self.session_context(id);
        let percent = usage.as_ref().map_or(0, |u| u.percent);
        let fill = meter_color(percent, &c);
        let text = match &usage {
            Some(u) if u.estimated => format!("≈{}%", u.percent),
            Some(u) => format!("{}%", u.percent),
            None => "–".to_owned(),
        };
        let tip = context_tooltip(usage.as_ref(), cx);
        let width = 44.;
        div()
            .id(SharedString::from(format!("st-{pane}-context")))
            .flex()
            .items_center()
            .gap_1p5()
            .px_1()
            .text_xs()
            .text_color(if ready == Ready::Off {
                c.muted_foreground
            } else {
                c.foreground
            })
            .child(
                div()
                    .w(px(width))
                    .h(px(6.))
                    .rounded_full()
                    .bg(c.border)
                    .overflow_hidden()
                    .child(
                        div()
                            .h_full()
                            .w(px(width * f32::from(percent) / 100.))
                            .rounded_full()
                            .bg(fill),
                    ),
            )
            .child(div().min_w(px(30.)).child(text))
            .tooltip(move |_, cx| LinesTooltip::build(tip.clone(), cx))
            .into_any_element()
    }
}

/// A click handler that sends `command` to session `id`.
fn send_listener(
    id: &str,
    command: &'static str,
    cx: &mut Context<ProjectWindow>,
) -> impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut App) + 'static {
    let id = id.to_owned();
    cx.listener(move |this, _, window, cx| this.send_and_focus(&id, command, window, cx))
}

fn separator(color: Hsla) -> impl IntoElement {
    div().w(px(1.)).h(px(16.)).mx_1().bg(color)
}

fn meter_color(percent: u8, c: &Colors) -> Hsla {
    if percent >= CONTEXT_HIGH {
        c.destructive
    } else if percent >= CONTEXT_WARN {
        hsla(38. / 360., 0.92, 0.50, 1.0)
    } else {
        c.primary
    }
}

fn context_tooltip(usage: Option<&ContextUse>, cx: &App) -> Vec<String> {
    let on = model(cx).read(cx).settings.session_status_line;
    let Some(u) = usage else {
        return vec![
            "Context: not known yet".into(),
            "It shows after Claude's first reply.".into(),
        ];
    };
    let mut lines = vec![format!("Context: {}% used", u.percent)];
    if let (Some(tokens), Some(window)) = (u.tokens, u.window) {
        lines.push(format!("{} of {} tokens", thousands(tokens), thousands(window)));
    }
    if u.estimated {
        lines.push("Estimated from the transcript.".into());
        if !on {
            lines.push("For exact numbers, turn on the status line in Settings.".into());
        }
    }
    lines.push("Compact frees space by summarising the conversation.".into());
    lines
}

fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::thousands;

    #[test]
    fn thousands_groups_digits() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000), "1,000");
        assert_eq!(thousands(200_000), "200,000");
        assert_eq!(thousands(1_234_567), "1,234,567");
    }
}
