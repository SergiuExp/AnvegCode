//! The right side of a project window and the window's `Render` impl:
//! the tab strip (Claude | Terminal 1 | Terminal 2 | +), the panes, the
//! "Session ended" overlay, the split view, and the action handlers.

use anveg_core::config::PaneContent;
use gpui_cn::TitleBar;
use gpui_cn::prelude::*;
use gpui_kit::base::{h_resizable, resizable_panel};
use gpui_kit::{
    AnyElement, AppContext as _, Context, Focusable as _, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    div, px,
};

use crate::actions::*;
use crate::project_window::{DraggedSession, Ended, ProjectWindow, Tab};

/// Dragging a document tab onto a pane (Phase 4).
#[derive(Clone, Debug)]
pub struct DraggedDoc {
    pub key: anveg_core::config::DocKey,
    pub title: SharedString,
}

impl gpui_kit::Render for DraggedDoc {
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
            .child(self.title.clone())
    }
}
use crate::ui::Colors;

/// What a pane shows.
enum PaneView {
    Session(String),
    Terminal(usize),
    Doc(usize),
}

impl ProjectWindow {
    fn render_tab_strip(&mut self, cx: &mut Context<Self>) -> AnyElement {
        // The tabs scroll sideways when there are many; Split stays put.
        let mut strip = div()
            .id("tab-strip")
            .flex()
            .items_center()
            .gap_1()
            .flex_1()
            .min_w_0()
            .overflow_x_scroll()
            .track_scroll(&self.tab_scroll);
        strip = strip.child(
            Button::new("tab-claude")
                .ghost()
                .size(ButtonSize::Sm)
                .label("Claude")
                .toggled(self.tab == Tab::Claude)
                .tooltip("Claude (⌘T)")
                .on_click(cx.listener(|this, _, window, cx| this.show_claude(window, cx))),
        );
        let many = self.terminals.len() > 1;
        for i in 0..self.terminals.len() {
            let label = match &self.terminals[i].label {
                Some(label) => label.to_string(),
                None if many => format!("Terminal {}", i + 1),
                None => "Terminal".to_owned(),
            };
            let active = self.tab == Tab::Terminal(i);
            strip = strip.child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        Button::new(SharedString::from(format!("tab-term-{i}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .label(label)
                            .toggled(active)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show_terminal(i, window, cx)
                            })),
                    )
                    .when(many || self.terminals[i].view.is_some(), |el| {
                        el.child(
                            Button::new(SharedString::from(format!("tab-term-close-{i}")))
                                .ghost()
                                .size(ButtonSize::Sm)
                                .icon(Icon::from(gpui_kit::assets::IconName::Close))
                                .tooltip("Close Terminal")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.close_terminal(i, window, cx)
                                })),
                        )
                    }),
            );
        }
        strip = strip.child(
            Button::new("tab-new-term")
                .ghost()
                .size(ButtonSize::Sm)
                .icon(Icon::from(gpui_kit::assets::IconName::Plus))
                .tooltip("New Terminal (⌘⇧T)")
                .on_click(cx.listener(|this, _, window, cx| this.new_terminal(window, cx))),
        );
        for i in 0..self.docs.len() {
            let title = self.docs[i].title(cx);
            let dirty = self.docs[i].is_dirty(cx);
            let active = self.tab == Tab::Doc(i);
            let drag = DraggedDoc {
                key: self.docs[i].key.clone(),
                title: title.clone().into(),
            };
            strip = strip.child(
                div()
                    .id(SharedString::from(format!("doc-tab-{i}")))
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .max_w(px(220.))
                    .on_drag(drag, |d, _, _, cx| cx.new(|_| d.clone()))
                    .child(
                        Button::new(SharedString::from(format!("tab-doc-{i}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .label(if dirty { format!("● {title}") } else { title })
                            .toggled(active)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show_doc(i, window, cx)
                            })),
                    )
                    .child(
                        Button::new(SharedString::from(format!("tab-doc-close-{i}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .icon(Icon::from(gpui_kit::assets::IconName::Close))
                            .tooltip("Close Tab (⌘⇧W)")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.close_doc(i, window, cx)
                            })),
                    ),
            );
        }
        let commands = self.render_command_buttons(cx);
        let strip = div().flex().items_center().w_full().min_w_0().child(strip).child(commands).child(
            Button::new("toggle-split")
                .ghost()
                .size(ButtonSize::Sm)
                .label(if self.split.is_some() {
                    "Unsplit"
                } else {
                    "Split"
                })
                .tooltip("Split View (⌘\\)")
                .on_click(cx.listener(|this, _, window, cx| this.toggle_split(window, cx))),
        );
        strip.into_any_element()
    }

    fn main_pane(&self) -> Option<PaneView> {
        match self.tab {
            Tab::Claude => self.selected.clone().map(PaneView::Session),
            Tab::Terminal(i) => Some(PaneView::Terminal(i)),
            Tab::Doc(i) => Some(PaneView::Doc(i)),
        }
    }

    fn render_pane(
        &mut self,
        pane: Option<PaneView>,
        is_split: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = Colors::of(cx);
        let body: AnyElement = match pane {
            None => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(c.muted_foreground)
                .child("Select a session on the left, or start a new one.")
                .child(
                    Button::new("main-new")
                        .primary()
                        .label("New Session")
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.begin_new_session(false, window, cx)
                        })),
                )
                .into_any_element(),
            Some(PaneView::Session(id)) => self.render_session_pane(&id, is_split, cx),
            Some(PaneView::Terminal(i)) => self.render_terminal_pane(i, cx),
            Some(PaneView::Doc(i)) => match self.docs.get(i).and_then(|d| d.view.clone()) {
                Some(view) => view.element(),
                None => div().into_any_element(),
            },
        };
        let header = is_split.then(|| {
            let title = match &self.split {
                Some(PaneContent::Session { session_id }) => self.session_name(session_id),
                Some(PaneContent::Terminal { index }) => format!("Terminal {}", index + 1),
                Some(PaneContent::Doc { doc }) => self
                    .doc_index(doc)
                    .map(|i| self.docs[i].title(cx))
                    .unwrap_or_default(),
                None => String::new(),
            };
            div()
                .h(px(28.))
                .px_2()
                .flex()
                .items_center()
                .gap_1()
                .border_b_1()
                .border_color(c.border)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .text_color(c.muted_foreground)
                        .child(title),
                )
                .child(
                    Button::new("close-split")
                        .ghost()
                        .size(ButtonSize::Sm)
                        .icon(Icon::from(gpui_kit::assets::IconName::Close))
                        .tooltip("Close Split")
                        .on_click(cx.listener(|this, _, window, cx| this.close_split(window, cx))),
                )
        });
        let drop_color = c.primary.opacity(0.15);
        div()
            .id(if is_split { "pane-split" } else { "pane-main" })
            .size_full()
            .flex()
            .flex_col()
            .children(header)
            .child(div().flex_1().min_h_0().relative().child(body))
            .drag_over::<DraggedSession>(move |style, _, _, _| style.bg(drop_color))
            .drag_over::<DraggedDoc>(move |style, _, _, _| style.bg(drop_color))
            .on_drop(cx.listener(move |this, dragged: &DraggedDoc, window, cx| {
                let Some(i) = this.doc_index(&dragged.key) else {
                    return;
                };
                if is_split {
                    this.ensure_doc_view(i, window, cx);
                    this.split = Some(PaneContent::Doc {
                        doc: dragged.key.clone(),
                    });
                    if this.tab == Tab::Doc(i) {
                        this.tab = Tab::Claude;
                    }
                    this.show_current(window, cx);
                } else {
                    this.show_doc(i, window, cx);
                }
            }))
            .on_drop(
                cx.listener(move |this, dragged: &DraggedSession, window, cx| {
                    if is_split {
                        this.show_in_split(&dragged.id, window, cx);
                    } else {
                        this.click_session(&dragged.id, window, cx);
                    }
                }),
            )
            .into_any_element()
    }

    fn render_session_pane(&mut self, id: &str, is_split: bool, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let Some(running) = self.running.get(id) else {
            let looking = self.model.read(cx).claude.is_none();
            let starting = self.starting.contains(id);
            let id = id.to_owned();
            return div()
                .size_full()
                .flex()
                .flex_col()
                .gap_2()
                .items_center()
                .justify_center()
                .text_color(c.muted_foreground)
                .child(if looking {
                    "Looking for Claude Code…"
                } else if starting {
                    "Starting…"
                } else {
                    "This session is not running."
                })
                .when(!looking && !starting, |el| {
                    el.child(
                        Button::new("resume-idle")
                            .primary()
                            .label("Resume")
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.open_session(&id, window, cx)
                            })),
                    )
                })
                .into_any_element();
        };
        let view = running.view.clone();
        let ended = running.ended.clone();
        let toolbar = self.render_session_toolbar(id, is_split, cx);
        let id = id.to_owned();
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(view)
                    .when_some(ended, |el, ended| {
                        el.child(self.ended_overlay(
                            "Session ended",
                            &ended,
                            "Resume",
                            cx.listener(move |this, _, window, cx| {
                                this.open_session(&id, window, cx)
                            }),
                            cx,
                        ))
                    }),
            )
            .into_any_element()
    }

    fn render_terminal_pane(&mut self, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(slot) = self.terminals.get(i) else {
            return div().into_any_element();
        };
        let Some(view) = slot.view.clone() else {
            return div().into_any_element();
        };
        let ended = slot.ended.clone();
        div()
            .size_full()
            .relative()
            .child(view)
            .when_some(ended, |el, ended| {
                el.child(self.ended_overlay(
                    "Process exited",
                    &ended,
                    "Restart",
                    cx.listener(move |this, _, window, cx| this.restart_terminal(i, window, cx)),
                    cx,
                ))
            })
            .into_any_element()
    }

    fn ended_overlay(
        &self,
        title: &str,
        ended: &Ended,
        button: &'static str,
        on_click: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = Colors::of(cx);
        let detail = match ended {
            Ended::Exited(info) => match (&info.code, &info.signal) {
                (Some(0), _) => None,
                (Some(code), _) => Some(format!("Exit code {code}")),
                (None, Some(signal)) => Some(format!("Ended by {signal}")),
                (None, None) => None,
            },
            Ended::Failed(message) => Some(message.to_string()),
            Ended::Stopped => Some("Stopped".to_owned()),
        };
        let title = title.to_owned();
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .bg(c.background.opacity(0.55))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_2()
                    .p_4()
                    .rounded_lg()
                    .border_1()
                    .border_color(c.border)
                    .bg(c.background)
                    .text_color(c.foreground)
                    .child(div().text_lg().child(title))
                    .when_some(detail, |el, d| {
                        el.child(div().text_sm().text_color(c.muted_foreground).child(d))
                    })
                    .child(
                        Button::new("ended-action")
                            .primary()
                            .label(button)
                            .on_click(on_click),
                    ),
            )
            .into_any_element()
    }

    fn render_banner(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.folder_missing(cx) {
            return None;
        }
        let c = Colors::of(cx);
        let folder = self
            .project(cx)
            .map(|p| p.main_folder.display().to_string())
            .unwrap_or_default();
        Some(
            div()
                .px_3()
                .py_2()
                .flex()
                .items_center()
                .gap_2()
                .bg(c.destructive.opacity(0.15))
                .text_sm()
                .text_color(c.foreground)
                .child(
                    div()
                        .flex_1()
                        .child(format!("The project folder {folder} no longer exists.")),
                )
                .child(
                    Button::new("banner-locate")
                        .outline()
                        .size(ButtonSize::Sm)
                        .label("Locate…")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.prompt_locate(window, cx)),
                        ),
                )
                .child(
                    Button::new("banner-remove")
                        .destructive()
                        .size(ButtonSize::Sm)
                        .label("Remove Project")
                        .on_click(
                            cx.listener(|this, _, window, cx| this.remove_project(window, cx)),
                        ),
                )
                .into_any_element(),
        )
    }

    fn prompt_locate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Locate".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = this.update_in(cx, |this, window, cx| this.locate_folder(path, window, cx));
            }
        })
        .detach();
    }

    fn remove_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.project_id;
        self.stop_all(cx);
        self.model
            .update(cx, |m, cx| m.update_projects(cx, |p| p.remove(id)));
        window.remove_window();
    }

    fn render_right(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let main_pane = self.main_pane();
        let main = self.render_pane(main_pane, false, cx);
        let Some(split) = self.split.clone() else {
            return main;
        };
        let split_view = match split {
            PaneContent::Session { session_id } => PaneView::Session(session_id),
            PaneContent::Terminal { index } => PaneView::Terminal(index),
            PaneContent::Doc { doc } => match self.doc_index(&doc) {
                Some(i) => PaneView::Doc(i),
                None => return main,
            },
        };
        let second = self.render_pane(Some(split_view), true, cx);
        let ratio = self.split_ratio;
        h_resizable("split")
            .with_state(&self.split_state)
            .child(
                resizable_panel()
                    .size(px(ratio * 1000.))
                    .size_range(px(200.)..px(4000.))
                    .child(main),
            )
            .child(
                resizable_panel()
                    .size(px((1.0 - ratio) * 1000.))
                    .size_range(px(200.)..px(4000.))
                    .child(second),
            )
            .into_any_element()
    }
}

