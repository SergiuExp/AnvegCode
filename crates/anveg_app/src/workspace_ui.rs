//! Window-level glue for Phases 4–6: the sidebar tabs, prompts from panels,
//! unsaved-changes checks, and running commands in a terminal tab.

use std::path::PathBuf;
use std::time::Duration;

use anveg_core::config::SidebarTab;
use gpui_cn::prelude::*;
use gpui_cn::{InputEvent, InputState};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, Focusable as _, InteractiveElement as _,
    IntoElement, ParentElement as _,
    SharedString, Styled as _, Window, div,
};

use anveg_core::notes::SendKind;

use crate::notes_panel::{NotesEvent, NotesPanel};
use crate::files_panel::{FileIndex, FilesPanel, QuickOpen, QuickOpenEvent, build_index};
use crate::git_panel::GitPanel;
use crate::github::GitHubPanel;
use crate::project_window::{DialogKind, ProjectWindow, Tab};
use crate::ui::Colors;
use crate::workspace::{ButtonStyle, Prompt};

impl ProjectWindow {
    /// The project's folders: the main folder, then the extra ones.
    pub fn roots(&self, cx: &App) -> Vec<PathBuf> {
        self.project(cx)
            .map(|p| {
                std::iter::once(p.main_folder)
                    .chain(p.extra_folders)
                    .collect()
            })
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------
    // Sidebar tabs
    // -----------------------------------------------------------------

    pub fn show_sidebar_tab(&mut self, tab: SidebarTab, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_tab = tab;
        match tab {
            SidebarTab::Sessions => {}
            SidebarTab::Files => {
                if self.files_panel.is_none() {
                    let roots = self.roots(cx);
                    let expanded = self.saved_expanded.clone();
                    let store = self.git_store.clone();
                    let panel = cx.new(|cx| FilesPanel::new(roots, expanded, store, window, cx));
                    let sub = cx.subscribe_in(&panel, window, Self::on_workspace_event);
                    self.push_long_subscription(sub);
                    self.files_panel = Some(panel);
                }
            }
            SidebarTab::Git => {
                if self.git_panel.is_none() {
                    let store = self.git_store.clone();
                    let panel = cx.new(|cx| GitPanel::new(store, window, cx));
                    let sub = cx.subscribe_in(&panel, window, Self::on_workspace_event);
                    self.push_long_subscription(sub);
                    self.git_panel = Some(panel);
                }
            }
            SidebarTab::GitHub => {
                if self.github_panel.is_none() {
                    let store = self.git_store.clone();
                    let panel = cx.new(|cx| GitHubPanel::new(store, window, cx));
                    let sub = cx.subscribe_in(&panel, window, Self::on_workspace_event);
                    self.push_long_subscription(sub);
                    self.github_panel = Some(panel);
                }
            }
        }
        self.update_panel_visibility(window, cx);
        self.save_soon(cx);
        cx.notify();
    }

    /// Tell the panels whether they are on screen: only visible panels
    /// watch files or run `git`/`gh` (next phases, 0.5).
    pub fn update_panel_visibility(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.sidebar_tab;
        let active = window.is_window_active();
        let files = tab == SidebarTab::Files;
        let git = tab == SidebarTab::Git;
        let github = tab == SidebarTab::GitHub;
        if let Some(p) = &self.files_panel {
            p.update(cx, |p, cx| p.set_visible(files, cx));
        }
        self.git_store
            .update(cx, |s, cx| s.set_active(files || git || github, cx));
        if let Some(p) = &self.github_panel {
            p.update(cx, |p, cx| p.set_visible(github, active, cx));
        }
    }

    /// The window gained focus: refresh what is visible, re-check `gh`.
    pub fn window_focused(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let model = self.model.clone();
        cx.defer(move |cx| model.update(cx, |m, cx| m.check_gh(false, cx)));
        self.git_store.update(cx, |s, cx| s.refresh_if_active(cx));
        self.update_panel_visibility(window, cx);
    }

    pub fn git_changed(&mut self, cx: &mut Context<Self>) {
        self.git_store.update(cx, |s, cx| s.refresh(cx));
    }

    /// Status was re-read (something changed on disk or in git): open git
    /// diff tabs show the new state too.
    pub fn on_git_store_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reload_git_docs(window, cx);
        cx.notify();
    }

