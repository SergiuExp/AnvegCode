//! The project tabs across the top of an app window: one tab per project
//! in the window (name, the folder in its tooltip, an amber dot when one
//! of its sessions needs input, × to close it), a + to open another, and
//! the window's name at the left when it has one or other windows exist.
//! Right-click a tab to move it to another window or rename the window.
//!
//! Windowed, the row is the window's title bar: macOS draws the window
//! buttons at its left and dragging it moves the window. In full screen
//! the row starts with the window buttons drawn like macOS's, since the
//! app hides macOS's slide-down title bar there (it covered the app's own
//! top rows; see `platform`).

use gpui_cn::prelude::*;
use gpui_cn::{ContextMenu, MenuEntry, MenuItem as CnMenuItem, MenuSubmenu, TitleBar};
use gpui_kit::assets::IconName;
use gpui_kit::{
    AnyElement, Context, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, Window, div, hsla, px, rgb,
};
use uuid::Uuid;

use crate::app_window::AppWindow;
use crate::model::model;
use crate::ui::Colors;
use crate::windows::MoveTarget;

impl AppWindow {
    /// Close, minimize and exit full screen, as macOS draws them: red,
    /// grey (minimize does not apply in full screen, as on macOS) and
    /// green, with their glyphs while the pointer is over them.
    fn window_buttons(&self, cx: &mut Context<Self>) -> AnyElement {
        let hovered = self.buttons_hovered;
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
                    el.child(Icon::from(glyph).size(px(8.)).text_color(glyph_color))
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
                if this.buttons_hovered != *hovered {
                    this.buttons_hovered = *hovered;
                    cx.notify();
                }
            }))
            .child(
                light(
                    "window-close",
                    rgb(0xff5f57).into(),
                    rgb(0xe0443e).into(),
                    Some(IconName::Close),
                    "Close Window (⌥⌘W)",
                )
                .cursor_pointer()
                // Asks about running sessions and unsaved files first.
                .on_click(cx.listener(|this, _, window, cx| {
                    if this.close_all(window, cx) {
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

    /// The tab's context menu.
    fn tab_menu_entries(&self, project_id: Uuid, this: &Entity<AppWindow>, cx: &gpui_kit::App) -> Vec<MenuEntry> {
        let move_item = |item: CnMenuItem, to: MoveTarget| {
            MenuEntry::Item(item.on_select(move |_, cx| {
                cx.defer(move |cx| crate::windows::move_project(project_id, to, cx));
            }))
        };
        let mut entries = Vec::new();
        if self.projects.len() > 1 {
            entries.push(move_item(
                CnMenuItem::new("move-new", "Move to New Window"),
                MoveTarget::NewWindow,
            ));
        }
        let others: Vec<MenuEntry> = model(cx)
            .read(cx)
            .app_windows
            .iter()
            .filter(|w| w.id != self.id)
            .filter_map(|w| {
                let view = w.view.upgrade()?;
                let v = view.read(cx);
                let mut item = CnMenuItem::new(format!("move-{}", w.id), v.display_name(cx));
                if v.name.is_some() {
                    // A named window: which projects it holds.
                    item = item.description(v.project_names(cx).join(", "));
                }
                Some(move_item(item, MoveTarget::Window(w.id)))
            })
            .collect();
        if !others.is_empty() {
            entries.push(MenuEntry::Submenu(
                MenuSubmenu::new("move-to", "Move to Window").entries(others),
            ));
        }
        if !entries.is_empty() {
            entries.push(MenuEntry::Separator);
        }
        let weak = this.downgrade();
        entries.push(MenuEntry::Item(
            CnMenuItem::new("rename", "Rename Window…").on_select(move |window, cx| {
                let _ = weak.update(cx, |this, cx| this.begin_rename(window, cx));
            }),
        ));
        let weak = this.downgrade();
        entries.push(MenuEntry::Item(
            CnMenuItem::new("close", "Close Project").on_select(move |window, cx| {
                let _ = weak.update(cx, |this, cx| {
                    if let Some(i) = this.index_of(project_id, cx) {
                        this.close_project(i, window, cx);
                    }
                });
            }),
        ));
        entries
    }

    /// The row of project tabs.
    pub fn render_tabs_row(&self, window: &Window, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        // Development aid: ANVEG_FULLSCREEN_ROW=1 draws the full-screen row
        // in a normal window too, in debug builds.
        let forced = cfg!(debug_assertions)
            && std::env::var_os("ANVEG_FULLSCREEN_ROW").is_some_and(|v| !v.is_empty());
        let fullscreen = window.is_fullscreen() || forced;
        let attention = c.status(Some(anveg_core::status::SessionStatus::NeedsInput));
        let menu_open = self.tab_menu.read(cx).is_open();
        let this = cx.entity();
        let several_windows = model(cx).read(cx).app_windows.len() > 1;
        let mut tabs = div()
            .id("project-tabs")
            .h_full()
            .flex_1()
            .min_w_0()
            .flex()
            .items_end()
            .gap_0p5()
            .overflow_x_scroll();
        if self.name.is_some() || several_windows {
            tabs = tabs.child(
                div()
                    .flex_shrink_0()
                    .self_center()
                    .max_w(px(200.))
                    .truncate()
                    .pr_2()
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(c.muted_foreground)
                    .child(self.display_name(cx)),
            );
        }
        for (i, view) in self.projects.iter().enumerate() {
            let current = i == self.active;
            let (project_id, name, path, needs_input) = {
                let p = view.read(cx);
                (
                    p.project_id,
                    p.project_name(cx),
                    p.project(cx)
                        .map(|p| p.main_folder.display().to_string())
                        .unwrap_or_default(),
                    p.needs_input_count() > 0,
                )
            };
            let tab = div()
                .id(SharedString::from(format!("ptab-{i}")))
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
                .when(!menu_open, |el| {
                    el.tooltip(move |_, cx| crate::ui::LinesTooltip::build(vec![path.clone()], cx))
                })
                // Its own press: a press on the bar around the tabs moves
                // the window.
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, window, _| {
                    window.prevent_default()
                })
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.closing = false;
                    this.activate(i, window, cx);
                }))
                .when_some(attention.filter(|_| needs_input), |el, color| {
                    el.child(div().size(px(7.)).rounded_full().bg(color).flex_shrink_0())
                })
                .child(div().flex_1().min_w_0().truncate().child(name))
                .child(
                    Button::new(SharedString::from(format!("ptab-close-{i}")))
                        .ghost()
                        .size(ButtonSize::Xs)
                        .icon(Icon::from(IconName::Close))
                        .tooltip("Close Project (⌘W)")
                        .on_click(cx.listener(move |this, _, window, cx| {
                            cx.stop_propagation();
                            this.close_project(i, window, cx);
                        })),
                );
            let menu_this = this.clone();
            tabs = tabs.child(
                ContextMenu::new(SharedString::from(format!("ptab-menu-{i}")), &self.tab_menu)
                    .items(move |_, cx| {
                        let host = menu_this.clone();
                        host.read(cx).tab_menu_entries(project_id, &host, cx)
                    })
                    .child(tab),
            );
        }
        tabs = tabs.child(
            div().self_center().flex_shrink_0().pl_1().child(
                Button::new("ptab-open")
                    .ghost()
                    .size(ButtonSize::Xs)
                    .icon(Icon::from(IconName::Plus))
                    .tooltip("Open Project… (⌘O)")
                    .on_click(|_, _, cx| crate::windows::prompt_open_folder(cx)),
            ),
        );

        let row = div()
            .flex_shrink_0()
            .w_full()
            .bg(c.sidebar)
            .border_b_1()
            .border_color(c.border);
        if fullscreen {
            row.id("project-tabs-row")
                .flex()
                .items_end()
                .h(px(34.))
                .px_1()
                .child(self.window_buttons(cx))
                .child(tabs)
                .into_any_element()
        } else {
            // Room for macOS's window buttons, and dragging moves the
            // window.
            row.child(TitleBar::new().child(tabs)).into_any_element()
        }
    }
}
