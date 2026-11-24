//! The session list on the left (build plan, section 6.2).

use std::ops::Range;

use anveg_core::time::{format_utc, now_millis, relative};
use gpui_cn::prelude::*;
use gpui_cn::{ContextMenu, Input, MenuEntry, MenuItem as CnMenuItem};
use gpui_kit::{
    AnyElement, AppContext as _, ClickEvent, Context, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window, div,
    px, uniform_list,
};

use crate::actions::SESSION_LIST_CONTEXT;
use crate::project_window::{DraggedSession, ProjectWindow, Row};
use crate::ui::{Colors, status_label};

pub const ROW_HEIGHT: f32 = 46.;

/// What is dragged from a row: a small label following the pointer.
pub struct DragPreview(SharedString);

impl gpui_kit::Render for DragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        div()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(c.background)
            .border_1()
            .border_color(c.border)
            .text_sm()
            .text_color(c.foreground)
            .child(self.0.clone())
    }
}

impl ProjectWindow {
    pub fn render_sidebar(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let attention = self.needs_input_count();
        let name = self.project_name(cx);
        let row_count = self.rows.len();
        let header =
            div()
                .flex()
                .items_center()
                .gap_1()
                .px_2()
                .pt_1()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_sm()
                        .text_color(c.foreground)
                        .child(name),
                )
                .when(attention > 0, |el| {
                    el.child(
                        div()
                            .px_1p5()
                            .rounded_full()
                            .bg(c
                                .status(Some(anveg_core::status::SessionStatus::NeedsInput))
                                .unwrap_or(c.accent))
                            .text_xs()
                            .text_color(gpui_kit::black())
                            .child(format!("{attention} need input")),
                    )
                })
                .child(
                    Button::new("new-session")
                        .ghost()
                        .size(ButtonSize::Sm)
                        .icon(Icon::from(gpui_kit::assets::IconName::Plus))
                        .tooltip("New Session (⌘N)")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.begin_new_session(false, window, cx)
                        })),
                )
                .child(
                    Button::new("project-settings")
                        .ghost()
                        .size(ButtonSize::Sm)
                        .icon(Icon::from(gpui_kit::assets::IconName::Settings))
                        .tooltip("Project Settings")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.begin_project_settings(window, cx)
                        })),
                );

        let list: AnyElement = if row_count == 0 {
            let filtering = !self.filter.read(cx).value().trim().is_empty();
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap_2()
                .pt_8()
                .text_sm()
                .text_color(c.muted_foreground)
                .child(if filtering {
                    "No matching sessions"
                } else {
                    "No sessions yet"
                })
                .when(!filtering, |el| {
                    el.child(
                        Button::new("empty-new")
                            .primary()
                            .size(ButtonSize::Sm)
                            .label("New Session")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.begin_new_session(false, window, cx)
                            })),
                    )
                })
                .into_any_element()
        } else {
            uniform_list(
                "sessions",
                row_count,
                cx.processor(|this, range: Range<usize>, window, cx| {
                    this.render_rows(range, window, cx)
                }),
            )
            .track_scroll(&self.list_scroll)
            .flex_1()
            .into_any_element()
        };

        div()
            .id("sidebar")
            .key_context(SESSION_LIST_CONTEXT)
            .track_focus(&self.list_focus)
            .size_full()
            .flex()
            .flex_col()
            .gap_1()
            .bg(c.sidebar)
            .child(header)
            .child(div().px_2().child(Input::new(&self.filter).cleanable(true)))
            .child(div().flex_1().min_h_0().flex().flex_col().child(list))
            .child(
                div()
                    .px_2()
                    .pb_1()
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child("Claude Code deletes sessions older than its cleanupPeriodDays (30 days by default)."),
            )
            .into_any_element()
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let c = Colors::of(cx);
        let now = now_millis();
        let show_memory = self.model.read(cx).settings.show_memory;
        // No row tooltips over an open context menu.
        let menu_open = self.context_menu.read(cx).is_open();
        range
            .filter_map(|ix| {
                let row = self.rows.get(ix)?.clone();
                Some(match row {
                    Row::Header {
                        worktree,
                        count,
                        collapsed,
                    } => {
                        let w = worktree.clone();
                        div()
                            .id(SharedString::from(format!("group-{worktree}")))
                            .h(px(ROW_HEIGHT))
                            .px_2()
                            .flex()
                            .items_end()
                            .pb_1()
                            .gap_1()
                            .cursor_pointer()
                            .text_xs()
                            .text_color(c.muted_foreground)
                            .child(if collapsed { "▸" } else { "▾" })
                            .child(format!("Worktree: {worktree} ({count})"))
                            .on_click(cx.listener(move |this, _, _, cx| this.toggle_group(&w, cx)))
                            .into_any_element()
                    }
                    Row::Session(i) => {
                        let s = self.visible.get(i)?.clone();
                        let status = self.status_of(&s.id);
                        let running = self.is_running(&s.id);
                        let selected = self.is_selected(&s.id);
                        let memory = self
                            .running
                            .get(&s.id)
                            .and_then(|r| r.memory)
                            .filter(|_| show_memory && running);
                        let mut tip = vec![
                            s.display_name.clone(),
                            format!("Session: {}", s.id),
                            format!("Status: {}", status_label(status)),
                        ];
                        if let Some(created) = s.created {
                            tip.push(format!("Created: {}", format_utc(created)));
                        }
                        tip.push(format!("Last activity: {}", format_utc(s.last_activity)));
                        if let Some(branch) = &s.git_branch {
                            tip.push(format!("Branch: {branch}"));
                        }
                        if let Some(w) = &s.worktree {
                            tip.push(format!("Worktree: {w}"));
                        }
                        if let Some(bytes) = memory {
                            tip.push(format!(
                                "Memory: {}",
                                anveg_core::procinfo::format_bytes(bytes)
                            ));
                        }

                        let dot = match c.status(status) {
                            Some(color) => div().size(px(8.)).rounded_full().bg(color),
                            None => div()
                                .size(px(8.))
                                .rounded_full()
                                .border_1()
                                .border_color(c.muted_foreground),
                        };
                        let id_click = s.id.clone();
                        let id_menu = s.id.clone();
                        let drag = DraggedSession {
                            id: s.id.clone(),
                            name: s.display_name.clone().into(),
                        };
                        let this = cx.entity().downgrade();
                        let row = div()
                            .id(SharedString::from(format!("row-{}", s.id)))
                            .h(px(ROW_HEIGHT))
                            .mx_1()
                            .px_2()
                            .rounded_md()
                            .flex()
                            .items_center()
                            .gap_2()
                            .cursor_pointer()
                            .when(selected, |el| el.bg(c.selected_row))
                            .when(!selected, |el| el.hover(|el| el.bg(c.accent.opacity(0.5))))
                            .relative()
                            .when(selected, |el| {
                                el.child(
                                    div()
                                        .absolute()
                                        .left_0()
                                        .top(px(8.))
                                        .bottom(px(8.))
                                        .w(px(3.))
                                        .rounded_full()
                                        .bg(c.selected_bar),
                                )
                            })
                            .child(dot)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .truncate()
                                            .text_sm()
                                            .text_color(c.foreground)
                                            .when(selected, |el| {
                                                el.font_weight(gpui_kit::FontWeight::MEDIUM)
                                            })
                                            .child(s.display_name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(c.muted_foreground)
                                            .child(relative(s.last_activity, now)),
                                    ),
                            )
                            .when(!menu_open, |el| {
                                el.tooltip(move |_, cx| {
                                    crate::ui::LinesTooltip::build(tip.clone(), cx)
                                })
                            })
                            .on_drag(drag, |d, _, _, cx| cx.new(|_| DragPreview(d.name.clone())))
                            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                                let m = event.modifiers();
                                window.focus(&this.list_focus, cx);
                                if m.platform {
                                    this.toggle_select(&id_click, cx);
                                } else if m.shift {
                                    this.range_select(&id_click, cx);
                                } else {
                                    this.click_session(&id_click, window, cx);
                                }
                            }));
                        ContextMenu::new(
                            SharedString::from(format!("menu-{}", s.id)),
                            &self.context_menu,
                        )
                        .items(move |_, cx| {
                            let Some(this) = this.upgrade() else {
                                return Vec::new();
                            };
                            this.read(cx).session_menu(&id_menu, &this)
                        })
                        .child(row)
                        .into_any_element()
                    }
                })
            })
            .collect()
    }

    /// The context menu for a session row.
    pub fn session_menu(&self, id: &str, this: &gpui_kit::Entity<ProjectWindow>) -> Vec<MenuEntry> {
        let targets = self.targets(Some(id));
        let many = targets.len() > 1;
        let any_running = targets.iter().any(|t| self.is_running(t));
        let item = |key: &str, label: &str, f: fn(&mut ProjectWindow, &str, &mut Window, &mut Context<ProjectWindow>)| {
            let this = this.downgrade();
            let id = id.to_owned();
            MenuEntry::Item(CnMenuItem::new(key.to_owned(), label.to_owned()).on_select(move |window, cx| {
                let _ = this.update(cx, |this, cx| f(this, &id, window, cx));
            }))
        };
        let mut entries = Vec::new();
        if !many {
            entries.push(item("open", "Open", |this, id, window, cx| {
                this.click_session(id, window, cx)
            }));
            entries.push(item(
                "open-split",
                "Open in Split View",
                |this, id, window, cx| this.show_in_split(id, window, cx),
            ));
            entries.push(item("rename", "Rename…", |this, id, window, cx| {
                this.begin_rename(id, window, cx)
            }));
            entries.push(item("fork", "Fork Session", |this, id, window, cx| {
                this.fork_session(id, window, cx)
            }));
        }
        if any_running {
            entries.push(item(
                "stop",
                if many { "Stop Selected" } else { "Stop" },
                |this, id, window, cx| {
                    let ids = this.targets(Some(id));
                    this.stop_sessions(&ids, window, cx);
                },
            ));
        }
        entries.push(MenuEntry::Separator);
        entries.push(item(
            "copy-id",
            if many {
                "Copy Session IDs"
            } else {
                "Copy Session ID"
            },
            |this, id, _, cx| this.copy_session_id(Some(id), cx),
        ));
        if !many {
            entries.push(item(
                "reveal",
                "Show Transcript in Finder",
                |this, id, _, cx| this.reveal_transcript(Some(id), cx),
            ));
        }
        entries.push(MenuEntry::Separator);
        let delete_label = if many {
            format!("Delete {} Sessions…", targets.len())
        } else {
            "Delete…".to_owned()
        };
        let this2 = this.downgrade();
        let id2 = id.to_owned();
        entries.push(MenuEntry::Item(
            CnMenuItem::new("delete", delete_label)
                .destructive()
                .on_select(move |_, cx| {
                    let _ = this2.update(cx, |this, cx| this.begin_delete(Some(&id2), cx));
                }),
        ));
        entries
    }
}
