//! Settings (⌘,), build plan section 6.8 plus the Phase 2 settings.
//! Changes are saved at once; text fields apply on Enter or when they lose
//! focus.

use anveg_core::config::Settings;
use gpui_cn::prelude::*;
use gpui_cn::{Input, InputEvent, InputState, TitleBar};
use gpui_kit::{
    AnyElement, App, AppContext as _, Bounds, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, WindowBounds, WindowOptions, div, px, size,
};

use crate::actions::CloseWindow;
use crate::model::{AppModel, model};
use crate::ui::Colors;

/// A text field bound to a setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    ClaudePath,
    DefaultFlags,
    FontFamily,
    FontSize,
    Scrollback,
    IdleMinutes,
    /// Phase 4: a language server command.
    Lsp(&'static str),
    /// Phase 4: extra file types ("tpl=html, jsonl=json").
    FileTypes,
}

/// The languages with a server command in Settings ▸ Editor.
const LSP_LANGUAGES: [(&str, &str); 5] = [
    ("rust", "Rust"),
    ("typescript", "TypeScript / TSX"),
    ("javascript", "JavaScript"),
    ("python", "Python"),
    ("go", "Go"),
];

pub struct SettingsWindow {
    model: Entity<AppModel>,
    fields: Vec<(Field, Entity<InputState>)>,
    error: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

pub fn open(cx: &mut App) {
    let model = model(cx);
    if let Some(handle) = model.read(cx).settings_window
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(640.), px(720.)),
            cx,
        ))),
        ..TitleBar::window_options(cx)
    };
    match gpui_kit::open_window(options, cx, |window, cx| {
        window.set_window_title("Settings");
        cx.new(|cx| SettingsWindow::new(window, cx))
    }) {
        Ok((handle, view)) => {
            model.update(cx, |m, _| m.settings_window = Some(handle));
            let model2 = model.clone();
            view.update(cx, |_, cx| {
                cx.on_release(move |_, cx| model2.update(cx, |m, _| m.settings_window = None))
                    .detach();
            });
        }
        Err(error) => tracing::error!(%error, "could not open Settings"),
    }
}

fn field_value(field: Field, s: &Settings) -> String {
    match field {
        Field::ClaudePath => s
            .claude_path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        Field::DefaultFlags => s.default_flags.clone(),
        Field::FontFamily => s.terminal.font_family.clone(),
        Field::FontSize => format!("{}", s.terminal.font_size),
        Field::Scrollback => s.terminal.scrollback_lines.to_string(),
        Field::IdleMinutes => s.idle_stop_minutes.to_string(),
        Field::Lsp(lang) => s.lsp_servers.get(lang).cloned().unwrap_or_default(),
        Field::FileTypes => s
            .editor
            .file_types
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join(", "),
    }
}

