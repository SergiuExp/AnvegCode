//! The window tabs shown across the top of a full-screen project window:
//! one tab per open project window, so you can switch between them
//! without leaving full screen (macOS hides its own window tab bar there
//! until the pointer reaches the top edge). It starts with the window
//! buttons (close, minimize, exit full screen), drawn like macOS's, since
//! the app hides macOS's slide-down title bar in full screen (it covered
//! the app's own top rows; see `platform`).
//!
//! Order: the windows in this window's macOS tab group first, in their tab
//! order, then any other project windows (in another full-screen space, or
//! not full screen), in the order they were opened.

use gpui_cn::prelude::*;
use gpui_kit::assets::IconName;
use gpui_kit::{
    AnyElement, AnyWindowHandle, Context, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    hsla, px, rgb, WeakEntity,
};

use crate::model::model;
use crate::project_window::ProjectWindow;
use crate::ui::Colors;

struct WindowTab {
    handle: AnyWindowHandle,
    view: WeakEntity<ProjectWindow>,
    name: String,
    path: String,
    needs_input: bool,
}

/// Order `windows` (in opening order) with the ids in `group` first, in
/// the group's order.
fn order_by_group<T, K: PartialEq>(windows: Vec<T>, group: &[K], key: impl Fn(&T) -> K) -> Vec<T> {
    let mut rest = windows;
    let mut out = Vec::with_capacity(rest.len());
    for id in group {
        if let Some(i) = rest.iter().position(|w| key(w) == *id) {
            out.push(rest.remove(i));
        }
    }
    out.extend(rest);
    out
}

impl ProjectWindow {
    fn window_tabs(&self, window: &Window, cx: &mut Context<Self>) -> Vec<WindowTab> {
        let me = cx.entity_id();
        let mine = self.needs_input_count() > 0;
        let m = model(cx).read(cx);
        let tabs: Vec<WindowTab> = m
            .windows
            .iter()
            .map(|w| {
                let project = m.projects.get(w.project_id);
                WindowTab {
                    handle: w.handle,
                    view: w.view.clone(),
                    name: project.map(|p| p.name.clone()).unwrap_or_default(),
                    path: project
                        .map(|p| p.main_folder.display().to_string())
                        .unwrap_or_default(),
                    // This window is being rendered: read it directly.
                    needs_input: if w.view.entity_id() == me {
                        mine
                    } else {
                        w.view
                            .upgrade()
                            .is_some_and(|v| v.read(cx).needs_input_count() > 0)
                    },
                }
            })
            .collect();
        let group: Vec<_> = window
            .tabbed_windows()
            .unwrap_or_default()
            .into_iter()
            .map(|t| t.id)
            .collect();
        order_by_group(tabs, &group, |t| t.handle.window_id())
    }

