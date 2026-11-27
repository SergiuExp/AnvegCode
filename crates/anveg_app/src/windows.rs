//! Opening projects and app windows (build plan, section 6.1). Each open
//! project is a tab of one app window (`app_window.rs`); a project opened
//! from anywhere joins the app window in front.

use std::path::PathBuf;
use std::time::Duration;

use anveg_core::config::{AppState, SavedBounds};
use gpui_cn::TitleBar;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Entity, PathPromptOptions, Size, WeakEntity,
    WindowBounds, WindowOptions, point, px, size,
};
use uuid::Uuid;

use crate::app_window::{AppWindow, ProjectToOpen};
use crate::model::model;
use crate::project_window::ProjectWindow;
use crate::workspace::{ButtonStyle, Prompt};

/// Marks app windows for the macOS code (`platform`), which tells them
/// from the welcome and settings windows by it. macOS's own window tabs
/// are off.
pub const TABBING_ID: &str = "dev.anveg.code.project";

/// Where a project moves to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MoveTarget {
    NewWindow,
    /// The app window with this id.
    Window(Uuid),
}

/// Open (or show) a project: a new tab of the app window in front.
pub fn open_project(
    project_id: Uuid,
    saved: Option<anveg_core::config::WindowState>,
    resume: Vec<String>,
    cx: &mut App,
) {
    if show_project(project_id, cx) {
        return;
    }
    let model = model(cx);
    if model.read(cx).projects.get(project_id).is_none() {
        tracing::warn!(%project_id, "unknown project");
        return;
    }
    model.update(cx, |m, cx| m.update_projects(cx, |p| p.touch(project_id)));
    let mut p = Some(ProjectToOpen {
        project_id,
        saved,
        resume,
    });
    if let Some((handle, host)) = front_window(cx) {
        let _ = handle.update(cx, |_, window, cx| {
            if let Some(p) = p.take() {
                host.update(cx, |h, cx| h.add_project(p, true, window, cx));
            }
            window.activate_window();
        });
    }
    if let Some(p) = p {
        if !model.read(cx).app_windows.is_empty() {
            // The front window is busy (an update of it is running): call
            // this outside of one, e.g. from `cx.defer`.
            tracing::warn!(%project_id, "could not add the tab to the front window; opening a new window");
        }
        open_window(None, None, vec![p], 0, cx);
    }
    crate::welcome::close(cx);
}

/// An app window waiting to go full screen.
type FullScreenWait = (AnyWindowHandle, WeakEntity<AppWindow>);

/// The app window in front, or the one last in front.
fn front_window(cx: &App) -> Option<(AnyWindowHandle, Entity<AppWindow>)> {
    let m = model(cx).read(cx);
    let active = cx.active_window();
    let w = m
        .app_windows
        .iter()
        .find(|w| Some(w.handle) == active)
        .or_else(|| m.last_active_window.and_then(|id| m.app_window(id)))
        .or_else(|| m.app_windows.last())?;
    Some((w.handle, w.view.upgrade()?))
}

/// Bring an open project's window forward and show its tab. False when it
/// is not open.
pub fn show_project(project_id: Uuid, cx: &mut App) -> bool {
    let target = {
        let m = model(cx).read(cx);
        m.window_for(project_id)
            .and_then(|w| Some((w.handle, w.host.upgrade()?)))
    };
    let Some((handle, host)) = target else {
        return false;
    };
    handle
        .update(cx, |_, window, cx| {
            window.activate_window();
            host.update(cx, |h, cx| h.activate_project(project_id, window, cx));
        })
        .is_ok()
}

