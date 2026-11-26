//! The GitHub log pane (bottom of the GitHub panel, the Publish tab and the
//! Open from GitHub window): what the app did with GitHub this run, each
//! operation with ✓ or ✗ and the requests inside it. Selectable and
//! copyable (it is a read-only editor; ⌘A ⌘C work, and so does search),
//! with Copy all and Clear. Memory only: the log starts empty on every
//! launch.
//!
//! While the text has focus (someone is selecting), new lines wait until
//! focus leaves, so a selection never jumps.

use std::time::Duration;

use anveg_core::github_log::{GitHubLog, Level};
use futures::StreamExt as _;
use gpui_cn::prelude::*;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, Focusable as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Task, Window, div, point, px,
};

use crate::model::model;
use crate::ui::Colors;

/// How often the text is rebuilt while lines arrive.
const BATCH: Duration = Duration::from_millis(120);

pub struct GitHubLogView {
    id: SharedString,
    log: GitHubLog,
    editor: Entity<EditorState>,
    open: bool,
    height: f32,
    /// New lines not yet shown (collapsed, or the text has focus).
    stale: bool,
    _task: Task<()>,
    _subs: Vec<gpui_kit::Subscription>,
}

impl GitHubLogView {
    /// `id` keeps element ids apart when several logs are on screen.
    pub fn new(id: &str, open: bool, height: f32, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let log = model(cx).read(cx).github_log.clone();
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("plaintext")
                .line_number(false)
                .indent_guides(false)
                .searchable(true)
                .soft_wrap(true)
                .placeholder("Nothing yet. Sign-in, pull requests, CI runs, Publish and clone show up here.")
        });
        editor.update(cx, |s, cx| s.set_readonly(true, cx));
        let mut rx = log.watch();
        let task = cx.spawn_in(window, async move |this, cx| {
            while rx.next().await.is_some() {
                cx.background_executor().timer(BATCH).await;
                // Everything that arrived meanwhile goes in one update.
                while rx.try_recv().is_ok() {}
                if this.update_in(cx, |this, window, cx| this.refresh(window, cx)).is_err() {
                    break;
                }
            }
        });
        let focus = editor.read(cx).focus_handle(cx);
        let subs = vec![cx.on_focus_out(&focus, window, |this, _, window, cx| {
            if this.stale {
                this.refresh(window, cx);
            }
        })];
        let mut this = Self {
            id: SharedString::from(id.to_owned()),
            log,
            editor,
            open,
            height,
            stale: true,
            _task: task,
            _subs: subs,
        };
        this.refresh(window, cx);
        this
    }

    /// Show the current lines and scroll to the newest, unless the pane is
    /// collapsed or someone is selecting text.
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.editor.read(cx).focus_handle(cx).contains_focused(window, cx);
        if !self.open || focused {
            self.stale = true;
            cx.notify();
            return;
        }
        self.stale = false;
        let text = self.log.text();
        self.editor.update(cx, |s, cx| {
            s.set_value(text, window, cx);
            // Clamped to the end of the text.
            s.set_scroll_offset(point(px(0.), px(-1.0e9)), cx);
        });
        cx.notify();
    }

    fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open = !self.open;
        if self.open && self.stale {
            self.refresh(window, cx);
        }
        cx.notify();
    }
}

impl Render for GitHubLogView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let id = self.id.clone();
        let eid = |name: &str| SharedString::from(format!("{id}-{name}"));
        let last = self.log.last();
        let failed = last.as_ref().is_some_and(|l| l.level == Level::Failed);
        let preview = last.map(|l| l.render().lines().next().unwrap_or_default().to_owned());
        let header = div()
            .flex_none()
            .h(px(28.))
            .px_1()
            .flex()
            .items_center()
            .gap_1()
            .text_xs()
            .child(
                Button::new(eid("toggle"))
                    .ghost()
                    .size(ButtonSize::Xs)
                    .icon(Icon::from(if self.open {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronUp
                    }))
                    .label("GitHub log")
                    .on_click(cx.listener(|this, _, window, cx| this.toggle(window, cx))),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis()
                    .text_color(if failed { c.destructive } else { c.muted_foreground })
                    // The newest line, so a failure shows even when collapsed.
                    .when(!self.open, |el| el.children(preview)),
            )
            .child(
                Button::new(eid("copy"))
                    .ghost()
                    .size(ButtonSize::Xs)
                    .icon(Icon::from(IconName::Copy))
                    .tooltip("Copy the whole log")
                    .disabled(self.log.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| {
                        cx.write_to_clipboard(ClipboardItem::new_string(this.log.text()));
                    })),
            )
            .child(
                Button::new(eid("clear"))
                    .ghost()
                    .size(ButtonSize::Xs)
                    .icon(Icon::from(IconName::Eraser))
                    .tooltip("Clear the log")
                    .disabled(self.log.is_empty())
                    .on_click(cx.listener(|this, _, _, _| this.log.clear())),
            );
        div()
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(c.border)
            .bg(c.background)
            .child(header)
            .when(self.open, |el| {
                el.child(
                    div()
                        .h(px(self.height))
                        .border_t_1()
                        .border_color(c.border)
                        .text_xs()
                        .overflow_hidden()
                        .child(Editor::new(&self.editor).bordered(false).readonly(true).h_full()),
                )
            })
    }
}