    /// Close, minimize and exit full screen, as macOS draws them: red,
    /// grey (minimize does not apply in full screen, as on macOS) and
    /// green, with their glyphs while the pointer is over them.
    fn window_buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        let hovered = self.window_buttons_hovered;
        let dark = cx.theme().is_dark();
        let glyph_color = hsla(0., 0., 0., 0.6);
        let light = |id: &'static str,
                     color: Hsla,
                     border: Hsla,
                     glyph: Option<IconName>,
                     tip: &'static str| {
            div()
                .id(id)
                .size(px(12.))
                .rounded_full()
                .bg(color)
                .border_1()
                .border_color(border)
                .flex()
                .items_center()
                .justify_center()
                .tooltip(move |_, cx| crate::ui::LinesTooltip::build(vec![tip.to_owned()], cx))
                .when_some(glyph.filter(|_| hovered), |el, glyph| {
                    el.child(
                        Icon::from(glyph)
                            .size(px(8.))
                            .text_color(glyph_color),
                    )
                })
        };
        let grey = if dark {
            (hsla(0., 0., 0.30, 1.), hsla(0., 0., 0.25, 1.))
        } else {
            (hsla(0., 0., 0.82, 1.), hsla(0., 0., 0.74, 1.))
        };
        div()
            .id("window-buttons")
            .h_full()
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(8.))
            .pl(px(8.))
            .pr(px(12.))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if this.window_buttons_hovered != *hovered {
                    this.window_buttons_hovered = *hovered;
                    cx.notify();
                }
            }))
            .child(
                light(
                    "window-close",
                    rgb(0xff5f57).into(),
                    rgb(0xe0443e).into(),
                    Some(IconName::Close),
                    "Close Window",
                )
                .cursor_pointer()
                // As ⌘W (asks about running sessions and unsaved files).
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.should_close(window, cx) {
                        window.remove_window();
                    }
                })),
            )
            .child(light(
                "window-minimize",
                grey.0,
                grey.1,
                None,
                "Minimize is not available in full screen",
            ))
            .child(
                light(
                    "window-fullscreen",
                    rgb(0x28c840).into(),
                    rgb(0x1aab29).into(),
                    Some(IconName::Minimize2),
                    "Exit Full Screen (⌃⌘F)",
                )
                .cursor_pointer()
                .on_click(cx.listener(|_, _, window, _| window.toggle_fullscreen())),
            )
            .into_any_element()
    }

    /// The row, when the window is full screen.
    pub fn render_window_tabs(&self, window: &Window, cx: &mut Context<Self>) -> Option<AnyElement> {
        // Development aid: ANVEG_WINDOW_TABS=1 shows the row outside full
        // screen too, in debug builds (Xvfb has no full screen).
        let forced = cfg!(debug_assertions) && std::env::var_os("ANVEG_WINDOW_TABS").is_some_and(|v| !v.is_empty());
        if !window.is_fullscreen() && !forced {
            return None;
        }
        let c = Colors::of(cx);
        let me = window.window_handle().window_id();
        let attention = Colors::of(cx).status(Some(anveg_core::status::SessionStatus::NeedsInput));
        let mut row = div()
            .id("window-tabs")
            .flex()
            .flex_shrink_0()
            .items_end()
            .h(px(34.))
            .px_1()
            .gap_0p5()
            .bg(c.sidebar)
            .border_b_1()
            .border_color(c.border)
            .overflow_x_scroll();
        row = row.child(self.window_buttons(cx));
        for (i, tab) in self.window_tabs(window, cx).into_iter().enumerate() {
            let current = tab.handle.window_id() == me;
            let handle = tab.handle;
            let close_handle = tab.handle;
            let close_view = tab.view.clone();
            let name = if tab.name.is_empty() {
                "Project".to_owned()
            } else {
                tab.name
            };
            row = row.child(
                div()
                    .id(SharedString::from(format!("wtab-{i}")))
                    .group(SharedString::from(format!("wtab-{i}")))
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .gap_1p5()
                    .h(px(28.))
                    .min_w(px(120.))
                    .max_w(px(260.))
                    .pl_3()
                    .pr_1()
                    .rounded_t_md()
                    .text_sm()
                    .border_1()
                    .border_b_0()
                    .when(current, |el| {
                        el.bg(c.background)
                            .border_color(c.border)
                            .border_t_2()
                            .border_color(c.selected_bar)
                            .text_color(c.foreground)
                    })
                    .when(!current, |el| {
                        el.border_color(gpui_kit::transparent_black())
                            .text_color(c.muted_foreground)
                            .cursor_pointer()
                            .hover(|s| s.bg(c.accent))
                    })
                    .tooltip(move |_, cx| {
                        crate::ui::LinesTooltip::build(vec![tab.path.clone()], cx)
                    })
                    .on_click(cx.listener(move |_, _, _, cx| {
                        if !current {
                            let _ = handle.update(cx, |_, window, _| window.activate_window());
                        }
                    }))
                    .when_some(attention.filter(|_| tab.needs_input), |el, color| {
                        el.child(div().size(px(7.)).rounded_full().bg(color).flex_shrink_0())
                    })
                    .child(div().flex_1().min_w_0().truncate().child(name))
                    .child(
                        Button::new(SharedString::from(format!("wtab-close-{i}")))
                            .ghost()
                            .size(ButtonSize::Xs)
                            .icon(Icon::from(IconName::Close))
                            .tooltip("Close Window")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                cx.stop_propagation();
                                if current {
                                    if this.should_close(window, cx) {
                                        window.remove_window();
                                    }
                                } else {
                                    let _ = close_handle.update(cx, |_, window, cx| {
                                        let _ = close_view.update(cx, |v, cx| {
                                            if v.should_close(window, cx) {
                                                window.remove_window();
                                            }
                                        });
                                    });
                                }
                            })),
                    ),
            );
        }
        Some(row.into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::order_by_group;

    #[test]
    fn group_members_come_first_in_tab_order() {
        let windows = vec![1, 2, 3, 4];
        assert_eq!(order_by_group(windows.clone(), &[3, 1], |w| *w), vec![3, 1, 2, 4]);
        // Ids that are not open (another app window) are skipped.
        assert_eq!(order_by_group(windows.clone(), &[9, 2], |w| *w), vec![2, 1, 3, 4]);
        assert_eq!(order_by_group(windows, &[], |w| *w), vec![1, 2, 3, 4]);
    }
}
