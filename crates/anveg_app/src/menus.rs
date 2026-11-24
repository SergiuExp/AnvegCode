//! The menu bar (build plan, section 6.9). Rebuilt when the recent
//! projects change.

use gpui_kit::{App, Menu, MenuItem, SystemMenuType};

use crate::actions::*;
use crate::model::model;

pub fn install(cx: &App) {
    let recent: Vec<MenuItem> = model(cx)
        .read(cx)
        .projects
        .recent()
        .into_iter()
        .take(15)
        .map(|p| MenuItem::action(p.name.clone(), OpenRecent { project_id: p.id }))
        .collect();
    let mut recent_items = recent;
    if !recent_items.is_empty() {
        recent_items.push(MenuItem::separator());
    }
    recent_items.push(MenuItem::action("Clear Menu", ClearRecent));

    cx.set_menus([
        Menu::new("Anveg Code").items([
            MenuItem::action("About Anveg Code", About),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Quit Anveg Code", Quit),
        ]),
        Menu::new("File").items([
            MenuItem::action("New Session", NewSession),
            MenuItem::action("New Session in Worktree…", NewSessionInWorktree),
            MenuItem::action("New Terminal", NewTerminal),
            MenuItem::separator(),
            MenuItem::action("Open Folder…", OpenFolder),
            MenuItem::action("Go to File…", QuickOpen),
            MenuItem::submenu(Menu::new("Open Recent").items(recent_items)),
            MenuItem::separator(),
            MenuItem::action("Save", SaveFile),
            MenuItem::action("Save All", SaveAll),
            MenuItem::separator(),
            MenuItem::action("Project Settings…", ProjectSettings),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("Edit").items([
            MenuItem::action("Copy", gpui_cn::terminal::actions::Copy),
            MenuItem::action("Paste", gpui_cn::terminal::actions::Paste),
            MenuItem::action("Select All", gpui_cn::terminal::actions::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find", FocusFilter),
            MenuItem::action("Go to Line…", GoToLine),
            MenuItem::action("Format Document", FormatDocument),
            MenuItem::action("Ask Claude About Selection", AskClaudeAboutSelection),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Claude/Terminal", ToggleTab),
            MenuItem::action("Split View", ToggleSplit),
            MenuItem::action("Soft Wrap", ToggleSoftWrap),
            MenuItem::separator(),
            MenuItem::action("Sessions", ShowSessions),
            MenuItem::action("Files", ShowFiles),
            MenuItem::action("Git", ShowGit),
            MenuItem::action("GitHub", ShowGitHub),
            MenuItem::separator(),
            MenuItem::action("Next Session", NextSession),
            MenuItem::action("Previous Session", PreviousSession),
        ]),
        Menu::new("Session").items([
            MenuItem::action("Rename…", RenameSession),
            MenuItem::action("Stop", StopSession),
            MenuItem::action("Fork Session", ForkSession),
            MenuItem::action("Delete…", DeleteSession),
            MenuItem::separator(),
            MenuItem::action("Copy Session ID", CopySessionId),
            MenuItem::action("Show Transcript in Finder", RevealTranscript),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", Minimize),
            MenuItem::action("Zoom", Zoom),
        ]),
    ]);
}
