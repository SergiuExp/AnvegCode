//! The menu bar (build plan, section 6.9) and the Dock menu. Rebuilt when
//! the recent projects or the open windows change.
//!
//! The open projects are listed by the app itself, in the Window menu and
//! the Dock menu, grouped by app window; macOS's automatic window list
//! leaves the app windows out
//! (`platform::keep_project_windows_out_of_window_list`).

use std::path::{Path, PathBuf};

use gpui_kit::{App, Menu, MenuItem, SystemMenuType};
use uuid::Uuid;

use crate::actions::*;
use crate::model::model;

/// Menu labels for open projects, in window order: the project's name, plus
/// its folder when two open projects share a name.
pub fn window_labels(open: &[(Uuid, String, PathBuf)]) -> Vec<(Uuid, String)> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let short = |p: &Path| match home.as_deref().and_then(|h| p.strip_prefix(h).ok()) {
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    };
    open.iter()
        .map(|(id, name, folder)| {
            let shared = open.iter().filter(|(_, n, _)| n == name).count() > 1;
            let label = if shared {
                format!("{name} — {}", short(folder))
            } else {
                name.clone()
            };
            (*id, label)
        })
        .collect()
}

/// Open projects as menu items (each shows its tab), in window and tab
/// order, a separator between windows.
fn open_window_items(cx: &App) -> Vec<MenuItem> {
    let m = model(cx).read(cx);
    let groups: Vec<Vec<Uuid>> = m
        .app_windows
        .iter()
        .filter_map(|w| w.view.upgrade())
        .map(|w| w.read(cx).projects.iter().map(|p| p.read(cx).project_id).collect())
        .collect();
    let open: Vec<(Uuid, String, PathBuf)> = groups
        .iter()
        .flatten()
        .filter_map(|id| {
            let p = m.projects.get(*id)?;
            Some((p.id, p.name.clone(), p.main_folder.clone()))
        })
        .collect();
    let labels = window_labels(&open);
    let mut items = Vec::new();
    for group in groups {
        if !items.is_empty() {
            items.push(MenuItem::separator());
        }
        items.extend(
            labels
                .iter()
                .filter(|(id, _)| group.contains(id))
                .map(|(id, label)| MenuItem::action(label.clone(), OpenRecent { project_id: *id })),
        );
    }
    items
}

/// Window ▸ Move Project to Window ▸ <the other windows>: every window but
/// the one in front.
fn move_to_items(cx: &App) -> Vec<MenuItem> {
    let m = model(cx).read(cx);
    let front = cx
        .active_window()
        .and_then(|a| m.app_windows.iter().find(|w| w.handle == a))
        .or_else(|| m.last_active_window.and_then(|id| m.app_window(id)))
        .map(|w| w.id);
    m.app_windows
        .iter()
        .filter(|w| Some(w.id) != front)
        .filter_map(|w| {
            let name = w.view.upgrade()?.read(cx).display_name(cx);
            Some(MenuItem::action(name, MoveProjectToWindow { window: w.id }))
        })
        .collect()
}

pub fn install(cx: &App) {
    let windows = open_window_items(cx);
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
            MenuItem::action("Open Project…", OpenFolder),
            MenuItem::action("Open from GitHub…", OpenFromGitHub),
            MenuItem::action("Publish to GitHub…", PublishToGitHub),
            MenuItem::action("Go to File…", QuickOpen),
            MenuItem::submenu(Menu::new("Open Recent").items(recent_items)),
            MenuItem::separator(),
            MenuItem::action("Save", SaveFile),
            MenuItem::action("Save All", SaveAll),
            MenuItem::separator(),
            MenuItem::action("Project Settings…", ProjectSettings),
            MenuItem::action("Toolbar Commands…", EditToolbarCommands),
            MenuItem::action("Close Tab", CloseTab),
            MenuItem::action("Close Project", CloseProject),
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
            MenuItem::action("Notes", ToggleNotes),
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
        Menu::new("Window").items({
            let mut items = vec![
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
                MenuItem::separator(),
                MenuItem::action("Show Next Project", NextProject),
                MenuItem::action("Show Previous Project", PreviousProject),
                MenuItem::separator(),
                MenuItem::action("Move Project to New Window", MoveProjectToNewWindow),
            ];
            let others = move_to_items(cx);
            if !others.is_empty() {
                items.push(MenuItem::submenu(
                    Menu::new("Move Project to Window").items(others),
                ));
                items.push(MenuItem::action("Merge All Windows", MergeAllWindows));
            }
            items.push(MenuItem::action("Rename Window…", RenameWindow));
            let projects = open_window_items(cx);
            if !projects.is_empty() {
                items.push(MenuItem::separator());
                items.extend(projects);
            }
            items
        }),
    ]);
    // After the menu bar: both use the same action table.
    let mut dock = windows;
    if !dock.is_empty() {
        dock.push(MenuItem::separator());
    }
    dock.push(MenuItem::action("Open Project…", OpenFolder));
    dock.push(MenuItem::action("Open from GitHub…", OpenFromGitHub));
    cx.set_dock_menu(dock);
    // Setting the menu bar hands macOS a new window-list menu; keep the
    // project windows out of its automatic list (they are listed above).
    crate::platform::keep_project_windows_out_of_window_list();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_tell_same_names_apart() {
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let labels = window_labels(&[
            (a, "app".into(), "/work/one/app".into()),
            (b, "api".into(), "/work/api".into()),
            (c, "app".into(), "/work/two/app".into()),
        ]);
        assert_eq!(
            labels,
            vec![
                (a, "app — /work/one/app".to_owned()),
                (b, "api".to_owned()),
                (c, "app — /work/two/app".to_owned()),
            ]
        );
    }
}