/// Open an app window with these project tabs, showing `projects[active]`.
/// One saved as full screen opens at its frame and is returned for
/// [`enter_full_screen`] too.
fn open_window(
    name: Option<String>,
    saved: Option<SavedBounds>,
    projects: Vec<ProjectToOpen>,
    active: usize,
    cx: &mut App,
) -> Option<(Entity<AppWindow>, Option<FullScreenWait>)> {
    let fullscreen = saved.is_some_and(|b| b.fullscreen);
    let bounds = match saved {
        Some(b) => {
            let rect = Bounds::new(point(px(b.x), px(b.y)), size(px(b.width), px(b.height)));
            // Not `WindowBounds::Fullscreen`: GPUI asks for full screen as
            // the window opens, and macOS sometimes ignores that (while the
            // app is still activating, or another window is opening).
            if b.maximized {
                WindowBounds::Maximized(rect)
            } else {
                WindowBounds::Windowed(rect)
            }
        }
        None => WindowBounds::Windowed(Bounds::centered(None, size(px(1200.), px(780.)), cx)),
    };
    let options = WindowOptions {
        window_bounds: Some(bounds),
        window_min_size: Some(Size {
            width: px(640.),
            height: px(400.),
        }),
        ..TitleBar::window_options(cx)
    };
    match gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| AppWindow::new(Uuid::new_v4(), name, projects, active, window, cx))
    }) {
        Ok((handle, view)) => {
            crate::welcome::close(cx);
            let wait = fullscreen.then(|| {
                view.update(cx, |w, _| w.restore_fullscreen = true);
                (handle, view.downgrade())
            });
            Some((view, wait))
        }
        Err(error) => {
            tracing::error!(%error, "could not open the window");
            None
        }
    }
}

/// Put these windows in full screen, one at a time: each is brought
/// forward, asked, and checked once macOS's animation is over, and asked
/// again (up to three times) when it did not take. Asking for several at
/// once, or while the app is still activating, gets some ignored.
fn enter_full_screen(windows: Vec<FullScreenWait>, cx: &mut App) {
    if windows.is_empty() {
        return;
    }
    cx.spawn(async move |cx| {
        let executor = cx.background_executor().clone();
        let pause = |ms: u64| executor.timer(Duration::from_millis(ms));
        // After launch's activation and the windows' first frames.
        pause(300).await;
        for (handle, host) in windows {
            let is_full = |cx: &mut gpui_kit::AsyncApp| {
                handle
                    .update(cx, |_, w, _| w.is_fullscreen())
                    .unwrap_or(false)
            };
            let mut done = false;
            for attempt in 0..3 {
                if handle.update(cx, |_, w, _| w.is_fullscreen()).is_err() {
                    // Closed meanwhile.
                    break;
                }
                if !is_full(cx) {
                    let _ = handle.update(cx, |_, w, _| {
                        w.activate_window();
                        w.toggle_fullscreen();
                    });
                }
                for _ in 0..25 {
                    pause(100).await;
                    if is_full(cx) {
                        break;
                    }
                }
                // macOS marks the window full screen as the animation
                // starts; one that fails leaves it again.
                pause(1200).await;
                if is_full(cx) {
                    done = true;
                    break;
                }
                tracing::info!(attempt, "full screen did not take; asking again");
            }
            if !done {
                tracing::warn!("could not restore a window to full screen");
            }
            let _ = host.update(cx, |w, cx| {
                w.restore_fullscreen = false;
                cx.notify();
            });
        }
    })
    .detach();
}

/// Open a folder as a project (creating it when new) and show its window.
pub fn open_folder(folder: PathBuf, cx: &mut App) {
    let model = model(cx);
    let result = model.update(cx, |m, cx| {
        let mut id = None;
        let mut err = None;
        m.update_projects(cx, |p| match p.open_folder(&folder) {
            Ok(project) => id = Some(project),
            Err(e) => err = Some(e),
        });
        match (id, err) {
            (Some(id), _) => Ok(id),
            (None, Some(e)) => Err(e),
            (None, None) => Err(anyhow::anyhow!("could not open the folder")),
        }
    });
    match result {
        Ok(id) => open_project(id, None, Vec::new(), cx),
        Err(error) => tracing::error!(folder = %folder.display(), %error, "could not open folder"),
    }
}

/// Welcome ▸ Publish a Folder to GitHub: pick a folder, open it as a
/// project, and show its Publish to GitHub tab.
pub fn prompt_publish_folder(cx: &mut App) {
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Publish".into()),
    });
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(paths))) = rx.await
            && let Some(folder) = paths.into_iter().next()
        {
            cx.update(|cx| {
                open_folder(folder.clone(), cx);
                // The window opened (or came forward) for that folder.
                let m = model(cx);
                let target = m.read(cx).windows.iter().find_map(|w| {
                    let p = m.read(cx).projects.get(w.project_id)?;
                    crate::editor_view::same_file(&p.main_folder, &folder)
                        .then(|| (w.handle, w.view.clone()))
                });
                if let Some((handle, view)) = target {
                    let _ = handle.update(cx, |_, window, cx| {
                        let _ = view.update(cx, |v, cx| {
                            v.open_doc(anveg_core::config::DocKey::PublishToGitHub, window, cx)
                        });
                    });
                }
            });
        }
    })
    .detach();
}

