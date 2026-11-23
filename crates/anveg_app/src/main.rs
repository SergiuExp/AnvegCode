//! Anveg Code — Milestone 1 terminal spike.
//!
//! One window with two tabs: **Claude** runs `claude` and **Terminal** runs
//! the user's login shell, each in the embedded terminal and each started
//! the first time its tab is shown. This exists to validate the terminal
//! (build plan, checklist 5.5) before the real app is built around it.

mod logging;
mod spike;

use std::time::Instant;

use gpui_kit::{
    App, AppContext as _, Bounds, KeyBinding, Menu, MenuItem, WindowBounds, WindowOptions, actions,
    px, size,
};

actions!(anveg, [Quit, ToggleTab, CloseWindow]);

fn main() {
    let started = Instant::now();
    let paths = anveg_core::paths::AppPaths::for_current_user();
    let _log_guard = match &paths {
        Ok(paths) => logging::init(paths),
        Err(error) => {
            logging::init_stderr();
            tracing::warn!(%error, "no app directories; logging to stderr only");
            None
        }
    };
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "Anveg Code starting");

    gpui_kit::application()
        .with_assets(gpui_kit::assets::Assets)
        .run(move |cx: &mut App| {
            gpui_kit::init(cx);
            anveg_term::init(cx);
            install_menus_and_keys(cx);

            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(1100.), px(720.)),
                    cx,
                ))),
                ..gpui_cn::TitleBar::window_options(cx)
            };
            match gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| spike::SpikeWindow::new(window, cx))
            }) {
                Ok(_) => {
                    tracing::info!(
                        elapsed_ms = started.elapsed().as_millis() as u64,
                        "first window opened"
                    );
                }
                Err(error) => {
                    tracing::error!(%error, "could not open the window");
                    cx.quit();
                }
            }
            cx.activate(true);
        });
}

fn install_menus_and_keys(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.bind_keys([
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-t", ToggleTab, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
    ]);
    cx.set_menus([
        Menu::new("Anveg Code").items([MenuItem::action("Quit Anveg Code", Quit)]),
        Menu::new("File").items([MenuItem::action("Close Window", CloseWindow)]),
        Menu::new("View").items([MenuItem::action("Toggle Claude/Terminal", ToggleTab)]),
    ]);
    // macOS keeps an app alive without windows; for the spike, closing the
    // only window quits.
    cx.on_window_closed(|cx, _| {
        if cx.windows().is_empty() {
            cx.quit();
        }
    })
    .detach();
}
