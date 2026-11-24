//! The Notes panel: plain-text notes for the project, opened from the
//! toolbar on the right edge of the window.
//!
//! Tabs: **Notes** (the default), any number of the user's own tabs, and
//! **Archive**. Each note has two buttons: **Send** pastes it into the
//! selected Claude session and presses Enter; **New Session** starts a
//! session with the note as its first message. Either way the note moves
//! to the Archive. Saved in the app's data folder (never in the project),
//! see `anveg_core::notes`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use anveg_core::notes::{self, NoteTab, NotesFile, SendKind, TabKind};
use anveg_core::time::{now_millis, relative};
use gpui_cn::prelude::*;
use gpui_cn::{InputEvent, Textarea, TextareaState};
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, px,
};

use crate::ui::Colors;
use crate::workspace::{ButtonStyle, Prompt, WorkspaceEvent};

/// What the panel asks its window to do with a note.
#[derive(Clone, Debug)]
pub enum NotesEvent {
    /// Paste into the selected session and press Enter.
    Send { note: String, text: String },
    /// Start a new session with this as its first message.
    NewSession { note: String, text: String },
}

/// Save this long after the last keystroke.
const SAVE_DELAY: Duration = Duration::from_millis(400);

pub struct NotesPanel {
    path: PathBuf,
    file: NotesFile,
    active: String,
    editors: HashMap<String, Entity<TextareaState>>,
    editor_subs: HashMap<String, Subscription>,
    /// The empty note at the end of the shown tab: (tab, editor). It is
    /// not stored; typing into it makes it a note and a new empty one
    /// appears below.
    draft: Option<(String, Entity<TextareaState>)>,
    _draft_sub: Option<Subscription>,
    dirty: bool,
    save_task: Option<Task<()>>,
    focus: FocusHandle,
}

impl EventEmitter<NotesEvent> for NotesPanel {}
impl EventEmitter<WorkspaceEvent> for NotesPanel {}

impl Focusable for NotesPanel {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl NotesPanel {
    pub fn new(path: PathBuf, cx: &mut Context<Self>) -> Self {
        let file = NotesFile::load(&path);
        let active = file.notes_tab().to_owned();
        // Whatever is typed last is saved when the panel goes away.
        cx.on_release(|this: &mut Self, _| this.flush()).detach();
        Self {
            path,
            file,
            active,
            editors: HashMap::new(),
            editor_subs: HashMap::new(),
            draft: None,
            _draft_sub: None,
            dirty: false,
            save_task: None,
            focus: cx.focus_handle(),
        }
    }

    /// Write pending changes now (window closing, app quitting).
    pub fn flush(&mut self) {
        if self.dirty {
            self.dirty = false;
            self.save_task = None;
            if let Err(error) = self.file.save(&self.path) {
                tracing::error!(%error, "could not save notes");
            }
        }
    }

