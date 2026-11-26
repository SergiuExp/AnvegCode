//! Logging: a file in the app's logs directory plus stderr.
//!
//! GPUI logs through the `log` crate; `tracing-subscriber`'s `tracing-log`
//! bridge routes those records here too.

use anveg_core::paths::{AppPaths, LOG_FILE_NAME};
use tracing_appender::non_blocking::WorkerGuard;
use tracing::Level;
use tracing_subscriber::filter::{FilterFn, filter_fn};
use tracing_subscriber::{
    EnvFilter, Layer as _, fmt, layer::SubscriberExt as _, util::SubscriberInitExt as _,
};

const DEFAULT_FILTER: &str = "info,naga=warn,wgpu=warn,blade_graphics=warn";

fn filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
}

/// Log to `<logs_dir>/anveg-code.log` and stderr. Keep the returned guard
/// alive until exit so buffered lines are flushed.
pub fn init(paths: &AppPaths) -> Option<WorkerGuard> {
    if let Err(error) = paths.ensure_dirs() {
        init_stderr();
        tracing::warn!(%error, "could not create the app directories; logging to stderr only");
        return None;
    }
    let appender = tracing_appender::rolling::never(&paths.logs_dir, LOG_FILE_NAME);
    let (file_writer, guard) = tracing_appender::non_blocking(appender);
    let result = tracing_subscriber::registry()
        .with(filter())
        .with(fmt::layer().with_writer(std::io::stderr).with_filter(below_warn()))
        .with(located().with_writer(std::io::stderr).with_filter(warn_or_worse()))
        .with(
            fmt::layer()
                .with_ansi(false)
                .with_writer(file_writer.clone())
                .with_filter(below_warn()),
        )
        .with(
            located()
                .with_ansi(false)
                .with_writer(file_writer)
                .with_filter(warn_or_worse()),
        )
        .try_init();
    if let Err(error) = result {
        eprintln!("anveg-code: could not set up logging: {error}");
    }
    Some(guard)
}

/// Log to stderr only.
pub fn init_stderr() {
    let _ = tracing_subscriber::registry()
        .with(filter())
        .with(fmt::layer().with_writer(std::io::stderr).with_filter(below_warn()))
        .with(located().with_writer(std::io::stderr).with_filter(warn_or_worse()))
        .try_init();
}

/// Warnings and errors also show their source file and line. GPUI's own
/// errors (such as "window not found") have no target, so that is the only
/// clue to where they come from.
fn located<S>() -> fmt::Layer<S> {
    fmt::layer().with_file(true).with_line_number(true)
}

fn warn_or_worse() -> FilterFn {
    filter_fn(|meta| {
        *meta.level() <= Level::WARN && !is_closed_window_noise(meta.file(), meta.line())
    })
}

/// GPUI's window-geometry callbacks (visual viewport, insets, resize, move)
/// in `gpui-pre` 0.3.10, which is pinned exactly. When macOS closes a window
/// (the red button, ⌘W, the Dock menu's Close), GPUI forgets the window
/// first and then lets AppKit finish closing it; AppKit resizes it on the
/// way, and GPUI logs "window not found" for that resize. The window is
/// gone, so nothing is lost: these lines are dropped. After a GPUI update the
/// lines move and this simply stops matching.
const GPUI_WINDOW_RS: &str = "gpui-pre-0.3.10/src/window.rs";
const GPUI_GEOMETRY_CALLBACK_LINES: &[u32] = &[1956, 1964, 1972, 1980];

fn is_closed_window_noise(file: Option<&str>, line: Option<u32>) -> bool {
    file.is_some_and(|f| f.replace('\\', "/").ends_with(GPUI_WINDOW_RS))
        && line.is_some_and(|l| GPUI_GEOMETRY_CALLBACK_LINES.contains(&l))
}

#[cfg(test)]
mod tests {
    use super::is_closed_window_noise;

    #[test]
    fn only_gpui_window_geometry_errors_are_dropped() {
        let gpui = "/Users/me/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/gpui-pre-0.3.10/src/window.rs";
        assert!(is_closed_window_noise(Some(gpui), Some(1972)));
        assert!(is_closed_window_noise(Some(gpui), Some(1980)));
        // Other GPUI errors stay.
        assert!(!is_closed_window_noise(Some(gpui), Some(2568)));
        // Another GPUI version, or our own files, stay.
        let newer = gpui.replace("0.3.10", "0.3.11");
        assert!(!is_closed_window_noise(Some(&newer), Some(1972)));
        assert!(!is_closed_window_noise(Some("crates/anveg_app/src/window.rs"), Some(1972)));
        assert!(!is_closed_window_noise(None, Some(1972)));
    }
}

fn below_warn() -> FilterFn {
    filter_fn(|meta| *meta.level() > Level::WARN)
}