    pub fn render_sidebar_tabs(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.sidebar_tab;
        let tabs = [
            (SidebarTab::Sessions, "Sessions"),
            (SidebarTab::Files, "Files"),
            (SidebarTab::Git, "Git"),
            (SidebarTab::GitHub, "GitHub"),
        ];
        let mut row = div().flex().items_center().gap_0p5().px_1();
        for (tab, label) in tabs {
            row = row.child(
                Button::new(SharedString::from(format!("side-{label}")))
                    .ghost()
                    .size(ButtonSize::Sm)
                    .label(label)
                    .toggled(current == tab)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.show_sidebar_tab(tab, window, cx)
                    })),
            );
        }
        row.into_any_element()
    }

    pub fn render_sidebar_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.sidebar_tab {
            SidebarTab::Sessions => self.render_sidebar(window, cx),
            SidebarTab::Files => self
                .files_panel
                .clone()
                .map(IntoElement::into_any_element)
                .unwrap_or_else(|| div().into_any_element()),
            SidebarTab::Git => self
                .git_panel
                .clone()
                .map(IntoElement::into_any_element)
                .unwrap_or_else(|| div().into_any_element()),
            SidebarTab::GitHub => self
                .github_panel
                .clone()
                .map(IntoElement::into_any_element)
                .unwrap_or_else(|| div().into_any_element()),
        }
    }

    // -----------------------------------------------------------------
    // Notes
    // -----------------------------------------------------------------

    /// Show or hide the Notes panel (toolbar button, ⌘⌥N).
    pub fn toggle_notes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.notes_open = !self.notes_open;
        if self.notes_open && self.notes_panel.is_none() {
            let path = self
                .model
                .read(cx)
                .paths
                .notes_file(&self.project_id.to_string());
            let panel = cx.new(|cx| NotesPanel::new(path, cx));
            let sub = cx.subscribe_in(&panel, window, Self::on_notes_event);
            self.push_long_subscription(sub);
            let sub = cx.subscribe_in(&panel, window, Self::on_workspace_event);
            self.push_long_subscription(sub);
            self.notes_panel = Some(panel);
        }
        self.save_soon(cx);
        cx.notify();
    }

    fn on_notes_event(
        &mut self,
        panel: &Entity<NotesPanel>,
        event: &NotesEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event.clone() {
            NotesEvent::Send { note, text } => {
                let target = self
                    .selected
                    .clone()
                    .filter(|id| self.running.get(id).is_some_and(|r| r.is_live()));
                let Some(session) = target else {
                    self.toast(
                        "Select a running Claude session first (or use New Session).",
                        cx,
                    );
                    return;
                };
                let Some(view) = self.running.get(&session).map(|r| r.view.clone()) else {
                    return;
                };
                // A paste, then Enter a moment later, so the Enter is not
                // taken as part of the paste.
                view.update(cx, |v, cx| v.paste_text(text, cx));
                cx.spawn(async move |_, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(150))
                        .await;
                    view.update(cx, |v, cx| v.send_text("\r", cx));
                })
                .detach();
                let name = self.session_name(&session);
                panel.update(cx, |p, cx| {
                    p.mark_sent(&note, &session, &name, SendKind::Paste, cx)
                });
                self.tab = Tab::Claude;
                self.show_current(window, cx);
            }
            NotesEvent::NewSession { note, text } => {
                if let Some(session) = self.start_new_session(None, None, Some(text.clone()), window, cx) {
                    let name: String = text
                        .lines()
                        .find(|l| !l.trim().is_empty())
                        .unwrap_or("")
                        .trim()
                        .chars()
                        .take(80)
                        .collect();
                    panel.update(cx, |p, cx| {
                        p.mark_sent(&note, &session, &name, SendKind::NewSession, cx)
                    });
                }
            }
        }
    }

    /// The narrow toolbar on the right edge of the window.
    pub fn render_right_toolbar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        div()
            .w(gpui_kit::px(36.))
            .h_full()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .pt_1()
            .border_l_1()
            .border_color(c.border)
            .child(
                Button::new("toolbar-notes")
                    .ghost()
                    .size(ButtonSize::Sm)
                    .icon(Icon::from(gpui_kit::assets::IconName::FileText))
                    .toggled(self.notes_open)
                    .tooltip(if self.notes_open {
                        "Hide Notes (⌘⌥N)"
                    } else {
                        "Show Notes (⌘⌥N)"
                    })
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_notes(window, cx))),
            )
            .into_any_element()
    }

    /// The right side's content, with the Notes panel beside it when open.
    pub fn with_notes(&mut self, content: AnyElement) -> AnyElement {
        let Some(panel) = self.notes_panel.clone().filter(|_| self.notes_open) else {
            return content;
        };
        gpui_kit::base::h_resizable("notes-split")
            .with_state(&self.notes_state)
            .child(gpui_kit::base::resizable_panel().child(content))
            .child(
                gpui_kit::base::resizable_panel()
                    .size(gpui_kit::px(self.notes_width))
                    .size_range(gpui_kit::px(240.)..gpui_kit::px(900.))
                    .child(panel),
            )
            .into_any_element()
    }

    // -----------------------------------------------------------------
    // Quick open (⌘P)
    // -----------------------------------------------------------------

    pub fn quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let roots = self.roots(cx);
        let index = self.file_index.take();
        let view = cx.new(|cx| QuickOpen::new(roots.clone(), index.as_ref(), window, cx));
        let needs_index = index.as_ref().is_none_or(|i| i.is_stale());
        self.file_index = index;
        let sub = cx.subscribe_in(&view, window, |this, _, event: &QuickOpenEvent, window, cx| {
            let open = match event {
                QuickOpenEvent::Open(path) => Some(path.clone()),
                QuickOpenEvent::Dismiss => None,
            };
            this.close_quick_open(window, cx);
            if let Some(path) = open {
                this.open_file(path, None, None, window, cx);
            }
        });
        self.push_long_subscription(sub);
        let handle = view.read(cx).focus_handle(cx);
        self.quick_open = Some(view);
        window.focus(&handle, cx);
        if needs_index {
            let task = build_index(roots.clone(), cx);
            cx.spawn_in(window, async move |this, cx| {
                let (files, labels) = task.await;
                let _ = this.update(cx, |this, cx| {
                    let index = FileIndex::new(files, labels, &roots, cx);
                    if let Some(view) = &this.quick_open {
                        view.update(cx, |v, cx| v.set_index(&index, cx));
                    }
                    this.file_index = Some(index);
                });
            })
            .detach();
        }
        cx.notify();
    }

    pub fn close_quick_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.quick_open.take().is_some() {
            self.show_current(window, cx);
        }
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Prompts
    // -----------------------------------------------------------------

    pub fn show_prompt(&mut self, prompt: Prompt, window: &mut Window, cx: &mut Context<Self>) {
        let input = prompt.input.clone().map(|(placeholder, value)| {
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(placeholder)
                    .default_value(value)
            });
            let sub = cx.subscribe_in(&input, window, |this, _, event, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    this.confirm_prompt(None, window, cx);
                }
            });
            self.push_subscription(sub);
            input
        });
        if let Some(input) = &input {
            let handle = input.read(cx).focus_handle(cx);
            window.focus(&handle, cx);
        }
        self.dialog = Some(DialogKind::Prompt {
            prompt,
            input,
            error: None,
        });
        cx.notify();
    }

    /// Run button `index` (or the first primary one for Enter).
    pub fn confirm_prompt(&mut self, index: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(DialogKind::Prompt { prompt, input, .. }) = &self.dialog else {
            return;
        };
        let index = index.or_else(|| {
            prompt
                .buttons
                .iter()
                .position(|b| b.style == ButtonStyle::Primary)
        });
        let Some(button) = index.and_then(|i| prompt.buttons.get(i)).cloned() else {
            return;
        };
        tracing::debug!(button = %button.label, "prompt button");
        let value = input.as_ref().map(|i| i.read(cx).value().to_string());
        let saved = Some((prompt.clone(), input.clone()));
        let Some(action) = button.action else {
            self.close_dialog(window, cx);
            return;
        };
        // Close first: the action may open another prompt. The input's
        // subscription stays until the next dialog, so a retry still works.
        self.dialog = None;
        let weak = cx.entity().downgrade();
        cx.defer_in(window, move |_, window, cx| {
            if let Err(message) = action(value, window, cx) {
                let _ = weak.update(cx, |this, cx| {
                    if let Some((prompt, input)) = saved {
                        this.dialog = Some(DialogKind::Prompt {
                            prompt,
                            input,
                            error: Some(message.into()),
                        });
                        cx.notify();
                    }
                });
            }
        });
        cx.notify();
    }

    // -----------------------------------------------------------------
    // Unsaved files
    // -----------------------------------------------------------------

    pub fn has_unsaved(&self, cx: &App) -> bool {
        !self.unsaved_ok && !self.dirty_doc_names(cx).is_empty()
    }

    /// When files have unsaved changes, ask (Save All / Don't Save /
    /// Cancel) and return true. Then the close or quit starts again.
    pub fn ask_unsaved(&mut self, quit: bool, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if !self.has_unsaved(cx) {
            return false;
        }
        let names = self.dirty_doc_names(cx);
        let weak = cx.entity().downgrade();
        let weak2 = weak.clone();
        let proceed = move |this: &mut ProjectWindow, window: &mut Window, cx: &mut Context<ProjectWindow>| {
            this.unsaved_ok = true;
            if quit {
                cx.defer(crate::request_quit);
            } else if this.should_close(window, cx) {
                window.remove_window();
            }
        };
        let proceed2 = proceed;
        let prompt = Prompt::new(
            format!(
                "Save changes to {} file{}?",
                names.len(),
                if names.len() == 1 { "" } else { "s" }
            ),
            names.join(", "),
        )
        .cancel()
        .button("Don't Save", ButtonStyle::Destructive, move |_, window, cx| {
            let _ = weak.update(cx, |this, cx| proceed(this, window, cx));
            Ok(())
        })
        .button("Save All", ButtonStyle::Primary, move |_, window, cx| {
            let _ = weak2.update(cx, |this, cx| {
                this.save_all(window, cx);
                // Saving is asynchronous; give it a moment before closing.
                cx.spawn_in(window, async move |this, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(400))
                        .await;
                    let _ = this.update_in(cx, |this, window, cx| proceed2(this, window, cx));
                })
                .detach();
            });
            Ok(())
        });
        self.show_prompt(prompt, window, cx);
        true
    }

    // -----------------------------------------------------------------
    // Terminals
    // -----------------------------------------------------------------

    /// ⌘-click on `path:line` in a terminal: open it in the editor.
    pub fn open_link(
        &mut self,
        link: &anveg_core::files::PathLink,
        cwd: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut roots: Vec<PathBuf> = cwd.into_iter().collect();
        roots.extend(self.roots(cx));
        let home = std::env::var_os("HOME").map(PathBuf::from);
        match anveg_core::files::resolve_link(link, &roots, home.as_deref()) {
            Some(path) => self.open_file(path, link.line, link.column, window, cx),
            None => self.toast(format!("No file named {}", link.path), cx),
        }
    }

    /// Run `args` (shell-quoted) in a new terminal tab.
    pub fn run_in_terminal(&mut self, args: Vec<String>, window: &mut Window, cx: &mut Context<Self>) {
        self.new_terminal(window, cx);
        let Tab::Terminal(i) = self.tab else {
            return;
        };
        let Some(view) = self.terminals.get(i).and_then(|t| t.view.clone()) else {
            return;
        };
        let line = shell_words::join(&args);
        cx.spawn(async move |_, cx| {
            // Let the shell start and print its prompt.
            cx.background_executor()
                .timer(Duration::from_millis(500))
                .await;
            view.update(cx, |v, cx| v.send_text(format!("{line}\r"), cx));
        })
        .detach();
    }

    pub fn render_prompt(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let c = Colors::of(cx);
        let Some(DialogKind::Prompt {
            prompt,
            input,
            error,
        }) = &self.dialog
        else {
            return None;
        };
        let weak = cx.entity().downgrade();
        let mut footer = div().flex().gap_2();
        for (i, b) in prompt.buttons.iter().enumerate() {
            let mut button = Button::new(SharedString::from(format!("prompt-{i}"))).label(b.label.clone());
            button = match b.style {
                ButtonStyle::Primary => button.primary(),
                ButtonStyle::Destructive => button.destructive(),
                ButtonStyle::Outline => button.outline(),
            };
            footer = footer.child(button.on_click(cx.listener(move |this, _, window, cx| {
                this.confirm_prompt(Some(i), window, cx)
            })));
        }
        let mut dialog = gpui_cn::Dialog::new("dlg-prompt")
            .open(true)
            .on_open_change(move |open, window, cx| {
                if !open {
                    let _ = weak.update(cx, |this, cx| this.close_dialog(window, cx));
                }
            })
            .title(prompt.title.clone());
        if !prompt.body.is_empty() {
            dialog = dialog.description(prompt.body.clone());
        }
        if let Some(input) = input {
            dialog = dialog.track_focus(&input.read(cx).focus_handle(cx));
        }
        let form = div()
            .flex()
            .flex_col()
            .gap_2()
            .on_action(cx.listener(|this, _: &gpui_kit::base::actions::Confirm, window, cx| {
                this.confirm_prompt(None, window, cx)
            }))
            .children(input.as_ref().map(gpui_cn::Input::new))
            .children(
                error
                    .clone()
                    .map(|e| div().text_sm().text_color(c.destructive).child(e)),
            );
        Some(dialog.child(form).footer(footer).into_any_element())
    }
}
