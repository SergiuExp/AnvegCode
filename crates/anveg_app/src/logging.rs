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
    filter_fn(|meta| *meta.level() <= Level::WARN)
}

fn below_warn() -> FilterFn {
    filter_fn(|meta| *meta.level() > Level::WARN)
}
