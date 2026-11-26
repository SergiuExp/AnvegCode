//! Where the app keeps its own files (section 11 of the build plan).

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};

/// The folder name used under `Application Support` and `Logs` on macOS.
pub const APP_DIR_NAME: &str = "Anveg Code";

/// The folder name debug builds use instead of [`APP_DIR_NAME`], so a
/// `cargo run` never reads or changes the installed app's settings.
pub const DEV_APP_DIR_NAME: &str = "Anveg Code Dev";

/// Set to a directory to keep everything (settings and logs) there instead,
/// e.g. a throwaway sandbox: `ANVEG_HOME=/tmp/anveg cargo run`.
pub const HOME_ENV: &str = "ANVEG_HOME";

/// The name of the log file inside [`AppPaths::logs_dir`].
pub const LOG_FILE_NAME: &str = "anveg-code.log";

/// The directories the app owns.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    /// Settings, projects, window state and caches.
    pub support_dir: PathBuf,
    /// The log file's directory.
    pub logs_dir: PathBuf,
}

impl AppPaths {
    /// The directories for the current user on this platform: `$ANVEG_HOME`
    /// if set, else the standard folders ([`DEV_APP_DIR_NAME`] in debug
    /// builds, so they never touch the installed app's files).
    pub fn for_current_user() -> Result<Self> {
        if let Some(home) = std::env::var_os(HOME_ENV).filter(|v| !v.is_empty()) {
            return Ok(Self::in_dir(Path::new(&home)));
        }
        let base = directories::BaseDirs::new().context("could not find the home directory")?;
        Ok(Self::from_base(
            base.home_dir(),
            base.data_dir(),
            base.cache_dir(),
            cfg!(debug_assertions),
        ))
    }

    /// Everything under one directory (`$ANVEG_HOME`).
    fn in_dir(dir: &Path) -> Self {
        Self {
            support_dir: dir.join("support"),
            logs_dir: dir.join("logs"),
        }
    }

    /// Build the paths from a home directory (macOS) or from the platform's
    /// data and cache directories (other platforms, used only for
    /// development builds: macOS is the only supported target).
    fn from_base(home: &Path, data_dir: &Path, cache_dir: &Path, dev: bool) -> Self {
        if cfg!(target_os = "macos") {
            let name = if dev { DEV_APP_DIR_NAME } else { APP_DIR_NAME };
            let library = home.join("Library");
            Self {
                support_dir: library.join("Application Support").join(name),
                logs_dir: library.join("Logs").join(name),
            }
        } else {
            let name = if dev { "anveg-code-dev" } else { "anveg-code" };
            Self {
                support_dir: data_dir.join(name),
                logs_dir: cache_dir.join(name).join("logs"),
            }
        }
    }

    /// True for a debug build's or `$ANVEG_HOME`'s directories: not the
    /// installed app's.
    pub fn is_sandbox(&self) -> bool {
        !self
            .support_dir
            .file_name()
            .is_some_and(|n| n == APP_DIR_NAME || n == "anveg-code")
    }

    /// `settings.json`.
    pub fn settings_file(&self) -> PathBuf {
        self.support_dir.join("settings.json")
    }

    /// `projects.json`.
    pub fn projects_file(&self) -> PathBuf {
        self.support_dir.join("projects.json")
    }

    /// `state.json`: open windows, bounds, selections.
    pub fn state_file(&self) -> PathBuf {
        self.support_dir.join("state.json")
    }

    /// `sessions/<project-id>.json`: aliases and the transcript cache.
    pub fn sessions_dir(&self) -> PathBuf {
        self.support_dir.join("sessions")
    }

    /// `notes/<project-id>.json`: the Notes panel's tabs and notes.
    pub fn notes_file(&self, project_id: &str) -> PathBuf {
        self.support_dir.join("notes").join(format!("{project_id}.json"))
    }

    /// `activity/<project-id>.json`: the Git activity log (pushes, pulls,
    /// fetches).
    pub fn activity_file(&self, project_id: &str) -> PathBuf {
        self.support_dir.join("activity").join(format!("{project_id}.json"))
    }

    /// Create the directories if they do not exist.
    pub fn ensure_dirs(&self) -> Result<()> {
        for dir in [&self.support_dir, &self.logs_dir] {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("could not create {}", dir.display()))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_live_in_the_support_dir() {
        let paths = AppPaths::from_base(
            Path::new("/home/u"),
            Path::new("/home/u/.local/share"),
            Path::new("/home/u/.cache"),
            false,
        );
        assert_eq!(
            paths.settings_file(),
            paths.support_dir.join("settings.json")
        );
        assert_eq!(
            paths.projects_file(),
            paths.support_dir.join("projects.json")
        );
        assert_eq!(paths.state_file(), paths.support_dir.join("state.json"));
        assert_eq!(paths.sessions_dir(), paths.support_dir.join("sessions"));
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_layout_matches_the_plan() {
        let paths =
            AppPaths::from_base(Path::new("/Users/u"), Path::new("x"), Path::new("y"), false);
        assert_eq!(
            paths.support_dir,
            Path::new("/Users/u/Library/Application Support/Anveg Code")
        );
        assert_eq!(
            paths.logs_dir,
            Path::new("/Users/u/Library/Logs/Anveg Code")
        );
        assert!(!paths.is_sandbox());
    }

    #[test]
    fn dev_builds_use_their_own_dirs() {
        let base = |dev| {
            AppPaths::from_base(
                Path::new("/Users/u"),
                Path::new("/Users/u/.local/share"),
                Path::new("/Users/u/.cache"),
                dev,
            )
        };
        let (installed, dev) = (base(false), base(true));
        assert_ne!(installed.support_dir, dev.support_dir);
        assert_ne!(installed.logs_dir, dev.logs_dir);
        assert!(dev.is_sandbox());
        assert!(AppPaths::in_dir(Path::new("/tmp/x")).is_sandbox());
    }

    #[test]
    fn ensure_dirs_creates_both() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let paths = AppPaths {
            support_dir: tmp.path().join("a/support"),
            logs_dir: tmp.path().join("b/logs"),
        };
        paths.ensure_dirs()?;
        assert!(paths.support_dir.is_dir());
        assert!(paths.logs_dir.is_dir());
        Ok(())
    }
}
