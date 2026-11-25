//! The welcome window: recent projects and Open Folder… (build plan, 6.1).
//! Shown when no project window is open.

use gpui_cn::TitleBar;
use gpui_cn::prelude::*;
use gpui_kit::{
    App, AppContext as _, Bounds, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Window, WindowBounds, WindowOptions, div, px, size,
};
use uuid::Uuid;

use crate::actions::{CloseWindow, OpenFolder};
use crate::model::{AppModel, model};
use crate::ui::Colors;

pub struct Welcome {
    model: Entity<AppModel>,
    _subscription: Subscription,
}

/// Open the welcome window (or focus it).
pub fn open(cx: &mut App) {
    let model = model(cx);
    if let Some(handle) = model.read(cx).welcome
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(620.), px(460.)),
            cx,
        ))),
        ..TitleBar::window_options(cx)
    };
    match gpui_kit::open_window(options, cx, |window, cx| {
        let sandbox = crate::model::model(cx).read(cx).paths.is_sandbox();
        window.set_window_title(if sandbox { "Anveg Code (Dev)" } else { "Anveg Code" });
        cx.new(|cx| {
            let model = crate::model::model(cx);
            let sub = cx.observe(&model, |_, _, cx| cx.notify());
            Welcome {
                model,
                _subscription: sub,
            }
        })
    }) {
        Ok((handle, _)) => model.update(cx, |m, _| m.welcome = Some(handle)),
        Err(error) => tracing::error!(%error, "could not open the welcome window"),
    }
}

/// Close the welcome window, if open.
pub fn close(cx: &mut App) {
    let model = model(cx);
    if let Some(handle) = model.update(cx, |m, _| m.welcome.take()) {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    }
}

impl Render for Welcome {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let m = self.model.read(cx);
        let recents: Vec<(Uuid, String, String, bool)> = m
            .projects
            .recent()
            .into_iter()
            .map(|p| {
                (
                    p.id,
                    p.name.clone(),
                    p.main_folder.display().to_string(),
                    p.folder_exists(),
                )
            })
            .collect();
        let claude_line: Option<(String, bool)> = match &m.claude {
            None => Some(("Looking for Claude Code…".to_owned(), false)),
            Some(info) if info.path.is_none() || info.version.is_none() => Some((
                "Claude Code was not found. Install it (https://code.claude.com/docs/en/setup), \
                 then restart Anveg Code, or set its path in Settings ▸ Claude."
                    .to_owned(),
                true,
            )),
            Some(info) => info
                .version
                .clone()
                .map(|v| (format!("Claude Code {v}"), false)),
        };

        div()
            .id("welcome")
            .size_full()
            .flex()
            .flex_col()
            .bg(c.background)
            .text_color(c.foreground)
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .child(TitleBar::new())
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .px_8()
                    .pb_6()
                    .child(div().text_2xl().child("Anveg Code"))
                    .when_some(claude_line, |el, (text, warn)| {
                        el.child(
                            div()
                                .text_sm()
                                .text_color(if warn {
                                    c.destructive
                                } else {
                                    c.muted_foreground
                                })
                                .child(text),
                        )
                    })
                    .child(
                        div().flex().child(
                            Button::new("welcome-open")
                                .primary()
                                .label("Open Folder…")
                                .tooltip("⌘O")
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(OpenFolder), cx)
                                }),
                        ),
                    )
                    .child(
                        div()
                            .pt_2()
                            .text_sm()
                            .text_color(c.muted_foreground)
                            .child("Recent projects"),
                    )
                    .child(
                        div()
                            .id("recents")
                            .flex_1()
                            .min_h_0()
                            .overflow_y_scroll()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .when(recents.is_empty(), |el| {
                                el.child(
                                    div()
                                        .text_sm()
                                        .text_color(c.muted_foreground)
                                        .child("None yet."),
                                )
                            })
                            .children(recents.into_iter().map(|(id, name, folder, exists)| {
                                div()
                                    .id(SharedString::from(format!("recent-{id}")))
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|el| el.bg(c.accent.opacity(0.5)))
                                    .on_click(move |_, _, cx| {
                                        crate::windows::open_project(id, None, Vec::new(), cx)
                                    })
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .child(div().text_sm().child(name))
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_xs()
                                                    .text_color(if exists {
                                                        c.muted_foreground
                                                    } else {
                                                        c.destructive
                                                    })
                                                    .child(if exists {
                                                        folder
                                                    } else {
                                                        format!("{folder} (missing)")
                                                    }),
                                            ),
                                    )
                                    .child(
                                        Button::new(SharedString::from(format!("forget-{id}")))
                                            .ghost()
                                            .size(ButtonSize::Sm)
                                            .icon(Icon::from(gpui_kit::assets::IconName::Close))
                                            .tooltip("Remove from list")
                                            .on_click(move |_, _, cx| {
                                                cx.stop_propagation();
                                                let model = model(cx);
                                                model.update(cx, |m, cx| {
                                                    m.update_projects(cx, |p| p.remove(id))
                                                });
                                            }),
                                    )
                            })),
                    ),
            )
    }
}
