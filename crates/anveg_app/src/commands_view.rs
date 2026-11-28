//! The Toolbar Commands tab: add, edit, reorder, hide and delete the
//! project's toolbar commands. Changes are saved as you go (text fields on
//! Enter or when they lose focus).

use anveg_core::command_icons::{self, ICONS, NO_ICON};
use anveg_core::config::ProjectCommand;
use gpui_cn::prelude::*;
use gpui_cn::{Input, InputEvent, InputState, Popover, Switch};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, WeakEntity, Window, div,
    px,
};
use uuid::Uuid;

use crate::model::{ModelEvent, model};
use crate::project_commands;
use crate::ui::Colors;
use crate::workspace::{ButtonStyle, Prompt, WorkspaceEvent};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Name,
    Command,
    Shortcut,
}

struct Row {
    id: String,
    name: Entity<InputState>,
    command: Entity<InputState>,
    shortcut: Entity<InputState>,
    error: Option<SharedString>,
    /// A save waiting for typing to stop, per field.
    saves: [Option<Task<()>>; 3],
    _subs: Vec<Subscription>,
}

pub struct CommandsView {
    project: Uuid,
    rows: Vec<Row>,
    /// A row to focus once it exists.
    focus_row: Option<String>,
    /// The command whose icon picker is open.
    icon_picker: Option<String>,
    focus: FocusHandle,
    _subs: Vec<Subscription>,
}

impl EventEmitter<WorkspaceEvent> for CommandsView {}

impl Focusable for CommandsView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl CommandsView {
    pub fn new(project: Uuid, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let m = model(cx);
        let subs = vec![cx.subscribe_in(&m, window, |this, _, event, window, cx| {
            if matches!(event, ModelEvent::ProjectsChanged) {
                this.sync_rows(window, cx);
            }
        })];
        let mut this = Self {
            project,
            rows: Vec::new(),
            focus_row: None,
            icon_picker: None,
            focus: cx.focus_handle(),
            _subs: subs,
        };
        this.sync_rows(window, cx);
        this
    }

    fn commands(&self, cx: &App) -> Vec<ProjectCommand> {
        model(cx)
            .read(cx)
            .projects
            .get(self.project)
            .map(|p| p.commands.clone())
            .unwrap_or_default()
    }

