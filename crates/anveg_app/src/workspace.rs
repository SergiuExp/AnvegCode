//! What the sidebar panels and document tabs ask their project window to
//! do (next phases, 0.1). Panels and documents are separate entities; they
//! emit [`WorkspaceEvent`]s and the window acts on them.

use std::path::PathBuf;
use std::rc::Rc;

use anveg_core::config::DocKey;
use gpui_kit::{App, SharedString, Window};

/// A request from a panel or a document to its window.
#[derive(Clone)]
pub enum WorkspaceEvent {
    /// Open a file in an editor tab, optionally at a 1-based line/column.
    OpenFile {
        path: PathBuf,
        line: Option<u32>,
        column: Option<u32>,
    },
    /// Open (or show) a document tab.
    OpenDoc(DocKey),
    /// Close this document's tab.
    CloseDoc(DocKey),
    /// The tab title or dirty state changed.
    TitleChanged,
    /// Type text into the active Claude session's prompt. Never presses
    /// Enter.
    TypeIntoClaude(String),
    /// Run a command in a new terminal tab.
    RunInTerminal(Vec<String>),
    /// A short message at the bottom of the window.
    Toast(SharedString),
    /// A modal question.
    Prompt(Prompt),
    /// Git state may have changed (after an operation): refresh it.
    GitChanged,
}

/// What a prompt's button does. Gets the text field's value, if the prompt
/// has one. An `Err` is shown in the dialog, which stays open.
pub type PromptAction = Rc<dyn Fn(Option<String>, &mut Window, &mut App) -> Result<(), String>>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonStyle {
    Primary,
    Destructive,
    Outline,
}

#[derive(Clone)]
pub struct PromptButton {
    pub label: SharedString,
    pub style: ButtonStyle,
    /// `None` just closes (Cancel).
    pub action: Option<PromptAction>,
}

/// A modal dialog any panel can ask for.
#[derive(Clone)]
pub struct Prompt {
    pub title: SharedString,
    pub body: SharedString,
    /// A one-line text field: (placeholder, initial value).
    pub input: Option<(SharedString, String)>,
    pub buttons: Vec<PromptButton>,
}

impl Prompt {
    pub fn new(title: impl Into<SharedString>, body: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            body: body.into(),
            input: None,
            buttons: Vec::new(),
        }
    }

    pub fn input(mut self, placeholder: impl Into<SharedString>, value: impl Into<String>) -> Self {
        self.input = Some((placeholder.into(), value.into()));
        self
    }

    pub fn cancel(mut self) -> Self {
        self.buttons.push(PromptButton {
            label: "Cancel".into(),
            style: ButtonStyle::Outline,
            action: None,
        });
        self
    }

    pub fn button(
        mut self,
        label: impl Into<SharedString>,
        style: ButtonStyle,
        action: impl Fn(Option<String>, &mut Window, &mut App) -> Result<(), String> + 'static,
    ) -> Self {
        self.buttons.push(PromptButton {
            label: label.into(),
            style,
            action: Some(Rc::new(action)),
        });
        self
    }

    /// A message with an OK button.
    pub fn message(title: impl Into<SharedString>, body: impl Into<SharedString>) -> Self {
        Self::new(title, body).button("OK", ButtonStyle::Primary, |_, _, _| Ok(()))
    }
}
