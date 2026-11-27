//! A log's text: what the app did (the Git log, the GitHub log), each
//! operation with ✓ or ✗ and what happened inside it. Selectable and
//! copyable (it is a read-only editor; ⌘A ⌘C work, and so does search).
//! Memory only: logs start empty on every launch.
//!
//! Two uses: a tab of the window's bottom logs panel (`logs_panel`), where
//! the panel draws the header; and on its own with a small header of its
//! own (collapse toggle, newest line, Copy, Clear), as in the Open from
//! GitHub window.
//!
//! While the text has focus (someone is selecting), or the log is not
//! shown, new lines wait, so a selection never jumps.

use std::time::Duration;

use anveg_core::op_log::{Level, OpLog};
use futures::StreamExt as _;
use gpui_cn::prelude::*;
use gpui_kit::assets::IconName;
use gpui_kit::component::input::{Editor, EditorState};
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, Focusable as _, IntoElement, ParentElement as _, Render,
    SharedString, Styled as _, Task, Window, div, point, px,
};

use crate::ui::Colors;

/// How often the text is rebuilt while lines arrive.
const BATCH: Duration = Duration::from_millis(120);

/// The header drawn by a log shown on its own.
struct OwnHeader {
    id: SharedString,
    label: SharedString,
    height: f32,
}

pub struct OpLogView {
    log: OpLog,
    editor: Entity<EditorState>,
    /// Shown (its panel tab is open and selected, or it is expanded).
    visible: bool,
    /// New lines not yet shown (hidden, or the text has focus).
    stale: bool,
    header: Option<OwnHeader>,
    _task: Task<()>,
    _subs: Vec<gpui_kit::Subscription>,
}

impl OpLogView {
    /// A log for a tab of the bottom panel: just the text; the panel says
    /// when it is visible.
    pub fn new(log: OpLog, placeholder: &'static str, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("plaintext")
                .line_number(false)
                .indent_guides(false)
                .searchable(true)
                .soft_wrap(true)
                .placeholder(placeholder)
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
        Self {
            log,
            editor,
            visible: false,
            stale: true,
            header: None,
            _task: task,
            _subs: subs,
        }
    }

    /// A log on its own, with its own header (collapsed unless `open`).
    pub fn standalone(
        id: &str,
        label: &str,
        log: OpLog,
        open: bool,
        height: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut this = Self::new(log, "Nothing yet.", window, cx);
        this.header = Some(OwnHeader {
            id: SharedString::from(id.to_owned()),
            label: SharedString::from(label.to_owned()),
            height,
        });
        this.set_visible(open, window, cx);
        this
    }

    pub fn log(&self) -> &OpLog {
        &self.log
    }

    pub fn set_visible(&mut self, visible: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.visible = visible;
        if visible && self.stale {
            self.refresh(window, cx);
        }
        cx.notify();
    }

    pub fn copy_all(&self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.log.text()));
    }

    /// Show the current lines and scroll to the newest, unless the log is
    /// hidden or someone is selecting text.
    fn refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let focused = self.editor.read(cx).focus_handle(cx).contains_focused(window, cx);
        if !self.visible || focused {
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

    fn text(&self) -> gpui_kit::AnyElement {
        div()
            .size_full()
            .text_xs()
            .overflow_hidden()
            .child(Editor::new(&self.editor).bordered(false).readonly(true).h_full())
            .into_any_element()
    }
}

/// The newest line of `log` as one line of text, and whether it is a
/// failure (shown in red).
pub fn newest_line(log: &OpLog) -> Option<(String, bool)> {
    let last = log.last()?;
    let failed = last.level == Level::Failed;
    Some((last.render().lines().next().unwrap_or_default().to_owned(), failed))
}

impl Render for OpLogView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(header) = &self.header else {
            return self.text();
        };
        let c = Colors::of(cx);
        let id = header.id.clone();
        let eid = |name: &str| SharedString::from(format!("{id}-{name}"));
        let preview = newest_line(&self.log);
        let failed = preview.as_ref().is_some_and(|p| p.1);
        let bar = div()
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
                    .icon(Icon::from(if self.visible {
                        IconName::ChevronDown
                    } else {
                        IconName::ChevronUp
                    }))
                    .label(header.label.clone())
                    .on_click(cx.listener(|this, _, window, cx| {
                        let visible = !this.visible;
                        this.set_visible(visible, window, cx);
                    })),
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
                    .when(!self.visible, |el| el.children(preview.map(|p| p.0))),
            )
            .child(
                Button::new(eid("copy"))
                    .ghost()
                    .size(ButtonSize::Xs)
                    .icon(Icon::from(IconName::Copy))
                    .tooltip("Copy the whole log")
                    .disabled(self.log.is_empty())
                    .on_click(cx.listener(|this, _, _, cx| this.copy_all(cx))),
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
        let height = header.height;
        div()
            .flex_none()
            .flex()
            .flex_col()
            .border_t_1()
            .border_color(c.border)
            .bg(c.background)
            .child(bar)
            .when(self.visible, |el| {
                el.child(
                    div()
                        .h(px(height))
                        .border_t_1()
                        .border_color(c.border)
                        .child(self.text()),
                )
            })
            .into_any_element()
    }
}
