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

/// Open (or focus) the window for a project.
pub fn open_project(
    project_id: Uuid,
    saved: Option<WindowState>,
    resume: Vec<String>,
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
pub fn restore(cx: &mut App) -> usize {
    let model = model(cx);
    let (state, resume_on_launch) = {
        let m = model.read(cx);
        (
            anveg_core::config::AppState::load(&m.paths.state_file()),
            m.settings.resume_on_launch,
        )
    };
    let mut opened = 0;
    for window in state.windows {
        if model.read(cx).projects.get(window.project_id).is_none() {
            continue;
        }
        let resume = if resume_on_launch {
            window.running_sessions.clone()
        } else {
            Vec::new()
        };
        open_project(window.project_id, Some(window), resume, cx);
        opened += 1;
    }
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
