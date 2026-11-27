//! Every action the menus and shortcuts dispatch (build plan, section 6.9).
//! All shortcuts use ⌘, so every other key reaches the focused terminal.

use gpui_kit::{Action, App, KeyBinding, actions};
use uuid::Uuid;

actions!(
    anveg,
    [
        Quit,
        About,
        OpenSettings,
        OpenFolder,
        OpenFromGitHub,
        PublishFolderToGitHub,
        PublishToGitHub,
        NewSession,
        NewSessionInWorktree,
        NewTerminal,
        CloseProject,
        CloseWindow,
        Minimize,
        Zoom,
        ToggleTab,
        NextSession,
        PreviousSession,
        FocusFilter,
        RenameSession,
        StopSession,
        DeleteSession,
        ForkSession,
        CopySessionId,
        RevealTranscript,
        ProjectSettings,
        ToggleSplit,
        ClearRecent,
        OpenSession1,
        OpenSession2,
        OpenSession3,
        OpenSession4,
        OpenSession5,
        OpenSession6,
        OpenSession7,
        OpenSession8,
        OpenSession9,
        // Phase 4
        QuickOpen,
        SaveFile,
        SaveAll,
        GoToLine,
        FormatDocument,
        AskClaudeAboutSelection,
        ToggleSoftWrap,
        CloseTab,
        ShowSessions,
        ShowFiles,
        ShowGit,
        ShowGitHub,
        // Phase 5
        Commit,
        // Notes
        ToggleNotes,
        // Toolbar commands
        EditToolbarCommands,
        // Project tabs and windows
        NextProject,
        PreviousProject,
        MoveProjectToNewWindow,
        MergeAllWindows,
        RenameWindow,
    ]
);

/// File ▸ Open Recent ▸ <project>.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = anveg, no_json)]
pub struct OpenRecent {
    pub project_id: Uuid,
}

/// Window ▸ Move Project to Window ▸ <window>: move the shown project to
/// the app window with this id.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = anveg, no_json)]
pub struct MoveProjectToWindow {
    pub window: Uuid,
}

/// A toolbar command's shortcut: the window runs its project's command
/// with this shortcut (see `project_commands.rs`).
#[derive(Clone, PartialEq, Action)]
#[action(namespace = anveg, no_json)]
pub struct RunProjectCommand {
    pub shortcut: String,
}

/// The key context of the session list, where ⌫ deletes.
pub const SESSION_LIST_CONTEXT: &str = "SessionList";

/// The key context of the Git panel, where ⌘↩ commits.
pub const GIT_PANEL_CONTEXT: &str = "GitPanel";

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-o", OpenFolder, None),
        KeyBinding::new("cmd-n", NewSession, None),
        KeyBinding::new("cmd-shift-t", NewTerminal, None),
        KeyBinding::new("cmd-shift-n", NewSessionInWorktree, None),
        KeyBinding::new("cmd-w", CloseProject, None),
        KeyBinding::new("cmd-alt-w", CloseWindow, None),
        KeyBinding::new("cmd-alt-right", NextProject, None),
        KeyBinding::new("cmd-alt-left", PreviousProject, None),
        KeyBinding::new("cmd-m", Minimize, None),
        KeyBinding::new("cmd-t", ToggleTab, None),
        KeyBinding::new("cmd-shift-]", NextSession, None),
        KeyBinding::new("cmd-shift-[", PreviousSession, None),
        KeyBinding::new("cmd-f", FocusFilter, None),
        KeyBinding::new("cmd-\\", ToggleSplit, None),
        KeyBinding::new("cmd-1", OpenSession1, None),
        KeyBinding::new("cmd-2", OpenSession2, None),
        KeyBinding::new("cmd-3", OpenSession3, None),
        KeyBinding::new("cmd-4", OpenSession4, None),
        KeyBinding::new("cmd-5", OpenSession5, None),
        KeyBinding::new("cmd-6", OpenSession6, None),
        KeyBinding::new("cmd-7", OpenSession7, None),
        KeyBinding::new("cmd-8", OpenSession8, None),
        KeyBinding::new("cmd-9", OpenSession9, None),
        KeyBinding::new("cmd-p", QuickOpen, None),
        KeyBinding::new("cmd-alt-n", ToggleNotes, None),
        KeyBinding::new("cmd-alt-s", SaveAll, None),
        KeyBinding::new("cmd-shift-w", CloseTab, None),
        KeyBinding::new("cmd-shift-1", ShowSessions, None),
        KeyBinding::new("cmd-shift-2", ShowFiles, None),
        KeyBinding::new("cmd-shift-3", ShowGit, None),
        KeyBinding::new("cmd-shift-4", ShowGitHub, None),
        KeyBinding::new("cmd-s", SaveFile, Some(crate::editor_view::EDITOR_CONTEXT)),
        KeyBinding::new("cmd-l", GoToLine, Some(crate::editor_view::EDITOR_CONTEXT)),
        KeyBinding::new("alt-shift-f", FormatDocument, Some(crate::editor_view::EDITOR_CONTEXT)),
        KeyBinding::new(
            "cmd-shift-l",
            AskClaudeAboutSelection,
            Some(crate::editor_view::EDITOR_CONTEXT),
        ),
        KeyBinding::new("alt-z", ToggleSoftWrap, Some(crate::editor_view::EDITOR_CONTEXT)),
        KeyBinding::new("cmd-enter", Commit, Some(GIT_PANEL_CONTEXT)),
        KeyBinding::new("backspace", DeleteSession, Some(SESSION_LIST_CONTEXT)),
        KeyBinding::new("cmd-backspace", DeleteSession, Some(SESSION_LIST_CONTEXT)),
    ]);
}