    /// Keep one row per command, in order, without touching fields that
    /// are being edited.
    fn sync_rows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let commands = self.commands(cx);
        let mut rows = Vec::new();
        for command in &commands {
            match self.rows.iter().position(|r| r.id == command.id) {
                Some(i) => rows.push(self.rows.remove(i)),
                None => rows.push(self.make_row(command, window, cx)),
            }
        }
        self.rows = rows;
        if let Some(id) = self.focus_row.take()
            && let Some(row) = self.rows.iter().find(|r| r.id == id)
        {
            let handle = row.name.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    fn make_row(&mut self, c: &ProjectCommand, window: &mut Window, cx: &mut Context<Self>) -> Row {
        let input = |value: &str, placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>| {
            let value = value.to_owned();
            cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            })
        };
        let name = input(&c.name, "Name, e.g. Build", window, cx);
        let command = input(&c.command, "Command, e.g. cargo build", window, cx);
        let shortcut = input(&c.shortcut, "Shortcut, e.g. cmd-shift-b", window, cx);
        let mut subs = Vec::new();
        for (state, field) in [
            (&name, Field::Name),
            (&command, Field::Command),
            (&shortcut, Field::Shortcut),
        ] {
            let id = c.id.clone();
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
            id: c.id.clone(),
            name,
            command,
            shortcut,
            error: None,
            saves: Default::default(),
            _subs: subs,
        }
    }

    fn update(&self, cx: &mut Context<Self>, f: impl FnOnce(&mut Vec<ProjectCommand>)) {
        let project = self.project;
        model(cx).update(cx, |m, cx| {
            m.update_projects(cx, |p| {
                if let Some(p) = p.get_mut(project) {
                    f(&mut p.commands);
                }
            })
        });
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
            Field::Name => row.name.read(cx).value().trim().to_owned(),
            Field::Command => row.command.read(cx).value().trim().to_owned(),
            Field::Shortcut => row.shortcut.read(cx).value().to_string(),
        };
        let mut error = None;
        let value = if field == Field::Shortcut {
            match project_commands::validate(&value) {
                Ok(s) => {
                    let taken = !s.is_empty()
                        && self
                            .commands(cx)
                            .iter()
                            .any(|c| c.id != id && c.shortcut == s);
                    if taken {
                        error = Some(format!("{s} is used by another command."));
                        None
                    } else {
                        Some(s)
                    }
                }
                Err(e) => {
                    error = Some(e);
                    None
                }
            }
        } else {
            Some(value)
        };
        if let Some(row) = self.rows.iter_mut().find(|r| r.id == id) {
            row.error = error.map(Into::into);
        }
        if let Some(value) = value {
            let id = id.to_owned();
            self.update(cx, |commands| {
                if let Some(c) = commands.iter_mut().find(|c| c.id == id) {
                    match field {
                        Field::Name => c.name = value,
                        Field::Command => c.command = value,
                        Field::Shortcut => c.shortcut = value,
                    }
                }
            });
        }
        cx.notify();
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        let command = ProjectCommand::new("", "");
        self.focus_row = Some(command.id.clone());
        self.update(cx, |commands| commands.push(command));
    }

    fn set_flag(&mut self, id: &str, toolbar: Option<bool>, new_terminal: Option<bool>, cx: &mut Context<Self>) {
        let id = id.to_owned();
        self.update(cx, |commands| {
            if let Some(c) = commands.iter_mut().find(|c| c.id == id) {
                if let Some(v) = toolbar {
                    c.show_in_toolbar = v;
                }
                if let Some(v) = new_terminal {
                    c.new_terminal = v;
                }
            }
        });
    }

    /// `None`: Automatic; `Some("none")`: no icon.
    fn set_icon(&mut self, id: &str, icon: Option<String>, cx: &mut Context<Self>) {
        let id = id.to_owned();
        self.icon_picker = None;
        self.update(cx, |commands| {
            if let Some(c) = commands.iter_mut().find(|c| c.id == id) {
                c.icon = icon;
            }
        });
    }

    /// The Icon row: a button showing the icon, which opens the picker.
    fn icon_control(&self, command: &ProjectCommand, cx: &mut Context<Self>) -> AnyElement {
        let id = command.id.clone();
        let guess = command_icons::guess(&command.name, &command.command);
        let (icon, text) = match command.icon.as_deref() {
            None => (
                Some(guess),
                format!("Automatic ({})", command_icons::label(guess).unwrap_or("Run")),
            ),
            Some(NO_ICON) => (None, "No icon".to_owned()),
            Some(key) => {
                let shown = command_icons::resolve(Some(key), "", "");
                (shown, shown.and_then(command_icons::label).unwrap_or("Run").to_owned())
            }
        };
        let mut trigger = Button::new(SharedString::from(format!("icon-{id}")))
            .outline()
            .size(ButtonSize::Sm)
            .label(text)
            .trailing_icon(Icon::from(gpui_kit::assets::IconName::ChevronDown));
        if let Some(key) = icon {
            trigger = trigger.icon(Icon::from(project_commands::icon_image(key)));
        }
        let weak = cx.entity().downgrade();
        let weak_content = weak.clone();
        let open_id = id.clone();
        let current = command.icon.clone();
        Popover::new(SharedString::from(format!("icon-picker-{id}")))
            .open(self.icon_picker.as_deref() == Some(id.as_str()))
            .on_open_change(move |open, _, cx| {
                let id = open_id.clone();
                let _ = weak.update(cx, |this, cx| {
                    this.icon_picker = open.then_some(id);
                    cx.notify();
                });
            })
            .trigger(trigger)
            .content(move |_, cx| icon_picker(&id, current.as_deref(), guess, weak_content, cx))
            .into_any_element()
    }

    fn shift(&mut self, id: &str, up: bool, cx: &mut Context<Self>) {
        let id = id.to_owned();
        self.update(cx, |commands| {
            let Some(i) = commands.iter().position(|c| c.id == id) else {
                return;
            };
            let j = if up { i.checked_sub(1) } else { Some(i + 1) };
            if let Some(j) = j.filter(|&j| j < commands.len()) {
                commands.swap(i, j);
            }
        });
    }

    fn delete(&mut self, id: &str, cx: &mut Context<Self>) {
        let name = self
            .commands(cx)
            .into_iter()
            .find(|c| c.id == id)
            .map(|c| c.name)
            .unwrap_or_default();
        let weak = cx.entity().downgrade();
        let id = id.to_owned();
        let prompt = Prompt::new(
            format!(
                "Delete the command “{}”?",
                if name.is_empty() { "Untitled" } else { &name }
            ),
            "Its button and shortcut go away. Its terminal tab, if open, stays.",
        )
        .cancel()
        .button("Delete", ButtonStyle::Destructive, move |_, _, cx| {
            let _ = weak.update(cx, |this, cx| {
                let id = id.clone();
                this.update(cx, |commands| commands.retain(|c| c.id != id));
            });
            Ok(())
        });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn render_row(&self, i: usize, row: &Row, command: &ProjectCommand, count: usize, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let id = row.id.clone();
        let (run, up, down, del, tb, nt) = (
            id.clone(),
            id.clone(),
            id.clone(),
            id.clone(),
            id.clone(),
            id.clone(),
        );
        let label = |text: &'static str| {
            div()
                .w(px(80.))
                .flex_shrink_0()
                .text_xs()
                .text_color(c.muted_foreground)
                .child(text)
        };
        div()
            .id(SharedString::from(format!("cmd-{id}")))
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
                    .child(label("Name"))
                    .child(div().flex_1().child(Input::new(&row.name))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(label("Command"))
                    .child(div().flex_1().child(Input::new(&row.command))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(label("Shortcut"))
                    .child(div().w(px(220.)).child(Input::new(&row.shortcut))),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(label("Icon"))
                    .child(self.icon_control(command, cx)),
            )
            .children(
                row.error
                    .clone()
                    .map(|e| div().text_xs().text_color(c.destructive).child(e)),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .child(
                        Switch::new(SharedString::from(format!("tb-{id}")))
                            .checked(command.show_in_toolbar)
                            .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                this.set_flag(&tb, Some(*v), None, cx)
                            })),
                    )
                    .child(div().text_sm().child("Show in toolbar"))
                    .child(div().w(px(12.)))
                    .child(
                        Switch::new(SharedString::from(format!("nt-{id}")))
                            .checked(command.new_terminal)
                            .on_change(cx.listener(move |this, v: &bool, _, cx| {
                                this.set_flag(&nt, None, Some(*v), cx)
                            })),
                    )
                    .child(div().text_sm().child("New terminal each time"))
                    .child(div().flex_1())
                    .child(
                        Button::new(SharedString::from(format!("run-{id}")))
                            .outline()
                            .size(ButtonSize::Sm)
                            .label("Run")
                            .disabled(command.command.trim().is_empty())
                            .on_click(cx.listener(move |_, _, _, cx| {
                                cx.emit(WorkspaceEvent::RunCommand(run.clone()))
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("up-{id}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .label("↑")
                            .tooltip("Move left in the toolbar")
                            .disabled(i == 0)
                            .on_click(cx.listener(move |this, _, _, cx| this.shift(&up, true, cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("down-{id}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .label("↓")
                            .tooltip("Move right in the toolbar")
                            .disabled(i + 1 == count)
                            .on_click(cx.listener(move |this, _, _, cx| this.shift(&down, false, cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("del-{id}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .icon(Icon::from(gpui_kit::assets::IconName::Delete))
                            .tooltip("Delete Command…")
                            .on_click(cx.listener(move |this, _, _, cx| this.delete(&del, cx))),
                    ),
            )
            .into_any_element()
    }
}

impl Render for CommandsView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let commands = self.commands(cx);
        let count = self.rows.len();
        let rows: Vec<AnyElement> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(i, row)| {
                let command = commands.iter().find(|c| c.id == row.id)?;
                Some(self.render_row(i, row, command, count, cx))
            })
            .collect();
        div()
            .id("commands-view")
            .track_focus(&self.focus)
            .size_full()
            .overflow_y_scroll()
            .p_4()
            .flex()
            .flex_col()
            .gap_3()
            .max_w(px(820.))
            .child(div().text_lg().child("Toolbar Commands"))
            .child(
                div()
                    .text_sm()
                    .text_color(c.muted_foreground)
                    .child(
                        "Each command gets a button in the top bar (unless hidden) and can have a ⌘ shortcut. \
                         It runs in the project's main folder, in a terminal tab of its own: running it again \
                         reuses that tab, stopping a run that is still going (Ctrl-C) first. Turn on \
                         “New terminal each time” to get a fresh tab instead. Saved with the project in the \
                         app's data folder.",
                    ),
            )
            .children(rows)
            .child(
                div().child(
                    Button::new("cmd-add")
                        .outline()
                        .icon(Icon::from(gpui_kit::assets::IconName::Plus))
                        .label("Add Command")
                        .on_click(cx.listener(|this, _, _, cx| this.add(cx))),
                ),
            )
    }
}

/// The icon picker: Automatic (picked from the name), No icon, and every
/// built-in icon, with what it is for in its tooltip.
fn icon_picker(
    id: &str,
    current: Option<&str>,
    guess: &'static str,
    view: WeakEntity<CommandsView>,
    cx: &App,
) -> AnyElement {
    let c = Colors::of(cx);
    let choose = |icon: Option<String>| {
        let view = view.clone();
        let id = id.to_owned();
        move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
            let _ = view.update(cx, |this, cx| this.set_icon(&id, icon.clone(), cx));
        }
    };
    let option = |key: &'static str, text: String, icon: Option<String>, selected: bool, preview: Option<&'static str>| {
        let mut b = Button::new(SharedString::from(format!("icon-opt-{key}")))
            .ghost()
            .size(ButtonSize::Sm)
            .label(text)
            .toggled(selected)
            .on_click(choose(icon));
        if let Some(p) = preview {
            b = b.icon(Icon::from(project_commands::icon_image(p)));
        }
        // The selected choice: a tinted cell, as in the grid below.
        div().rounded(px(4.)).when(selected, |el| el.bg(c.selected_row)).child(b)
    };
    let mut grid = div().flex().flex_wrap().gap_0p5();
    for (key, what) in ICONS {
        let selected = current == Some(*key);
        grid = grid.child(
            div().rounded(px(4.)).when(selected, |el| el.bg(c.selected_row)).child(
                Button::new(SharedString::from(format!("icon-{key}")))
                    .ghost()
                    .size(ButtonSize::Sm)
                    .icon(Icon::from(project_commands::icon_image(key)))
                    .toggled(selected)
                    .tooltip(*what)
                    .on_click(choose(Some((*key).to_owned()))),
            ),
        );
    }
    div()
        .w(px(300.))
        .p_1()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .gap_1()
                .child(option(
                    "auto",
                    format!("Automatic ({})", command_icons::label(guess).unwrap_or("Run")),
                    None,
                    current.is_none(),
                    Some(guess),
                ))
                .child(option("none", "No icon".to_owned(), Some(NO_ICON.to_owned()), current == Some(NO_ICON), None)),
        )
        .child(div().h(px(1.)).bg(c.border))
        .child(grid)
        .child(
            div()
                .text_xs()
                .text_color(c.muted_foreground)
                .child("Automatic picks one from the name, then the command: Build gets a hammer, Test a flask."),
        )
        .into_any_element()
}