impl SettingsWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = model(cx);
        let settings = model.read(cx).settings.clone();
        let mut fields = Vec::new();
        let mut subscriptions = vec![cx.observe(&model, |_, _, cx| cx.notify())];
        let all = [
            Field::ClaudePath,
            Field::DefaultFlags,
            Field::FontFamily,
            Field::FontSize,
            Field::Scrollback,
            Field::IdleMinutes,
            Field::FileTypes,
        ]
        .into_iter()
        .chain(LSP_LANGUAGES.iter().map(|(l, _)| Field::Lsp(l)));
        for field in all {
            let value = field_value(field, &settings);
            let placeholder = match field {
                Field::ClaudePath => "Auto-detect on PATH",
                Field::DefaultFlags => "e.g. --permission-mode plan",
                Field::Lsp(_) => "Empty turns it off",
                Field::FileTypes => "e.g. tpl=html, jsonl=json",
                _ => "",
            };
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            });
            subscriptions.push(
                cx.subscribe_in(&input, window, move |this, _, event, _, cx| {
                    if matches!(event, InputEvent::PressEnter { .. } | InputEvent::Blur) {
                        this.apply(field, cx);
                    }
                }),
            );
            fields.push((field, input));
        }
        Self {
            model,
            fields,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    fn input(&self, field: Field) -> Option<&Entity<InputState>> {
        self.fields
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, i)| i)
    }

    fn apply(&mut self, field: Field, cx: &mut Context<Self>) {
        let Some(input) = self.input(field) else {
            return;
        };
        let text = input.read(cx).value().trim().to_owned();
        let current = self.model.read(cx).settings.clone();
        if text == field_value(field, &current) {
            return;
        }
        let mut next = current;
        let result: Result<(), String> = match field {
            Field::ClaudePath => {
                next.claude_path = (!text.is_empty()).then(|| text.clone().into());
                Ok(())
            }
            Field::DefaultFlags => shell_words::split(&text)
                .map(|_| next.default_flags = text.clone())
                .map_err(|e| format!("Default flags: {e}")),
            Field::FontFamily => {
                if text.is_empty() {
                    Err("The font family can't be empty.".into())
                } else {
                    next.terminal.font_family = text.clone();
                    Ok(())
                }
            }
            Field::FontSize => match text.parse::<f32>() {
                Ok(v) if (6.0..=72.0).contains(&v) => {
                    next.terminal.font_size = v;
                    Ok(())
                }
                _ => Err("Font size must be a number from 6 to 72.".into()),
            },
            Field::Scrollback => match text.parse::<usize>() {
                Ok(v) if (100..=1_000_000).contains(&v) => {
                    next.terminal.scrollback_lines = v;
                    Ok(())
                }
                _ => Err("Scrollback must be a whole number from 100 to 1,000,000.".into()),
            },
            Field::IdleMinutes => match text.parse::<u32>() {
                Ok(v) if v <= 24 * 60 => {
                    next.idle_stop_minutes = v;
                    Ok(())
                }
                _ => Err("Minutes must be a whole number (0 turns it off).".into()),
            },
            Field::Lsp(lang) => shell_words::split(&text)
                .map(|_| {
                    next.lsp_servers.insert(lang.to_owned(), text.clone());
                })
                .map_err(|e| format!("Language server command: {e}")),
            Field::FileTypes => {
                let mut map = std::collections::BTreeMap::new();
                let mut bad = None;
                for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    match part.split_once('=') {
                        Some((k, v)) if !k.trim().is_empty() && !v.trim().is_empty() => {
                            map.insert(
                                k.trim().trim_start_matches('.').to_lowercase(),
                                v.trim().to_lowercase(),
                            );
                        }
                        _ => bad = Some(part.to_owned()),
                    }
                }
                match bad {
                    Some(part) => Err(format!("File types: “{part}” should look like ext=language.")),
                    None => {
                        next.editor.file_types = map;
                        Ok(())
                    }
                }
            }
        };
        match result {
            Ok(()) => {
                self.error = None;
                self.model
                    .update(cx, |m, cx| m.update_settings(cx, |s| *s = next));
            }
            Err(message) => self.error = Some(message.into()),
        }
        cx.notify();
    }

    fn toggle(&mut self, cx: &mut Context<Self>, f: impl FnOnce(&mut Settings, bool), value: bool) {
        self.model
            .update(cx, |m, cx| m.update_settings(cx, |s| f(s, value)));
    }

    fn row(&self, label: &str, help: Option<&str>, control: AnyElement, cx: &App) -> AnyElement {
        let c = Colors::of(cx);
        div()
            .flex()
            .items_center()
            .gap_3()
            .py_1()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(div().text_sm().child(label.to_owned()))
                    .when_some(help, |el, h| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child(h.to_owned()),
                        )
                    }),
            )
            .child(div().w(px(260.)).flex().justify_end().child(control))
            .into_any_element()
    }

    fn switch(
        &self,
        id: &'static str,
        checked: bool,
        f: fn(&mut Settings, bool),
        cx: &mut Context<Self>,
    ) -> AnyElement {
        Switch::new(id)
            .checked(checked)
            .on_change(cx.listener(move |this, value: &bool, _, cx| this.toggle(cx, f, *value)))
            .into_any_element()
    }

    fn text(&self, field: Field) -> AnyElement {
        match self.input(field) {
            Some(input) => div().w_full().child(Input::new(input)).into_any_element(),
            None => div().into_any_element(),
        }
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let (s, claude) = {
            let m = self.model.read(cx);
            (m.settings.clone(), m.claude.clone())
        };
        let detected_path = claude
            .as_ref()
            .and_then(|c| c.path.as_ref())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| {
                if claude.is_some() {
                    "not found".into()
                } else {
                    "detecting…".into()
                }
            });
        let version = claude
            .as_ref()
            .and_then(|c| c.version.clone())
            .unwrap_or_else(|| "—".into());
        let config_dir = claude
            .as_ref()
            .map(|c| c.config_dir.display().to_string())
            .unwrap_or_else(|| "detecting…".into());
        let hooks_on = self.model.read(cx).hooks.is_some();
        let section = |title: &str| {
            div()
                .pt_4()
                .pb_1()
                .text_xs()
                .text_color(c.muted_foreground)
                .child(title.to_uppercase())
        };
        let readonly = |text: String| div().text_sm().truncate().child(text).into_any_element();

        div()
            .id("settings")
            .size_full()
            .flex()
            .flex_col()
            .bg(c.background)
            .text_color(c.foreground)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .child(TitleBar::new().child(div().text_sm().child("Settings")))
            .child(
                div()
                    .id("settings-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_6()
                    .pb_6()
                    .flex()
                    .flex_col()
                    .when_some(self.error.clone(), |el, e| {
                        el.child(div().py_1().text_sm().text_color(c.destructive).child(e))
                    })
                    .child(section("General"))
                    .child(self.row(
                        "Confirm before stopping running sessions",
                        Some("On close or quit."),
                        self.switch("s-confirm", s.confirm_on_close, |s, v| s.confirm_on_close = v, cx),
                        cx,
                    ))
                    .child(self.row(
                        "Resume sessions that were running when I quit",
                        None,
                        self.switch("s-resume", s.resume_on_launch, |s, v| s.resume_on_launch = v, cx),
                        cx,
                    ))
                    .child(section("Claude"))
                    .child(self.row("Path to claude", Some(&format!("Detected: {detected_path}")), self.text(Field::ClaudePath), cx))
                    .child(self.row("Version", None, readonly(version), cx))
                    .child(self.row("Config directory", None, readonly(config_dir), cx))
                    .child(self.row(
                        "Default extra flags",
                        Some("Added to every claude launch. Applies to sessions started afterwards."),
                        self.text(Field::DefaultFlags),
                        cx,
                    ))
                    .child(self.row(
                        "Show non-interactive sessions",
                        Some("Sessions from `claude -p` and the Agent SDK."),
                        self.switch("s-nonint", s.show_noninteractive, |s, v| s.show_noninteractive = v, cx),
                        cx,
                    ))
                    .child(section("Terminal"))
                    .child(self.row("Font family", None, self.text(Field::FontFamily), cx))
                    .child(self.row("Font size", None, self.text(Field::FontSize), cx))
                    .child(self.row(
                        "Scrollback lines",
                        Some("Applies to terminals started afterwards."),
                        self.text(Field::Scrollback),
                        cx,
                    ))
                    .child(self.row(
                        "Cursor blink",
                        Some("Off saves idle CPU."),
                        self.switch("s-blink", s.terminal.cursor_blink, |s, v| s.terminal.cursor_blink = v, cx),
                        cx,
                    ))
                    .child(self.row(
                        "Option key acts as Meta",
                        None,
                        self.switch("s-meta", s.terminal.option_as_meta, |s, v| s.terminal.option_as_meta = v, cx),
                        cx,
                    ))
                    .child(section("Editor"))
                    .child(self.row(
                        "Soft wrap",
                        Some("For files opened afterwards; each editor also has a Wrap toggle."),
                        self.switch("s-wrap", s.editor.soft_wrap, |s, v| s.editor.soft_wrap = v, cx),
                        cx,
                    ))
                    .child(self.row(
                        "Format on save",
                        Some("Uses the file's language server."),
                        self.switch("s-fmt", s.editor.format_on_save, |s, v| s.editor.format_on_save = v, cx),
                        cx,
                    ))
                    .child(self.row(
                        "Extra file types",
                        Some("Highlight these extensions as another language."),
                        self.text(Field::FileTypes),
                        cx,
                    ))
                    .child(section("Language servers"))
                    .child(
                        div()
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child("Found on your login shell's PATH. A server starts when you open a file in its language and stops 10 minutes after the last one closes."),
                    )
                    .children(LSP_LANGUAGES.iter().map(|(lang, label)| {
                        self.row(label, None, self.text(Field::Lsp(lang)), cx)
                    }))
                    .child(section("Git"))
                    .child(self.row(
                        "Pull",
                        Some("How Pull combines remote changes with yours."),
                        {
                            let mut row = div().flex().gap_1();
                            for (mode, label) in [
                                (anveg_core::git::PullMode::FastForwardOnly, "Fast-forward only"),
                                (anveg_core::git::PullMode::Merge, "Merge"),
                                (anveg_core::git::PullMode::Rebase, "Rebase"),
                            ] {
                                row = row.child(
                                    gpui_cn::Button::new(gpui_kit::SharedString::from(format!("pull-{label}")))
                                        .ghost()
                                        .size(gpui_cn::ButtonSize::Sm)
                                        .label(label)
                                        .toggled(s.pull_mode == mode)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.model.update(cx, |m, cx| {
                                                m.update_settings(cx, |s| s.pull_mode = mode)
                                            })
                                        })),
                                );
                            }
                            row.into_any_element()
                        },
                        cx,
                    ))
                    .child(section("Status and resources"))
                    .when(!hooks_on, |el| {
                        el.child(
                            div()
                                .text_xs()
                                .text_color(c.muted_foreground)
                                .child("Live status is off: anveg-hook was not found next to the app (run `cargo build`, or use the bundled app)."),
                        )
                    })
                    .child(self.row(
                        "Notifications",
                        Some("When a session you're not looking at needs input or finishes."),
                        self.switch("s-notify", s.notifications, |s, v| s.notifications = v, cx),
                        cx,
                    ))
                    .child(self.row(
                        "Stop idle sessions after (minutes)",
                        Some("Only hidden, idle sessions. 0 turns it off."),
                        self.text(Field::IdleMinutes),
                        cx,
                    ))
                    .child(self.row(
                        "Show memory use",
                        Some("In each running session's tooltip; sampled every 10 s."),
                        self.switch("s-memory", s.show_memory, |s, v| s.show_memory = v, cx),
                        cx,
                    )),
            )
    }
}