/// ⌘O: pick a folder.
pub fn prompt_open_folder(cx: &mut App) {
    let rx = cx.prompt_for_paths(PathPromptOptions {
        files: false,
        directories: true,
        multiple: false,
        prompt: Some("Open".into()),
    });
    cx.spawn(async move |cx| {
        if let Ok(Ok(Some(paths))) = rx.await
            && let Some(folder) = paths.into_iter().next()
        {
            cx.update(|cx| open_folder(folder, cx));
        }
    })
    .detach();
}

/// Reopen the windows saved in `state.json`. Returns how many projects
/// opened.
///
/// Windows come back with their tabs, frames and names; full-screen ones
/// come back full screen (each in its own space, as they were). A file
/// from before windows held several projects gives one window with all of
/// them.
pub fn restore(cx: &mut App) -> usize {
    let model = model(cx);
    let (state, resume_on_launch) = {
        let m = model.read(cx);
        (
            AppState::load(&m.paths.state_file()),
            m.settings.resume_on_launch,
        )
    };
    let mut windows = state.app_windows_to_restore();
    // Windowed ones first, so a full-screen window ends up in front.
    windows.sort_by_key(|w| w.bounds.is_some_and(|b| b.fullscreen));
    let mut opened = 0;
    let mut full = Vec::new();
    for w in windows {
        let shown = w.projects.get(w.active).copied();
        let ids: Vec<Uuid> = w
            .projects
            .iter()
            .copied()
            .filter(|id| model.read(cx).projects.get(*id).is_some())
            .collect();
        if ids.is_empty() {
            continue;
        }
        let active = shown
            .and_then(|id| ids.iter().position(|p| *p == id))
            .unwrap_or(0);
        let projects: Vec<ProjectToOpen> = ids
            .iter()
            .map(|id| {
                let saved = state.project(*id).cloned();
                let resume = match (&saved, resume_on_launch) {
                    (Some(s), true) => s.running_sessions.clone(),
                    _ => Vec::new(),
                };
                ProjectToOpen {
                    project_id: *id,
                    saved,
                    resume,
                }
            })
            .collect();
        for id in &ids {
            model.update(cx, |m, cx| m.update_projects(cx, |p| p.touch(*id)));
        }
        opened += projects.len();
        if let Some((_, Some(wait))) = open_window(w.name, w.bounds, projects, active, cx) {
            full.push(wait);
        }
    }
    enter_full_screen(full, cx);
    opened
}

/// Select a session from a notification click.
pub fn reveal_session(project_id: Uuid, session_id: String, cx: &mut App) {
    if !show_project(project_id, cx) {
        open_project(project_id, None, Vec::new(), cx);
        return;
    }
    let target = model(cx)
        .read(cx)
        .window_for(project_id)
        .map(|w| (w.handle, w.view.clone()));
    if let Some((handle, view)) = target {
        let _ = handle.update(cx, |_, window, cx| {
            let _ = view.update(cx, |v, cx| v.reveal_session(&session_id, window, cx));
        });
    }
}

// ---------------------------------------------------------------------
// Moving projects between windows
// ---------------------------------------------------------------------

/// An open project: its window, its window's view, and its own view.
fn find(project_id: Uuid, cx: &App) -> Option<(AnyWindowHandle, Entity<AppWindow>, Entity<ProjectWindow>)> {
    let m = model(cx).read(cx);
    let w = m.window_for(project_id)?;
    Some((w.handle, w.host.upgrade()?, w.view.upgrade()?))
}

/// Show a prompt in a project's tab.
fn ask(project_id: Uuid, prompt: Prompt, cx: &mut App) {
    if let Some((handle, _, view)) = find(project_id, cx) {
        let _ = handle.update(cx, |_, window, cx| {
            view.update(cx, |v, cx| v.show_prompt(prompt, window, cx));
        });
    }
}