impl Render for ProjectWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        let sidebar_tabs = self.render_sidebar_tabs(cx);
        let sidebar = self.render_sidebar_body(window, cx);
        let quick_open = self.quick_open.clone();
        let tabs = self.render_tab_strip(cx);
        let banner = self.render_banner(cx);
        let right = self.render_right(cx);
        let right = self.with_notes(right);
        let toolbar = self.render_right_toolbar(cx);
        let dialog = self.render_dialog(window, cx);
        let toast = self.toast.clone();
        let sidebar_width = self.sidebar_width;
        // Full screen: the window tabs run across the top, and the
        // sidebar loses its empty title bar (no window controls there).
        let window_tabs = self.render_window_tabs(window, cx);
        let fullscreen = window_tabs.is_some();

        div()
            .id("project-window")
            .size_full()
            .flex()
            .flex_col()
            .bg(c.background)
            .text_color(c.foreground)
            .on_action(cx.listener(|this, _: &NewSession, window, cx| {
                this.begin_new_session(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &NewSessionInWorktree, window, cx| {
                this.begin_new_session(true, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &NewTerminal, window, cx| this.new_terminal(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleTab, window, cx| this.toggle_tab(window, cx)))
            .on_action(
                cx.listener(|this, _: &ToggleSplit, window, cx| this.toggle_split(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &NextSession, window, cx| this.step_session(1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &PreviousSession, window, cx| {
                this.step_session(-1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusFilter, window, cx| {
                let handle = this.filter.read(cx).focus_handle(cx);
                window.focus(&handle, cx);
            }))
            .on_action(cx.listener(|this, _: &RenameSession, window, cx| {
                if let Some(id) = this.selected.clone() {
                    this.begin_rename(&id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &StopSession, window, cx| {
                let ids = this.targets(None);
                this.stop_sessions(&ids, window, cx);
            }))
            .on_action(cx.listener(|this, _: &DeleteSession, _, cx| this.begin_delete(None, cx)))
            .on_action(cx.listener(|this, _: &ForkSession, window, cx| {
                if let Some(id) = this.selected.clone() {
                    this.fork_session(&id, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &CopySessionId, _, cx| this.copy_session_id(None, cx)))
            .on_action(
                cx.listener(|this, _: &RevealTranscript, _, cx| this.reveal_transcript(None, cx)),
            )
            .on_action(cx.listener(|this, _: &ProjectSettings, window, cx| {
                this.begin_project_settings(window, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| {
                if this.should_close(window, cx) {
                    window.remove_window();
                }
            }))
            .on_action(cx.listener(|_, _: &Minimize, window, _| window.minimize_window()))
            .on_action(cx.listener(|this, _: &QuickOpen, window, cx| this.quick_open(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleNotes, window, cx| this.toggle_notes(window, cx)))
            .on_action(cx.listener(|this, _: &PublishToGitHub, window, cx| {
                this.open_doc(anveg_core::config::DocKey::PublishToGitHub, window, cx)
            }))
            .on_action(cx.listener(|this, _: &EditToolbarCommands, window, cx| {
                this.open_doc(anveg_core::config::DocKey::Commands, window, cx)
            }))
            .on_action(cx.listener(|this, action: &RunProjectCommand, window, cx| {
                let id = this.project(cx).and_then(|p| {
                    p.commands
                        .into_iter()
                        .find(|c| c.shortcut == action.shortcut)
                        .map(|c| c.id)
                });
                match id {
                    Some(id) => this.run_project_command(&id, window, cx),
                    // Not this project's shortcut: let the key through.
                    None => cx.propagate(),
                }
            }))
            .on_action(cx.listener(|this, _: &SaveAll, window, cx| this.save_all(window, cx)))
            .on_action(cx.listener(|this, _: &SaveFile, window, cx| {
                // The editor handles ⌘S when it has focus; this is the menu.
                if let Tab::Doc(i) = this.tab
                    && let Some(crate::docs::DocView::Editor(e)) =
                        this.docs.get(i).and_then(|d| d.view.clone())
                {
                    e.update(cx, |e, cx| e.save(window, cx));
                }
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| match this.tab {
                Tab::Doc(i) => this.close_doc(i, window, cx),
                Tab::Terminal(i) => this.close_terminal(i, window, cx),
                Tab::Claude => {}
            }))
            .on_action(cx.listener(|this, _: &ShowSessions, window, cx| {
                this.show_sidebar_tab(anveg_core::config::SidebarTab::Sessions, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowFiles, window, cx| {
                this.show_sidebar_tab(anveg_core::config::SidebarTab::Files, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowGit, window, cx| {
                this.show_sidebar_tab(anveg_core::config::SidebarTab::Git, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowGitHub, window, cx| {
                this.show_sidebar_tab(anveg_core::config::SidebarTab::GitHub, window, cx)
            }))
            .on_action(cx.listener(|_, _: &Zoom, window, _| window.zoom_window()))
            .on_action(cx.listener(|this, _: &OpenSession1, w, cx| this.open_nth(0, w, cx)))
            .on_action(cx.listener(|this, _: &OpenSession2, w, cx| this.open_nth(1, w, cx)))
            .on_action(cx.listener(|this, _: &OpenSession3, w, cx| this.open_nth(2, w, cx)))
            .on_action(cx.listener(|this, _: &OpenSession4, w, cx| this.open_nth(3, w, cx)))
            .on_action(cx.listener(|this, _: &OpenSession5, w, cx| this.open_nth(4, w, cx)))
            .on_action(cx.listener(|this, _: &OpenSession6, w, cx| this.open_nth(5, w, cx)))
            .on_action(cx.listener(|this, _: &OpenSession7, w, cx| this.open_nth(6, w, cx)))
            .on_action(cx.listener(|this, _: &OpenSession8, w, cx| this.open_nth(7, w, cx)))
            .on_action(cx.listener(|this, _: &OpenSession9, w, cx| this.open_nth(8, w, cx)))
            .children(window_tabs)
            .child(
                h_resizable("sidebar-split")
                    .with_state(&self.sidebar_state)
                    .child(
                        resizable_panel()
                            .size(px(sidebar_width))
                            .size_range(px(180.)..px(520.))
                            .child(
                                div()
                                    .size_full()
                                    .flex()
                                    .flex_col()
                                    .border_r_1()
                                    .border_color(c.border)
                                    .map(|el| {
                                        if fullscreen {
                                            // No window buttons: the tabs go on
                                            // one line with the right side's.
                                            el.child(TitleBar::new().inset(false).child(sidebar_tabs))
                                        } else {
                                            // Room for the traffic lights.
                                            el.child(TitleBar::new()).child(sidebar_tabs)
                                        }
                                    })
                                    .child(div().flex_1().min_h_0().child(sidebar)),
                            ),
                    )
                    .child(
                        resizable_panel().child(
                            div()
                                .size_full()
                                .flex()
                                .flex_col()
                                .child(TitleBar::new().inset(false).child(tabs))
                                .children(banner)
                                .child(
                                    div()
                                        .flex_1()
                                        .min_h_0()
                                        .flex()
                                        .child(div().flex_1().min_w_0().h_full().child(right))
                                        .child(toolbar),
                                ),
                        ),
                    ),
            )
            .when_some(toast, |el, text| {
                el.child(
                    div()
                        .absolute()
                        .bottom_4()
                        .right_4()
                        .px_3()
                        .py_2()
                        .rounded_md()
                        .border_1()
                        .border_color(c.border)
                        .bg(c.background)
                        .text_sm()
                        .child(text),
                )
            })
            .when_some(quick_open, |el, view| {
                el.child(
                    div()
                        .id("quick-open-backdrop")
                        .absolute()
                        .inset_0()
                        .flex()
                        .justify_center()
                        .pt(px(60.))
                        .on_mouse_down(
                            gpui_kit::MouseButton::Left,
                            cx.listener(|this, _, window, cx| this.close_quick_open(window, cx)),
                        )
                        .child(
                            div()
                                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                                    cx.stop_propagation()
                                })
                                .child(view),
                        ),
                )
            })
            .children(dialog)
    }
}