    fn changed(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        let file = self.file.clone();
        let path = self.path.clone();
        self.save_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SAVE_DELAY).await;
            let saved = cx
                .background_spawn(async move { file.save(&path) })
                .await;
            let _ = this.update(cx, |this, _| match saved {
                Ok(()) => this.dirty = false,
                Err(error) => tracing::error!(%error, "could not save notes"),
            });
        }));
        cx.notify();
    }

    /// The window sent a note (or started a session with it).
    pub fn mark_sent(
        &mut self,
        note: &str,
        session_id: &str,
        session_name: &str,
        how: SendKind,
        cx: &mut Context<Self>,
    ) {
        self.file.mark_sent(note, session_id, session_name, how);
        self.drop_editor(note);
        self.changed(cx);
    }

    fn drop_editor(&mut self, note: &str) {
        self.editors.remove(note);
        self.editor_subs.remove(note);
    }

    fn is_archive(&self) -> bool {
        self.active == self.file.archive_tab()
    }

    fn text_of(&self, note: &str, cx: &App) -> String {
        match self.editors.get(note) {
            Some(e) => e.read(cx).value().to_string(),
            None => self.file.note(note).map(|n| n.text.clone()).unwrap_or_default(),
        }
    }

    // -----------------------------------------------------------------
    // Notes
    // -----------------------------------------------------------------

    /// Typing into the empty note: it becomes a note in the tab (keeping
    /// its editor, so focus and the cursor stay), and a new empty note is
    /// made on the next render.
    fn promote_draft(&mut self, cx: &mut Context<Self>) {
        let Some((tab, editor)) = self.draft.clone() else {
            return;
        };
        let text = editor.read(cx).value().to_string();
        if text.trim().is_empty() {
            return;
        }
        let id = self.file.add_note(&tab);
        self.file.set_text(&id, &text);
        let nid = id.clone();
        let sub = cx.subscribe(&editor, move |this, editor, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                let value = editor.read(cx).value().to_string();
                if this.file.set_text(&nid, &value) {
                    this.changed(cx);
                }
            }
        });
        self.editors.insert(id.clone(), editor);
        self.editor_subs.insert(id, sub);
        self.draft = None;
        self._draft_sub = None;
        self.changed(cx);
    }

    fn send(&mut self, note: &str, new_session: bool, cx: &mut Context<Self>) {
        let text = self.text_of(note, cx);
        if text.trim().is_empty() {
            return;
        }
        // Keep what was typed even if the send fails.
        if self.file.set_text(note, &text) {
            self.changed(cx);
        }
        let note = note.to_owned();
        cx.emit(if new_session {
            NotesEvent::NewSession { note, text }
        } else {
            NotesEvent::Send { note, text }
        });
    }

    fn delete(&mut self, note: &str, cx: &mut Context<Self>) {
        let text = self.text_of(note, cx);
        if text.trim().is_empty() {
            self.file.delete_note(note);
            self.drop_editor(note);
            self.changed(cx);
            return;
        }
        let weak = cx.entity().downgrade();
        let id = note.to_owned();
        let first = text.lines().next().unwrap_or("").chars().take(60).collect::<String>();
        let prompt = Prompt::new("Delete this note?", format!("“{first}”"))
            .cancel()
            .button("Delete", ButtonStyle::Destructive, move |_, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.file.delete_note(&id);
                    this.drop_editor(&id);
                    this.changed(cx);
                });
                Ok(())
            });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn move_note(&mut self, note: &str, cx: &mut Context<Self>) {
        let text = self.text_of(note, cx);
        self.file.set_text(note, &text);
        let targets: Vec<NoteTab> = self
            .file
            .tabs
            .iter()
            .filter(|t| t.kind != TabKind::Archive && t.id != self.active)
            .cloned()
            .collect();
        let mut prompt = Prompt::new("Move to…", "Choose the tab for this note.").cancel();
        for tab in targets {
            let weak = cx.entity().downgrade();
            let id = note.to_owned();
            prompt = prompt.button(tab.name.clone(), ButtonStyle::Outline, move |_, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.file.move_note(&id, &tab.id);
                    this.drop_editor(&id);
                    this.changed(cx);
                });
                Ok(())
            });
        }
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn restore(&mut self, note: &str, cx: &mut Context<Self>) {
        self.file.restore(note);
        self.changed(cx);
    }

    // -----------------------------------------------------------------
    // Tabs
    // -----------------------------------------------------------------

    fn add_tab(&mut self, cx: &mut Context<Self>) {
        let weak = cx.entity().downgrade();
        let prompt = Prompt::new("New Tab", "A tab for a group of notes, for example a milestone.")
            .input("Name", "")
            .cancel()
            .button("Create", ButtonStyle::Primary, move |value, _, cx| {
                let value = value.unwrap_or_default();
                let name = notes::validate_tab_name(&value)?.to_owned();
                let _ = weak.update(cx, |this, cx| {
                    this.active = this.file.add_tab(&name);
                    this.changed(cx);
                });
                Ok(())
            });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn rename_tab(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.file.tab(&self.active).cloned() else {
            return;
        };
        let weak = cx.entity().downgrade();
        let prompt = Prompt::new("Rename Tab", "")
            .input("Name", tab.name.clone())
            .cancel()
            .button("Rename", ButtonStyle::Primary, move |value, _, cx| {
                let value = value.unwrap_or_default();
                let name = notes::validate_tab_name(&value)?.to_owned();
                let _ = weak.update(cx, |this, cx| {
                    this.file.rename_tab(&tab.id, &name);
                    this.changed(cx);
                });
                Ok(())
            });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    fn delete_tab(&mut self, cx: &mut Context<Self>) {
        let Some(tab) = self.file.tab(&self.active).cloned() else {
            return;
        };
        let count = self.file.count_in(&tab.id);
        let weak = cx.entity().downgrade();
        let body = if count == 0 {
            "The tab is empty.".to_owned()
        } else {
            format!(
                "Its {count} note{} move{} to Notes.",
                if count == 1 { "" } else { "s" },
                if count == 1 { "s" } else { "" }
            )
        };
        let prompt = Prompt::new(format!("Delete the tab “{}”?", tab.name), body)
            .cancel()
            .button("Delete Tab", ButtonStyle::Destructive, move |_, _, cx| {
                let _ = weak.update(cx, |this, cx| {
                    this.file.delete_tab(&tab.id);
                    this.active = this.file.notes_tab().to_owned();
                    this.changed(cx);
                });
                Ok(())
            });
        cx.emit(WorkspaceEvent::Prompt(prompt));
    }

    // -----------------------------------------------------------------
    // Rendering
    // -----------------------------------------------------------------

    /// Editors exist only for the notes of the shown tab.
    fn ensure_editors(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.is_archive() {
            return;
        }
        let ids: Vec<(String, String)> = self
            .file
            .notes_in(&self.active)
            .into_iter()
            .map(|n| (n.id.clone(), n.text.clone()))
            .collect();
        for (id, text) in ids {
            if self.editors.contains_key(&id) {
                continue;
            }
            let editor = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("Write a note…")
                    .auto_grow(2, 14)
                    .default_value(text)
            });
            let nid = id.clone();
            let sub = cx.subscribe(&editor, move |this, editor, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let value = editor.read(cx).value().to_string();
                    if this.file.set_text(&nid, &value) {
                        this.changed(cx);
                    }
                }
            });
            self.editors.insert(id.clone(), editor);
            self.editor_subs.insert(id, sub);
        }
        // The empty note at the end of this tab.
        if self.draft.as_ref().is_none_or(|(tab, _)| *tab != self.active) {
            let editor = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("New note…")
                    .auto_grow(2, 14)
            });
            self._draft_sub = Some(cx.subscribe(&editor, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.promote_draft(cx);
                }
            }));
            self.draft = Some((self.active.clone(), editor));
        }
    }

    fn render_tabs(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut row = div()
            .id("notes-tabs")
            .flex()
            .items_center()
            .gap_0p5()
            .flex_1()
            .min_w_0()
            .overflow_x_scroll();
        for tab in self.file.tabs.clone() {
            let count = self.file.count_in(&tab.id);
            let label = if count > 0 && tab.kind != TabKind::Archive {
                format!("{} ({count})", tab.name)
            } else {
                tab.name.clone()
            };
            let id = tab.id.clone();
            row = row.child(
                Button::new(SharedString::from(format!("nt-{}", tab.id)))
                    .ghost()
                    .size(ButtonSize::Sm)
                    .label(label)
                    .toggled(self.active == tab.id)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active = id.clone();
                        cx.notify();
                    })),
            );
        }
        row = row.child(
            Button::new("nt-add")
                .ghost()
                .size(ButtonSize::Sm)
                .icon(Icon::from(gpui_kit::assets::IconName::Plus))
                .tooltip("New Tab")
                .on_click(cx.listener(|this, _, _, cx| this.add_tab(cx))),
        );
        div()
            .flex()
            .items_center()
            .px_1()
            .py_0p5()
            .border_b_1()
            .border_color(Colors::of(cx).border)
            .child(row)
            .into_any_element()
    }

    fn render_note(&mut self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let c = Colors::of(cx);
        let Some(note) = self.file.note(id).cloned() else {
            return div().into_any_element();
        };
        let archive = self.is_archive();
        let empty = self.text_of(id, cx).trim().is_empty();
        let has_other_tabs = self
            .file
            .tabs
            .iter()
            .filter(|t| t.kind != TabKind::Archive)
            .count()
            > 1;
        let body: AnyElement = match (archive, self.editors.get(id)) {
            (false, Some(editor)) => Textarea::new(editor).into_any_element(),
            _ => div()
                .text_sm()
                .whitespace_normal()
                .child(note.text.clone())
                .into_any_element(),
        };
        let now = now_millis();
        let meta = note.last_sent().map(|s| {
            let how = match s.how {
                SendKind::Paste => "Sent to",
                SendKind::NewSession => "Started session",
            };
            let times = if note.sent.len() > 1 {
                format!(" · sent {} times", note.sent.len())
            } else {
                String::new()
            };
            format!("{how} “{}” · {}{times}", s.session_name, relative(s.at, now))
        });
        let (a, b, d, m, r) = (
            id.to_owned(),
            id.to_owned(),
            id.to_owned(),
            id.to_owned(),
            id.to_owned(),
        );
        div()
            .id(SharedString::from(format!("note-{id}")))
            .flex()
            .flex_col()
            .gap_1()
            .p_2()
            .rounded_md()
            .border_1()
            .border_color(c.border)
            .child(body)
            .children(meta.map(|m| div().text_xs().text_color(c.muted_foreground).child(m)))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_1()
                    .child(
                        Button::new(SharedString::from(format!("send-{id}")))
                            .primary()
                            .size(ButtonSize::Sm)
                            .label("Send")
                            .tooltip("Paste into the selected session and press Enter")
                            .disabled(empty)
                            .on_click(cx.listener(move |this, _, _, cx| this.send(&a, false, cx))),
                    )
                    .child(
                        Button::new(SharedString::from(format!("new-{id}")))
                            .outline()
                            .size(ButtonSize::Sm)
                            .label("New Session")
                            .tooltip("Start a new session with this note as its first message")
                            .disabled(empty)
                            .on_click(cx.listener(move |this, _, _, cx| this.send(&b, true, cx))),
                    )
                    .child(div().flex_1())
                    .when(archive, |el| {
                        el.child(
                            Button::new(SharedString::from(format!("restore-{id}")))
                                .ghost()
                                .size(ButtonSize::Sm)
                                .label("Restore")
                                .tooltip("Move back to the tab it came from")
                                .on_click(cx.listener(move |this, _, _, cx| this.restore(&r, cx))),
                        )
                    })
                    .when(!archive && has_other_tabs, |el| {
                        el.child(
                            Button::new(SharedString::from(format!("move-{id}")))
                                .ghost()
                                .size(ButtonSize::Sm)
                                .label("Move…")
                                .on_click(cx.listener(move |this, _, _, cx| this.move_note(&m, cx))),
                        )
                    })
                    .child(
                        Button::new(SharedString::from(format!("del-{id}")))
                            .ghost()
                            .size(ButtonSize::Sm)
                            .icon(Icon::from(gpui_kit::assets::IconName::Delete))
                            .tooltip("Delete Note")
                            .on_click(cx.listener(move |this, _, _, cx| this.delete(&d, cx))),
                    ),
            )
            .into_any_element()
    }
}