/// What moving these projects would interrupt, as a sentence (empty when
/// nothing would be).
fn interruptions(projects: &[Uuid], cx: &App) -> String {
    let mut sessions = Vec::new();
    let mut busy = false;
    for id in projects {
        if let Some((_, _, view)) = find(*id, cx) {
            let v = view.read(cx);
            sessions.extend(v.running_session_names());
            busy |= v.has_busy_terminal(cx);
        }
    }
    let mut text = String::new();
    if !sessions.is_empty() {
        text.push_str(&format!(
            "Running sessions restart there and continue their conversations ({}); a reply Claude is writing stops.",
            sessions.join(", ")
        ));
    }
    if busy {
        if !text.is_empty() {
            text.push(' ');
        }
        text.push_str("Commands running in terminal tabs stop.");
    }
    text
}

/// Projects among these with unsaved files that autosave does not cover.
fn unsaved_names(projects: &[Uuid], cx: &App) -> Vec<String> {
    projects
        .iter()
        .filter_map(|id| find(*id, cx))
        .filter(|(_, _, v)| v.read(cx).has_unsaved(cx))
        .map(|(_, _, v)| v.read(cx).project_name(cx))
        .collect()
}

/// Move a project to another window (or a new one). Its views open again
/// there from its saved state; asks first when that would stop something.
pub fn move_project(project_id: Uuid, to: MoveTarget, cx: &mut App) {
    let Some((_, host, view)) = find(project_id, cx) else {
        return;
    };
    let destination = match to {
        MoveTarget::NewWindow if host.read(cx).projects.len() < 2 => return,
        MoveTarget::NewWindow => "a new window".to_owned(),
        MoveTarget::Window(id) if id == host.read(cx).id => return,
        MoveTarget::Window(id) => match model(cx).read(cx).app_window(id).and_then(|w| w.view.upgrade()) {
            Some(w) => format!("“{}”", w.read(cx).display_name(cx)),
            None => return,
        },
    };
    // Its questions show in its tab.
    show_project(project_id, cx);
    let name = view.read(cx).project_name(cx);
    if !unsaved_names(&[project_id], cx).is_empty() {
        ask(
            project_id,
            Prompt::message(
                format!("Save “{name}” first"),
                "It has files with unsaved changes. Save them (⌥⌘S), then move it.",
            ),
            cx,
        );
        return;
    }
    let stops = interruptions(&[project_id], cx);
    if stops.is_empty() {
        start_move(vec![project_id], to, cx);
        return;
    }
    let prompt = Prompt::new(
        format!("Move “{name}” to {destination}?"),
        format!("It opens again in the other window. {stops}"),
    )
    .cancel()
    .button("Move", ButtonStyle::Primary, move |_, _, cx| {
        cx.defer(move |cx| start_move(vec![project_id], to, cx));
        Ok(())
    });
    ask(project_id, prompt, cx);
}

/// Window ▸ Merge All Windows: move every project into this window.
pub fn merge_all_into(target: Uuid, cx: &mut App) {
    let (projects, shown) = {
        let m = model(cx).read(cx);
        let mut projects = Vec::new();
        for w in &m.app_windows {
            if w.id == target {
                continue;
            }
            if let Some(view) = w.view.upgrade() {
                projects.extend(view.read(cx).projects.iter().map(|p| p.read(cx).project_id));
            }
        }
        let shown = m
            .app_window(target)
            .and_then(|w| w.view.upgrade())
            .and_then(|w| w.read(cx).active_project().map(|p| p.read(cx).project_id));
        (projects, shown)
    };
    let Some(shown) = shown else { return };
    if projects.is_empty() {
        return;
    }
    let unsaved = unsaved_names(&projects, cx);
    if !unsaved.is_empty() {
        ask(
            shown,
            Prompt::message(
                "Save files first",
                format!(
                    "{} {} files with unsaved changes. Save them (⌥⌘S), then merge the windows.",
                    unsaved.join(", "),
                    if unsaved.len() == 1 { "has" } else { "have" }
                ),
            ),
            cx,
        );
        return;
    }
    let to = MoveTarget::Window(target);
    let stops = interruptions(&projects, cx);
    if stops.is_empty() {
        start_move(projects, to, cx);
        return;
    }
    let prompt = Prompt::new(
        "Merge All Windows?",
        format!("The projects in other windows open again in this one. {stops}"),
    )
    .cancel()
    .button("Merge", ButtonStyle::Primary, move |_, _, cx| {
        let projects = projects.clone();
        cx.defer(move |cx| start_move(projects, to, cx));
        Ok(())
    });
    ask(shown, prompt, cx);
}

