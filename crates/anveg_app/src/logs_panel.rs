//! The logs panel along the bottom of a project, under the sidebar and the
//! main area: the Git log (every git command the app runs that changes a
//! repository or reaches a remote) and the GitHub log (GitHub operations
//! and their requests), as tabs.
//!
//! Closed, it is a thin bar: the two tabs (a red dot on one whose newest
//! line is a failure) and the newest line of whichever log changed last.
//! Click a tab (or View ▸ Logs, ⌥⌘L) to open it; the window makes it
//! resizable from its top edge (`panes`). Its open state, tab and height
//! are saved with the project's window state.

use anveg_core::op_log::OpLog;
use futures::StreamExt as _;
use gpui_cn::prelude::*;
use gpui_kit::assets::IconName;
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, IntoElement, ParentElement as _, Render, SharedString,
    Styled as _, Task, Window, div, px,
};

use crate::model::model;
use crate::op_log_view::{OpLogView, newest_line};
use crate::ui::Colors;

/// The bar's height (the panel's height when closed).
pub const BAR_HEIGHT: f32 = 28.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogTab {
    Git,
    GitHub,
}

impl LogTab {
    pub fn key(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::GitHub => "github",
        }
    }

    pub fn from_key(key: &str) -> Self {
        if key == "github" { Self::GitHub } else { Self::Git }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Git => "Git",
            Self::GitHub => "GitHub",
        }
    }
}

/// The panel opened, closed or changed tab: the window lays itself out
/// again and saves.
pub struct LogsChanged;

pub struct LogsPanel {
    git: Entity<OpLogView>,
    github: Entity<OpLogView>,
    pub tab: LogTab,
    pub open: bool,
    /// The log that changed last (its newest line shows in the closed bar).
    latest: LogTab,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<LogsChanged> for LogsPanel {}

impl LogsPanel {
    pub fn new(open: bool, tab: LogTab, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (git_log, github_log) = {
            let m = model(cx).read(cx);
            (m.git_log.clone(), m.github_log.clone())
        };
        let git = cx.new(|cx| {
            OpLogView::new(
                git_log.clone(),
                "Nothing yet. Commits, staging, push, pull, fetch, branches and stashes show up here.",
                window,
                cx,
            )
        });
        let github = cx.new(|cx| {
            OpLogView::new(
                github_log.clone(),
                "Nothing yet. Sign-in, pull requests, CI runs, Publish and clone show up here.",
                window,
                cx,
            )
        });
        let tasks = vec![
            Self::watch(git_log, LogTab::Git, cx),
            Self::watch(github_log, LogTab::GitHub, cx),
        ];
        let mut this = Self {
            git,
            github,
            tab,
            open,
            latest: tab,
            _tasks: tasks,
        };
        this.show_current(window, cx);
        this
    }

    fn watch(log: OpLog, tab: LogTab, cx: &mut Context<Self>) -> Task<()> {
        let mut rx = log.watch();
        cx.spawn(async move |this, cx| {
            while rx.next().await.is_some() {
                while rx.try_recv().is_ok() {}
                if this
                    .update(cx, |this, cx| {
                        this.latest = tab;
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
    }

    fn view(&self, tab: LogTab) -> &Entity<OpLogView> {
        match tab {
            LogTab::Git => &self.git,
            LogTab::GitHub => &self.github,
        }
    }

    fn show_current(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        for tab in [LogTab::Git, LogTab::GitHub] {
            let visible = self.open && tab == self.tab;
            self.view(tab).update(cx, |v, cx| v.set_visible(visible, window, cx));
        }
    }

    /// Open on `tab`, or close when that tab is already showing.
    pub fn click_tab(&mut self, tab: LogTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.open && self.tab == tab {
            self.open = false;
        } else {
            self.open = true;
            self.tab = tab;
        }
        self.changed(window, cx);
    }

    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = !self.open;
        self.changed(window, cx);
    }

    fn changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_current(window, cx);
        cx.emit(LogsChanged);
        cx.notify();
    }

    fn render_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let current_log = self.view(self.tab).read(cx).log().clone();
        let tab_button = |tab: LogTab, cx: &mut Context<Self>| {
            let log = self.view(tab).read(cx).log().clone();
            let failed = newest_line(&log).is_some_and(|l| l.1);
            let shown = self.open && self.tab == tab;
            Button::new(SharedString::from(format!("logs-tab-{}", tab.key())))
                .ghost()
                .size(ButtonSize::Xs)
                .label(tab.label())
                .selected(shown)
                .toggled(shown)
                .when(failed, |b| b.child(div().size(px(6.)).rounded_full().bg(c.destructive)))
                .tooltip(if shown {
                    "Hide the log (⌥⌘L)".to_owned()
                } else {
                    format!("Show the {} log", tab.label())
                })
                .on_click(cx.listener(move |this, _, window, cx| this.click_tab(tab, window, cx)))
        };
        let preview = (!self.open)
            .then(|| {
                let log = self.view(self.latest).read(cx).log().clone();
                newest_line(&log).map(|(text, failed)| (format!("{}: {text}", self.latest.label()), failed))
            })
            .flatten();
        let failed = preview.as_ref().is_some_and(|p| p.1);
        div()
            .flex_none()
            .h(px(BAR_HEIGHT))
            .px_1()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .bg(c.sidebar)
            .border_t_1()
            .border_color(c.border)
            .child(
                Button::new("logs-toggle")
                    .ghost()
                    .size(ButtonSize::Xs)
                    .icon(Icon::from(if self.open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronUp
                    }))
                    .tooltip(if self.open { "Hide the logs (⌥⌘L)" } else { "Show the logs (⌥⌘L)" })
                    .on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx))),
            )
            .child(tab_button(LogTab::Git, cx))
            .child(tab_button(LogTab::GitHub, cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .pl_2()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(if failed { c.destructive } else { c.muted_foreground })
                    .children(preview.map(|p| p.0)),
            )
            .when(self.open, |el| {
                let empty = current_log.is_empty();
                el.child(
                    Button::new("logs-copy")
                        .ghost()
                        .size(ButtonSize::Xs)
                        .icon(Icon::from(IconName::Copy))
                        .tooltip("Copy the whole log")
                        .disabled(empty)
                        .on_click(cx.listener(|this, _, _, cx| {
                            let view = this.view(this.tab).clone();
                            view.update(cx, |v, cx| v.copy_all(cx));
                        })),
                )
                .child(
                    Button::new("logs-clear")
                        .ghost()
                        .size(ButtonSize::Xs)
                        .icon(Icon::from(IconName::Eraser))
                        .tooltip("Clear the log")
                        .disabled(empty)
                        .on_click(move |_, _, _| current_log.clear()),
                )
            })
    }
}

impl Render for LogsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let bar = self.render_bar(cx);
        let body = self.view(self.tab).clone();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(c.background)
            .child(bar)
            .when(self.open, |el| {
                el.child(div().flex_1().min_h_0().border_t_1().border_color(c.border).child(body))
            })
    }
}
