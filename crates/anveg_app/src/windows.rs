//! Opening project windows (build plan, section 6.1).

use std::path::PathBuf;

use anveg_core::config::WindowState;
use gpui_cn::TitleBar;
use gpui_kit::{
    App, AppContext as _, Bounds, PathPromptOptions, Size, WindowBounds, WindowOptions, point, px,
    size,
};
use uuid::Uuid;

use crate::model::{OpenWindow, model};
use crate::project_window::ProjectWindow;

/// Project windows share this macOS tab group, so a window opened while
/// the app is full screen becomes a tab of it (System Settings ▸ Desktop &
/// Dock ▸ Prefer tabs).
pub const TABBING_ID: &str = "dev.anveg.code.project";

/// How to open a restored window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Restore {
    /// As it was saved (windowed, maximized or full screen).
    AsSaved,
    /// At its saved frame but not full screen: macOS adds it as a tab of
    /// the full-screen window in front, which makes it full screen too.
    AsTab,
}

/// Open (or focus) the window for a project.
pub fn open_project(
    project_id: Uuid,
    saved: Option<WindowState>,
    resume: Vec<String>,
    cx: &mut App,
) {
    open_project_with(project_id, saved, resume, Restore::AsSaved, cx);
}

fn open_project_with(
    project_id: Uuid,
    saved: Option<WindowState>,
    resume: Vec<String>,
    how: Restore,
    cx: &mut App,
) {
    let model = model(cx);
    if let Some(existing) = model.read(cx).window_for(project_id) {
        let handle = existing.handle;
        let _ = handle.update(cx, |_, window, _| window.activate_window());
        return;
    }
    if model.read(cx).projects.get(project_id).is_none() {
        tracing::warn!(%project_id, "unknown project");
        return;
    }
    model.update(cx, |m, cx| m.update_projects(cx, |p| p.touch(project_id)));

    let bounds = match saved.as_ref().and_then(|s| s.bounds) {
        Some(b) => {
            let rect = Bounds::new(point(px(b.x), px(b.y)), size(px(b.width), px(b.height)));
            if b.fullscreen && how == Restore::AsSaved {
                WindowBounds::Fullscreen(rect)
            } else if b.maximized {
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
        tabbing_identifier: Some(TABBING_ID.to_owned()),
        ..TitleBar::window_options(cx)
    };
    let saved_for_view = saved.clone();
    match gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| ProjectWindow::new(project_id, saved_for_view, resume, window, cx))
    }) {
        Ok((handle, view)) => {
            model.update(cx, |m, cx| {
                m.register_window(
                    OpenWindow {
                        project_id,
                        handle,
                        view: view.downgrade(),
                    },
                    cx,
                );
            });
            crate::welcome::close(cx);
        }
        Err(error) => tracing::error!(%error, "could not open the project window"),
    }
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

/// Reopen the windows saved in `state.json`. Returns how many opened.
///
/// Windows that were full screen come back full screen. When there are
/// several and macOS prefers tabs in full screen (the default), the first
/// opens full screen and the others join it as tabs once it is there, as
/// they were; with tabs turned off, each gets its own full-screen space.
pub fn restore(cx: &mut App) -> usize {
    let model = model(cx);
    let (state, resume_on_launch) = {
        let m = model.read(cx);
        (
            anveg_core::config::AppState::load(&m.paths.state_file()),
            m.settings.resume_on_launch,
        )
    };
    let windows: Vec<WindowState> = state
        .windows
        .into_iter()
        .filter(|w| model.read(cx).projects.get(w.project_id).is_some())
        .collect();
    let opened = windows.len();
    let resume_for = |w: &WindowState| {
        if resume_on_launch {
            w.running_sessions.clone()
        } else {
            Vec::new()
        }
    };
    let (full, other): (Vec<WindowState>, Vec<WindowState>) = windows
        .into_iter()
        .partition(|w| w.bounds.is_some_and(|b| b.fullscreen));
    // Windowed ones first, so the full-screen window ends up in front.
    for w in other {
        let resume = resume_for(&w);
        open_project(w.project_id, Some(w), resume, cx);
    }
    let tabs = crate::platform::tabbing_mode() != crate::platform::TabbingMode::Never;
    let mut full = full.into_iter();
    let Some(first) = full.next() else {
        return opened;
    };
    let rest: Vec<(WindowState, Vec<String>)> = full
        .map(|w| {
            let resume = resume_for(&w);
            (w, resume)
        })
        .collect();
    let first_id = first.project_id;
    let resume = resume_for(&first);
    open_project(first.project_id, Some(first), resume, cx);
    if rest.is_empty() {
        return opened;
    }
    if !tabs {
        for (w, resume) in rest {
            open_project(w.project_id, Some(w), resume, cx);
        }
        return opened;
    }
    // Wait for the first window to finish entering full screen (macOS
    // animates it), then add the others: macOS makes them its tabs.
    cx.spawn(async move |cx| {
        let mut ready = false;
        for _ in 0..40 {
            cx.background_executor()
                .timer(std::time::Duration::from_millis(100))
                .await;
            ready = cx.update(|cx| {
                let handle = crate::model::model(cx)
                    .read(cx)
                    .window_for(first_id)
                    .map(|w| w.handle);
                handle.is_some_and(|h| {
                    h.update(cx, |_, window, _| window.is_fullscreen())
                        .unwrap_or(false)
                })
            });
            if ready {
                break;
            }
        }
        cx.update(|cx| {
            let how = if ready {
                Restore::AsTab
            } else {
                Restore::AsSaved
            };
            for (w, resume) in rest {
                open_project_with(w.project_id, Some(w), resume, how, cx);
            }
        });
    })
    .detach();
    opened
}

/// Select a session from a notification click.
pub fn reveal_session(project_id: Uuid, session_id: String, cx: &mut App) {
    let model = model(cx);
    let target = model
        .read(cx)
        .window_for(project_id)
        .map(|w| (w.handle, w.view.clone()));
    match target {
        Some((handle, view)) => {
            let _ = handle.update(cx, |_, window, cx| {
                let _ = view.update(cx, |v, cx| v.reveal_session(&session_id, window, cx));
            });
        }
        None => open_project(project_id, None, Vec::new(), cx),
    }
}