/// Finish the editors' autosaves (the moved views read the files again),
/// then move.
fn start_move(projects: Vec<Uuid>, to: MoveTarget, cx: &mut App) {
    let views: Vec<_> = projects.iter().filter_map(|id| find(*id, cx)).collect();
    let writes: Vec<_> = views
        .into_iter()
        .flat_map(|(_, _, v)| v.update(cx, |v, cx| v.autosave_all(cx)))
        .collect();
    cx.spawn(async move |cx| {
        for write in writes {
            write.await;
        }
        cx.update(|cx| {
            // The first one moving to a new window makes it; the rest
            // follow it there.
            let mut to = to;
            for id in projects {
                if let Some(window) = finish_move(id, to, cx) {
                    to = MoveTarget::Window(window);
                }
            }
        });
    })
    .detach();
}

/// Move one project now: stop it here, open it there, take its tab out
/// of the window it was in, and resume its sessions once the old
/// processes have exited. Returns the window it moved to.
fn finish_move(project_id: Uuid, to: MoveTarget, cx: &mut App) -> Option<Uuid> {
    let (handle, host, view) = find(project_id, cx)?;
    let state = view.read(cx).window_state(cx);
    let resume = state.running_sessions.clone();
    let terminals = view.read(cx).terminal_views();
    view.update(cx, |v, cx| v.stop_all(cx));
    let p = ProjectToOpen {
        project_id,
        saved: Some(state),
        resume: Vec::new(),
    };
    let moved = match to {
        MoveTarget::NewWindow => {
            // The source's size, a little down and to the right (or full
            // screen, as the source is).
            let bounds = host.read(cx).bounds().map(|mut b| {
                if !b.fullscreen {
                    b.x += 30.;
                    b.y += 30.;
                }
                b
            });
            open_window(None, bounds, vec![p], 0, cx).and_then(|(w, wait)| {
                enter_full_screen(wait.into_iter().collect(), cx);
                let w = w.read(cx);
                Some((w.id, w.projects.first()?.clone()))
            })
        }
        MoveTarget::Window(id) => {
            let target = model(cx)
                .read(cx)
                .app_window(id)
                .and_then(|w| Some((w.handle, w.view.upgrade()?)));
            target.and_then(|(target_handle, target)| {
                target_handle
                    .update(cx, |_, window, cx| {
                        let view = target.update(cx, |h, cx| h.add_project(p, true, window, cx));
                        (id, view)
                    })
                    .ok()
            })
        }
    };
    let Some((window_id, new_view)) = moved else {
        tracing::error!(%project_id, "could not move the project; resuming it where it was");
        let _ = handle.update(cx, |_, window, cx| {
            view.update(cx, |v, cx| v.resume_sessions(resume, window, cx));
        });
        return None;
    };
    tracing::info!(%project_id, sessions = resume.len(), "project moved to another window");
    let old = view.entity_id();
    drop(view);
    let _ = handle.update(cx, |_, window, cx| {
        host.update(cx, |h, cx| {
            if let Some(i) = h.projects.iter().position(|p| p.entity_id() == old) {
                h.remove_at(i, window, cx);
            }
        });
    });
    // Go where it went (its full-screen space, if it is in one), now that
    // the window it left is done with.
    show_project(project_id, cx);
    if !resume.is_empty() {
        cx.spawn(async move |cx| {
            // `claude --resume` of a session whose old process still runs
            // would make two; they stop within moments (up to 3 s).
            for _ in 0..60 {
                let stopped = cx.update(|cx| terminals.iter().all(|t| !t.read(cx).is_running(cx)));
                if stopped {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(50)).await;
            }
            cx.update(|cx| {
                let handle = model(cx).read(cx).window_for(project_id).map(|w| w.handle);
                if let Some(handle) = handle {
                    let _ = handle.update(cx, |_, window, cx| {
                        new_view.update(cx, |v, cx| v.resume_sessions(resume, window, cx));
                    });
                }
            });
        })
        .detach();
    }
    Some(window_id)
}
