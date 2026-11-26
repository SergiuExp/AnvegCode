//! The chime when Claude finishes a turn (`anveg_core::chime`).
//!
//! The sound is written once to `<app data>/sounds/finished-v1.wav` and
//! played with macOS's `afplay` (on Linux, for development: `paplay`, else
//! `aplay`), on a background thread so the UI never waits. Several sessions
//! finishing together give one chime.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anveg_core::config::FinishSound;
use gpui_kit::App;

use crate::model::model;

/// The file name carries a version: change it when the chime changes.
const FILE_NAME: &str = "finished-v1.wav";
/// At most one chime this often.
const MIN_GAP: Duration = Duration::from_millis(1500);

static LAST: Mutex<Option<Instant>> = Mutex::new(None);

/// Claude finished a turn in a session; `looking` says whether that
/// session is in front of the user. Plays the chime if the setting says so.
pub fn finished(looking: bool, cx: &App) {
    let play = match model(cx).read(cx).settings.finish_sound {
        FinishSound::Off => false,
        FinishSound::WhenNotLooking => !looking,
        FinishSound::Always => true,
    };
    if play {
        play_chime(cx);
    }
}

/// Play the chime now (also the Settings preview).
pub fn play_chime(cx: &App) {
    if let Ok(mut last) = LAST.lock() {
        let now = Instant::now();
        if last.is_some_and(|t| now.duration_since(t) < MIN_GAP) {
            return;
        }
        *last = Some(now);
    }
    let dir = model(cx).read(cx).paths.support_dir.join("sounds");
    let spawned = std::thread::Builder::new()
        .name("chime".into())
        .spawn(move || {
            match ensure_file(&dir) {
                Ok(path) => play_file(&path),
                Err(error) => tracing::warn!(%error, "could not write the chime"),
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not start the chime");
    }
}

fn ensure_file(dir: &Path) -> std::io::Result<PathBuf> {
    let path = dir.join(FILE_NAME);
    if path.is_file() {
        return Ok(path);
    }
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{FILE_NAME}.tmp"));
    std::fs::write(&tmp, anveg_core::chime::finished_wav())?;
    std::fs::rename(&tmp, &path)?;
    Ok(path)
}

fn players() -> &'static [&'static [&'static str]] {
    if cfg!(target_os = "macos") {
        &[&["/usr/bin/afplay"]]
    } else {
        &[&["paplay"], &["aplay", "-q"]]
    }
}

fn play_file(path: &Path) {
    for player in players() {
        let Some((program, args)) = player.split_first() else {
            continue;
        };
        let status = Command::new(program)
            .args(args.iter())
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        match status {
            Ok(s) if s.success() => return,
            Ok(s) => tracing::debug!(program, ?s, "chime player failed"),
            Err(error) => tracing::debug!(program, %error, "chime player not available"),
        }
    }
    tracing::debug!("no player could play the chime");
}
