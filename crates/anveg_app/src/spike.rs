//! The Milestone 1 spike window: a Claude tab and a Terminal tab.

use std::path::PathBuf;

use anveg_core::launcher::LaunchSpec;
use anveg_term::{ExitInfo, TerminalSettings, TerminalView, TerminalViewEvent};
use gpui_cn::prelude::*;
use gpui_kit::{
    AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, Styled as _, Subscription, Window, div,
};

use crate::{CloseWindow, ToggleTab};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tab {
    Claude,
    Terminal,
}

impl Tab {
    fn other(self) -> Self {
        match self {
            Tab::Claude => Tab::Terminal,
            Tab::Terminal => Tab::Claude,
        }
    }
}

/// A started terminal and what happened to it.
struct Slot {
    view: Entity<TerminalView>,
    ended: Option<Ended>,
    _events: Subscription,
}

#[derive(Clone, Debug)]
enum Ended {
    Exited(ExitInfo),
    Failed(SharedString),
}

pub struct SpikeWindow {
    active: Tab,
    claude: Option<Slot>,
    shell: Option<Slot>,
    settings: TerminalSettings,
    cwd: Option<PathBuf>,
    _quit: Subscription,
}

impl SpikeWindow {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let quit = cx.on_app_quit(|this: &mut Self, cx| {
            this.stop_all(cx);
            async {}
        });
        cx.on_release(|this: &mut Self, cx| {
            for slot in [this.claude.take(), this.shell.take()]
                .into_iter()
                .flatten()
            {
                slot.view.update(cx, |view, cx| view.stop(cx));
            }
        })
        .detach();
        let mut this = Self {
            active: Tab::Claude,
            claude: None,
            shell: None,
            settings: TerminalSettings::default(),
            cwd: std::env::var_os("ANVEG_SPIKE_DIR")
                .map(PathBuf::from)
                .or_else(|| std::env::current_dir().ok()),
            _quit: quit,
        };
        this.show(Tab::Claude, window, cx);
        this
    }

    fn launch_spec(&self, tab: Tab) -> LaunchSpec {
        match tab {
            Tab::Terminal => LaunchSpec::login_shell(self.cwd.clone()),
            Tab::Claude => {
                let program = std::env::var("ANVEG_CLAUDE").unwrap_or_else(|_| "claude".to_owned());
                LaunchSpec::program(program, Vec::<String>::new(), self.cwd.clone())
            }
        }
    }

    fn slot(&self, tab: Tab) -> Option<&Slot> {
        match tab {
            Tab::Claude => self.claude.as_ref(),
            Tab::Terminal => self.shell.as_ref(),
        }
    }

    fn slot_mut(&mut self, tab: Tab) -> &mut Option<Slot> {
        match tab {
            Tab::Claude => &mut self.claude,
            Tab::Terminal => &mut self.shell,
        }
    }

    fn start(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(old) = self.slot_mut(tab).take() {
            old.view.update(cx, |view, cx| view.stop(cx));
        }
        let spec = self.launch_spec(tab);
        let settings = self.settings.clone();
        let id = match tab {
            Tab::Claude => "claude",
            Tab::Terminal => "shell",
        };
        let view = cx.new(|cx| TerminalView::spawn(id, &spec, &settings, window, cx));
        let events = cx.subscribe(&view, move |this, _, event, cx| {
            let ended = match event {
                TerminalViewEvent::Exited(info) => Some(Ended::Exited(info.clone())),
                TerminalViewEvent::Failed(message) => Some(Ended::Failed(message.clone())),
                TerminalViewEvent::TitleChanged(_) => None,
            };
            if let Some(ended) = ended
                && let Some(slot) = this.slot_mut(tab)
            {
                slot.ended = Some(ended);
                cx.notify();
            }
        });
        *self.slot_mut(tab) = Some(Slot {
            view,
            ended: None,
            _events: events,
        });
    }

    fn show(&mut self, tab: Tab, window: &mut Window, cx: &mut Context<Self>) {
        self.active = tab;
        if self.slot(tab).is_none() {
            self.start(tab, window, cx);
        }
        for (t, slot) in [(Tab::Claude, &self.claude), (Tab::Terminal, &self.shell)] {
            if let Some(slot) = slot {
                slot.view
                    .update(cx, |view, cx| view.set_visible(t == tab, cx));
            }
        }
        if let Some(slot) = self.slot(tab) {
            let handle = slot.view.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        cx.notify();
    }

    fn stop_all(&mut self, cx: &mut Context<Self>) {
        for slot in [&self.claude, &self.shell].into_iter().flatten() {
            slot.view.update(cx, |view, cx| view.stop(cx));
        }
    }

    fn restart(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.active;
        self.start(tab, window, cx);
        self.show(tab, window, cx);
    }

    fn tab_button(&self, tab: Tab, cx: &mut Context<Self>) -> impl IntoElement {
        let (id, label) = match tab {
            Tab::Claude => ("tab-claude", "Claude"),
            Tab::Terminal => ("tab-terminal", "Terminal"),
        };
        Button::new(id)
            .ghost()
            .size(ButtonSize::Sm)
            .label(label)
            .toggled(self.active == tab)
            .on_click(cx.listener(move |this, _, window, cx| this.show(tab, window, cx)))
    }

    fn ended_overlay(&self, ended: &Ended, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (background, border, foreground, muted) = (
            theme.background(),
            theme.border(),
            theme.foreground(),
            theme.muted_foreground(),
        );
        let (title, detail) = match (self.active, ended) {
            (Tab::Claude, Ended::Exited(info)) => ("Session ended", exit_detail(info)),
            (Tab::Terminal, Ended::Exited(info)) => ("Process exited", exit_detail(info)),
            (_, Ended::Failed(message)) => ("Could not start", Some(message.to_string())),
        };
        let button = match self.active {
            Tab::Claude => "Start again",
            Tab::Terminal => "Restart",
        };
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(background.opacity(0.6))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(border)
                    .bg(background)
                    .text_color(foreground)
                    .child(div().text_lg().child(title))
                    .when_some(detail, |el, detail| {
                        el.child(div().text_sm().text_color(muted).child(detail))
                    })
                    .child(
                        Button::new("restart")
                            .primary()
                            .label(button)
                            .on_click(cx.listener(|this, _, window, cx| this.restart(window, cx))),
                    ),
            )
    }
}

fn exit_detail(info: &ExitInfo) -> Option<String> {
    match (&info.code, &info.signal) {
        (Some(0), _) => None,
        (Some(code), _) => Some(format!("Exit code {code}")),
        (None, Some(signal)) => Some(format!("Ended by {signal}")),
        (None, None) => None,
    }
}

impl Render for SpikeWindow {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.slot(self.active);
        let view = active.map(|slot| slot.view.clone());
        let ended = active.and_then(|slot| slot.ended.clone());
        let theme = cx.theme();
        let border = theme.border();
        div()
            .id("spike-window")
            .size_full()
            .flex()
            .flex_col()
            .on_action(cx.listener(|this, _: &ToggleTab, window, cx| {
                let tab = this.active.other();
                this.show(tab, window, cx);
            }))
            .on_action(cx.listener(|_, _: &CloseWindow, window, _| window.remove_window()))
            .child(
                TitleBar::new().border_b_1().border_color(border).child(
                    div()
                        .flex()
                        .gap_1()
                        .child(self.tab_button(Tab::Claude, cx))
                        .child(self.tab_button(Tab::Terminal, cx)),
                ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .when_some(view, |el, view| el.child(view))
                    .when_some(ended, |el, ended| el.child(self.ended_overlay(&ended, cx))),
            )
    }
}
