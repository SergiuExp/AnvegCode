//! Anveg Code: a lightweight desktop app for managing Claude Code sessions.
//!
//! One process for all windows. Each project is a tab of an app window,
//! with its sessions on the left and the selected session's real Claude
//! Code UI on the right. See `docs/build-plan.md`.

mod actions;
mod activity_view;
mod app_window;
mod app_assets;
mod component_theme;
mod dialogs;
mod diff_view;
mod docs;
mod editor_view;
mod files_panel;
mod git_panel;
mod github;
mod github_log_view;
mod image_view;
mod languages;
mod lsp_manager;
mod logging;
mod menus;
mod model;
mod commands_view;
mod project_commands;
mod notes_panel;
mod open_github;
mod panes;
mod platform;
mod project_window;
mod publish_view;
mod settings_window;
mod session_buttons_view;
mod session_toolbar;
mod sidebar;
mod sound;
mod split_diff_view;
mod ui;
mod welcome;
mod window_tabs;
mod windows;
mod workspace;
mod workspace_ui;

use std::time::Instant;

use gpui_kit::{App, AppContext as _, PromptLevel};
use uuid::Uuid;

use crate::actions::*;
use crate::model::{AppModel, ModelHandle, model};

fn main() {
    let started = Instant::now();
    let paths = match anveg_core::paths::AppPaths::for_current_user() {
        Ok(paths) => paths,
        Err(error) => {
            logging::init_stderr();
            tracing::error!(%error, "no home directory; cannot start");
            return;
        }
    };
    let _log_guard = logging::init(&paths);
    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        data = %paths.support_dir.display(),
        "Anveg Code starting"
    );

    let app = gpui_kit::application().with_assets(app_assets::AppAssets);
    // Clicking the Dock icon with no windows open.
    app.on_reopen(|cx| {
        if cx.has_global::<ModelHandle>() && model(cx).read(cx).windows.is_empty() {
            welcome::open(cx);
        }
    });
    app.run(move |cx: &mut App| {
        gpui_kit::init(cx);
        anveg_term::init(cx);
        // Development aid: ANVEG_THEME=dark|light in debug builds, for
        // machines without a system appearance setting (Linux under Xvfb).
        #[cfg(debug_assertions)]
        match std::env::var("ANVEG_THEME").as_deref() {
            Ok("dark") => gpui_cn::Theme::change(gpui_cn::ThemeMode::Dark, cx),
            Ok("light") => gpui_cn::Theme::change(gpui_cn::ThemeMode::Light, cx),
            _ => {}
        }
        component_theme::sync(cx);
        actions::bind_keys(cx);

        let app_model = cx.new(|cx| AppModel::new(paths, cx));
        cx.set_global(ModelHandle(app_model));
        install_actions(cx);
        menus::install(cx);
        project_commands::bind_shortcuts(cx);
        platform::init(cx);

        // Dock quit, logout and our own quit all end here: save and stop,
        // never ask.
        cx.on_app_quit(|cx| {
            let model = model(cx);
            // Autosave the open editors; GPUI waits for these writes (up
            // to its shutdown timeout) before the process ends.
            let views: Vec<_> = model.read(cx).windows.iter().filter_map(|w| w.view.upgrade()).collect();
            let writes: Vec<_> = views
                .into_iter()
                .flat_map(|v| v.update(cx, |v, cx| v.autosave_all(cx)))
                .collect();
            model.update(cx, |m, cx| m.on_app_quit(cx));
            async move {
                for write in writes {
                    write.await;
                }
            }
        })
        .detach();

        let opened = windows::restore(cx);
        if opened == 0 {
            welcome::open(cx);
        }
        tracing::info!(
            elapsed_ms = started.elapsed().as_millis() as u64,
            projects = opened,
            "first window opened"
        );
        cx.activate(true);
    });
}

fn install_actions(cx: &mut App) {
    // Deferred: a global action runs while GPUI is still dispatching in the
    // window it came from, and that window can't be updated until then.
    cx.on_action(|_: &Quit, cx| cx.defer(request_quit));
    cx.on_action(|_: &OpenFolder, cx| windows::prompt_open_folder(cx));
    cx.on_action(|_: &OpenSettings, cx| settings_window::open(cx));
    cx.on_action(|_: &OpenFromGitHub, cx| open_github::open(cx));
    cx.on_action(|_: &PublishFolderToGitHub, cx| windows::prompt_publish_folder(cx));
    cx.on_action(|action: &OpenRecent, cx| {
        windows::open_project(action.project_id, None, Vec::new(), cx)
    });
    cx.on_action(|_: &ClearRecent, cx| {
        let model = model(cx);
        let open: Vec<_> = model
            .read(cx)
            .windows
            .iter()
            .map(|w| w.project_id)
            .collect();
        model.update(cx, |m, cx| {
            m.update_projects(cx, |p| p.projects.retain(|x| open.contains(&x.id)))
        });
    });
    cx.on_action(|_: &About, cx| {
        let version = env!("CARGO_PKG_VERSION");
        let claude = model(cx)
            .read(cx)
            .claude
            .as_ref()
            .and_then(|c| c.version.clone())
            .unwrap_or_else(|| "not found".into());
        if let Some(window) = cx.active_window() {
            let _ = window.update(cx, |_, window, cx| {
                drop(window.prompt(
                    PromptLevel::Info,
                    &format!("Anveg Code {version}"),
                    Some(&format!(
                        "Claude Code: {claude}\n\nA lightweight home for your Claude Code sessions."
                    )),
                    &["OK"],
                    cx,
                ));
            });
        }
    });
}

/// ⌘Q: ask first when sessions are running (and the setting is on).
pub(crate) fn request_quit(cx: &mut App) {
    tracing::debug!("quit requested");
    let model = model(cx);
    // Unsaved files first: ask in the project that has them.
    let unsaved = model.read(cx).windows.iter().find_map(|w| {
        let view = w.view.upgrade()?;
        view.read(cx).has_unsaved(cx).then_some((w.handle, w.project_id, view))
    });
    if let Some((handle, project_id, view)) = unsaved {
        windows::show_project(project_id, cx);
        let _ = handle.update(cx, |_, window, cx| {
            view.update(cx, |v, cx| {
                v.ask_unsaved(true, window, cx);
            });
        });
        return;
    }
    let running = model.read(cx).running_sessions(cx);
    let confirm = model.read(cx).settings.confirm_on_close;
    if running.is_empty() || !confirm {
        model.update(cx, |m, cx| m.quit_now(cx));
        return;
    }
    let names: Vec<String> = running.iter().flat_map(|(_, _, n)| n.clone()).collect();
    // Ask in the window in front when it has some, in the project shown
    // there when that one has some.
    let active = cx.active_window();
    let shown = |id: Uuid, cx: &App| {
        model
            .read(cx)
            .window_for(id)
            .and_then(|w| w.view.upgrade())
            .is_some_and(|v| v.read(cx).shown)
    };
    let (_, project_id, _) = running
        .iter()
        .find(|(h, id, _)| Some(*h) == active && shown(*id, cx))
        .or_else(|| running.iter().find(|(h, _, _)| Some(*h) == active))
        .unwrap_or(&running[0]);
    let project_id = *project_id;
    windows::show_project(project_id, cx);
    let target = model
        .read(cx)
        .window_for(project_id)
        .map(|w| (w.handle, w.view.clone()));
    if let Some((handle, view)) = target {
        let _ = handle.update(cx, |_, _, cx| {
            let _ = view.update(cx, |v, cx| v.ask_quit(names, cx));
        });
    }
}