impl Render for NotesPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = Colors::of(cx);
        if self.file.tab(&self.active).is_none() {
            self.active = self.file.notes_tab().to_owned();
        }
        self.ensure_editors(window, cx);
        let tabs = self.render_tabs(cx);
        let archive = self.is_archive();
        let custom = self
            .file
            .tab(&self.active)
            .is_some_and(|t| t.kind == TabKind::Custom);
        let ids: Vec<String> = self
            .file
            .notes_in(&self.active)
            .into_iter()
            .map(|n| n.id.clone())
            .collect();
        let notes: Vec<AnyElement> = ids.iter().map(|id| self.render_note(id, cx)).collect();
        let empty = notes.is_empty();
        let draft = (!archive)
            .then(|| self.draft.as_ref().map(|(_, e)| e.clone()))
            .flatten();
        div()
            .id("notes-panel")
            .track_focus(&self.focus)
            .size_full()
            .flex()
            .flex_col()
            .bg(c.background)
            .child(tabs)
            // The user's own tabs can be renamed and deleted.
            .when(custom, |el| {
                el.child(
                    div()
                        .px_2()
                        .py_1()
                        .flex()
                        .items_center()
                        .justify_end()
                        .gap_1()
                        .child(
                            Button::new("nt-rename")
                                .ghost()
                                .size(ButtonSize::Sm)
                                .label("Rename Tab…")
                                .on_click(cx.listener(|this, _, _, cx| this.rename_tab(cx))),
                        )
                        .child(
                            Button::new("nt-delete")
                                .ghost()
                                .size(ButtonSize::Sm)
                                .label("Delete Tab…")
                                .on_click(cx.listener(|this, _, _, cx| this.delete_tab(cx))),
                        ),
                )
            })
            .child(
                div()
                    .id("notes-list")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px_2()
                    .pb_2()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .children(notes)
                    .children(draft.map(|editor| {
                        div()
                            .p_2()
                            .rounded_md()
                            .border_1()
                            .border_dashed()
                            .border_color(c.border)
                            .child(Textarea::new(&editor))
                    }))
                    .when(empty && archive, |el| {
                        el.child(
                            div()
                                .pt_4()
                                .text_sm()
                                .text_color(c.muted_foreground)
                                .child("Notes you send to a session land here."),
                        )
                    }),
            )
            .child(
                div()
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .border_t_1()
                    .border_color(c.border)
                    .text_xs()
                    .text_color(c.muted_foreground)
                    .child("Saved in the app's data folder, not in the project."),
            )
    }
}
