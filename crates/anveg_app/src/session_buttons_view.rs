//! The Session Buttons tab: your own buttons for the bar above Claude
//! sessions. Each one types its text into the session and presses Enter.
//! They are app-wide (saved in settings.json in the app's data folder) and
//! saved as you go (text fields on Enter or when they lose focus).

use anveg_core::config::SessionButton;
use gpui_cn::prelude::*;
use gpui_cn::{Input, InputEvent, InputState, Switch};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};

use crate::model::{ModelEvent, model};
use crate::ui::Colors;
use crate::workspace::{ButtonStyle, Prompt, WorkspaceEvent};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Label,
    Text,
}

struct Row {
    id: String,
    label: Entity<InputState>,
    text: Entity<InputState>,
    /// A save waiting for typing to stop, per field.
    saves: [Option<Task<()>>; 2],
    _subs: Vec<Subscription>,
}

pub struct SessionButtonsView {
    rows: Vec<Row>,
    /// A row to focus once it exists.
    focus_row: Option<String>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<WorkspaceEvent> for SessionButtonsView {}

impl Focusable for SessionButtonsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SessionButtonsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let m = model(cx);
        let subs = vec![cx.subscribe_in(&m, window, |this, _, event, window, cx| {
            if matches!(event, ModelEvent::SettingsChanged) {
                this.sync_rows(window, cx);
            }
        })];
        let mut this = Self {
            rows: Vec::new(),
            focus_row: None,
            focus: cx.focus_handle(),
            _subs: subs,
        };
        this.sync_rows(window, cx);
        this
    }

    fn buttons(&self, cx: &App) -> Vec<SessionButton> {
        model(cx).read(cx).settings.session_buttons.clone()
    }

    /// Keep one row per button, in order, without touching fields that
    /// are being edited.
    fn sync_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let buttons = self.buttons(cx);
        let mut rows = Vec::new();
        for button in &buttons {
            match self.rows.iter().position(|r| r.id == button.id) {
                Some(i) => rows.push(self.rows.remove(i)),
                None => rows.push(self.make_row(button, window, cx)),
            }
        }
        self.rows = rows;
        if let Some(id) = self.focus_row.take()
            && let Some(row) = self.rows.iter().find(|r| r.id == id)
        {
            let handle = row.label.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    fn make_row(&mut self, b: &SessionButton, window: &mut Window, cx: &mut Context<Self>) -> Row {
        let input = |value: &str, placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            let value = value.to_owned();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let label = input(&b.label, "Label, e.g. Review", window, cx);
        let text = input(&b.text, "Text to send, e.g. /review or “Run the tests”", window, cx);
        let mut subs = Vec::new();
        for (state, field) in [(&label, Field::Label), (&text, Field::Text)] {
            let id = b.id.clone();
            subs.push(cx.subscribe(state, move |this, _, event: &InputEvent, cx| match event {
                InputEvent::PressEnter { .. } | InputEvent::Blur => {
                    this.cancel_save(&id, field);
                    this.apply(&id, field, cx);
                }
                // Saved a moment after typing stops (the field may never
                // report losing focus).
                InputEvent::Change => this.schedule_save(&id, field, cx),
                _ => {}
            }));
        }
        Row {
            id: b.id.clone(),
            label,
            text,
            saves: Default::default(),
            _subs: subs,
        }
    }

    fn update(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut Vec<SessionButton>)) {
        model(cx).update(cx, |m, cx| m.update_settings(cx, |s| f(&mut s.session_buttons)));
    }

    fn cancel_save(&mut self, id: &str, field: Field) {
        if let Some(row) = self.rows.iter_mut().find(|r| r.id == id) {
            row.saves[field as usize] = None;
        }
    }

    fn schedule_save(&mut self, id: &str, field: Field, cx: &mut Context<Self>) {
        let key = id.to_owned();
        let task = cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(600))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.cancel_save(&key, field);
                this.apply(&key, field, cx);
            });
        });
        if let Some(row) = self.rows.iter_mut().find(|r| r.id == id) {
            row.saves[field as usize] = Some(task);
        }
    }

    fn apply(&mut self, id: &str, field: Field, cx: &mut Context<Self>) {
        let Some(row) = self.rows.iter().find(|r| r.id == id) else {
            return;
        };
        let value = match field {
            Field::Label => row.label.read(cx).value().trim().to_owned(),
            Field::Text => row.text.read(cx).value().trim().to_owned(),
        };
        let unchanged = self.buttons(cx).iter().any(|b| {
            b.id == id
                && match field {
                    Field::Label => b.label == value,
                    Field::Text => b.text == value,
                }
        });
        if unchanged {
            return;
        }
        let id = id.to_owned();
        self.update(cx, |buttons| {
            if let Some(b) = buttons.iter_mut().find(|b| b.id == id) {
                match field {
                    Field::Label => b.label = value,
                    Field::Text => b.text = value,
                }
            }
        });
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        let button = SessionButton::new("", "");
        self.focus_row = Some(button.id.clone());
        self.update(cx, |buttons| buttons.push(button));
    }

    fn set_while_working(&mut self, id: &str, value: bool, cx: &mut Context<Self>) {
        let id = id.to_owned();
        self.update(cx, |buttons| {
            if let Some(b) = buttons.iter_mut().find(|b| b.id == id) {
                b.while_working = value;
            }
        });
    }

    fn shift(&mut self, id: &str, up: bool, cx: &mut Context<Self>) {
        let id = id.to_owned();
        self.update(cx, |buttons| {
            let Some(i) = buttons.iter().position(|b| b.id == id) else {
                return;
            };
            let j = if up { i.checked_sub(1) } else { Some(i + 1) };
            if let Some(j) = j.filter(|&j| j < buttons.len()) {
                buttons.swap(i, j);
            }
        });
    }

    fn delete(&mut self, id: &str, cx: &mut Context<Self>) {
        let label = self
            .buttons(cx)
            .into_iter()
            .find(|b| b.id == id)
            .map(|b| b.label)
            .unwrap_or_default();
        let weak = cx.entity().downgrade();
        let id = id.to_owned();
        let prompt = Prompt::new(
            format!(
                "Delete the button “{}”?",
                if label.is_empty() { "Untitled" } else { &label }
            ),
            "It goes away from the bar above every session.",
        )
        .cancel()
        .button("Delete", ButtonStyle::Destructive, move |_, _, cx| {
            let _ = weak.update(cx, |this, cx| {
                let id = id.clone();
                this.update(cx, |buttons| buttons.retain(|b| b.id != id));
            });
            Ok(())
        });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn render_row(&self, i: usize, row: &Row, button: &SessionButton, count: usize, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let id = row.id.clone();
        let (up, down, del, ww) = (id.clone(), id.clone(), id.clone(), id.clone());
        let label = |text: &'static str| {
            div()
                .w(px(80.))
                .flex_shrink_0()
                .text_xs()
                .text_color(c.muted_foreground)
                .child(text)
        };
        div()
            .id(SharedString::from(format!("sb-{id}")))
            .flex()
            .flex_col()
            .gap_1p5()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(c.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(label("Label"))
                    .child(div().w(px(220.)).child(Input::new(&row.label))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(label("Sends"))
                    .child(div().flex_1().child(Input::new(&row.text))),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        Switch::new(SharedString::from(format!("sb-ww-{id}")))
                            .checked(button.while_working)
                            .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                this.set_while_working(&ww, *v, cx)
                            })),
                    )
                    .child(div().text_sm().child("Usable while Claude is working"))
                    .child(div().flex_1())
                    .child(
                        Button::new(SharedString::from(format!("sb-up-{id}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .label("↑")
                            .tooltip("Move left in the bar")
                            .disabled(i == 0)
                            .on_click(cx.listener(move |this, _, _, cx| this.shift(&up, true, cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("sb-down-{id}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .label("↓")
                            .tooltip("Move right in the bar")
                            .disabled(i + 1 == count)
                            .on_click(cx.listener(move |this, _, _, cx| this.shift(&down, false, cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("sb-del-{id}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .icon(Icon::from(gpui_kit::assets::IconName::Delete))
                            .tooltip("Delete Button…")
                            .on_click(cx.listener(move |this, _, _, cx| this.delete(&del, cx))),
                    ),
            )
            .into_any_element()
    }
}

impl Render for SessionButtonsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let buttons = self.buttons(cx);
        let count = self.rows.len();
        let rows: Vec<AnyElement> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| {
                let button = buttons.iter().find(|b| b.id == row.id)?;
                Some(self.render_row(i, row, button, count, cx))
            })
            .collect();
        div()
            .id("session-buttons-view")
            .track_focus(&self.focus)
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .max_w(px(820.))
            .child(div().text_lg().child("Session Buttons"))
            .child(
                div()
                    .text_sm()
                    .text_color(c.muted_foreground)
                    .child(
                        "Your own buttons in the bar above every Claude session. A button types its text \
                         into the session and presses Enter: a slash command (/review, /security-review, \
                         a command of your own) or a plain message. Buttons work while the session waits \
                         for you; turn on “Usable while Claude is working” for text that may be queued \
                         during a turn. Shared by all projects.",
                    ),
            )
            .children(rows)
            .child(
                div().child(
                    Button::new("sb-add")
                        .outline()
                        .icon(Icon::from(gpui_kit::assets::IconName::Plus))
                        .label("Add Button")
                        .on_click(cx.listener(|this, _, _, cx| this.add(cx))),
                ),
            )
    }
}
