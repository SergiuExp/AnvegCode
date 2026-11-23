//! What to run in a terminal, and the `claude` command lines for new,
//! resumed, forked and worktree sessions (build plan, section 5.4).

use std::path::{Path, PathBuf};

use crate::claude_env::{EnvMap, INHERITED_CLAUDE_MARKERS};

/// The environment variable that tells `anveg-hook` where the app listens.
pub const HOOK_SOCKET_ENV: &str = "ANVEG_HOOK_SOCKET";

/// A process to start in a pseudo-terminal.
///
/// This is plain data so the core stays GUI-free; `anveg_term` turns it
/// into whatever the terminal engine needs.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchSpec {
    /// The program. `None` runs the user's shell.
    pub program: Option<PathBuf>,
    /// Arguments, not including the program itself.
    pub args: Vec<String>,
    /// The working directory. `None` uses the home directory.
    pub cwd: Option<PathBuf>,
    /// Environment variables, applied on top of the inherited ones.
    pub env: Vec<(String, String)>,
    /// Inherited variables to remove.
    pub env_remove: Vec<String>,
    /// Start the shell as a login shell (only used when `program` is `None`).
    pub login: bool,
}

impl LaunchSpec {
    /// The user's shell as a login shell.
    pub fn login_shell(cwd: Option<PathBuf>) -> Self {
        Self {
            program: None,
            args: Vec::new(),
            cwd,
            env: Vec::new(),
            env_remove: Vec::new(),
            login: true,
        }
    }

    /// A program with arguments.
    pub fn program(
        program: impl Into<PathBuf>,
        args: impl IntoIterator<Item = impl Into<String>>,
        cwd: Option<PathBuf>,
    ) -> Self {
        Self {
            program: Some(program.into()),
            args: args.into_iter().map(Into::into).collect(),
            cwd,
            env: Vec::new(),
            env_remove: Vec::new(),
            login: false,
        }
    }

    /// Add an environment variable.
    #[must_use]
    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }

    /// Use `env` as the child environment (the captured login environment),
    /// and remove the Claude Code markers a parent session may have set.
    #[must_use]
    pub fn with_child_env(mut self, env: &EnvMap) -> Self {
        for (name, value) in env {
            // The terminal engine sets these itself.
            if matches!(
                name.as_str(),
                "TERM" | "TERMINFO" | "COLORTERM" | "TERM_PROGRAM" | "TERM_PROGRAM_VERSION"
            ) {
                continue;
            }
            self.env.push((name.clone(), value.clone()));
        }
        self.env_remove
            .extend(INHERITED_CLAUDE_MARKERS.iter().map(|s| (*s).to_owned()));
        self
    }
}

/// How a `claude` session starts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStart {
    /// A new session with an id the app chose.
    New {
        session_id: String,
        name: Option<String>,
    },
    /// Continue an existing session.
    Resume { session_id: String },
    /// Copy an existing session into a new one, with an id the app chose
    /// (`--resume <from> --fork-session --session-id <new>`).
    Fork { from: String, session_id: String },
    /// A new session in a new git worktree `<main>/.claude/worktrees/<name>`.
    Worktree {
        worktree: String,
        session_id: String,
        name: Option<String>,
    },
}

impl SessionStart {
    /// The id the session will have.
    pub fn session_id(&self) -> &str {
        match self {
            Self::New { session_id, .. }
            | Self::Resume { session_id }
            | Self::Fork { session_id, .. }
            | Self::Worktree { session_id, .. } => session_id,
        }
    }
}

/// Everything besides the start mode that goes into a `claude` command line.
#[derive(Clone, Debug, Default)]
pub struct ClaudeOptions {
    /// The project's extra folders (`--add-dir`, passed again on every
    /// resume because Claude Code does not restore them).
    pub extra_folders: Vec<PathBuf>,
    /// Settings ▸ Claude ▸ Default extra flags, already split.
    pub default_flags: Vec<String>,
    /// Phase 2: the app's hooks settings file (`--settings`).
    pub hooks_settings: Option<PathBuf>,
    /// Phase 2: the socket `anveg-hook` reports to.
    pub hook_socket: Option<PathBuf>,
}

/// The arguments for `claude`.
pub fn claude_args(start: &SessionStart, options: &ClaudeOptions) -> Vec<String> {
    let mut args = Vec::new();
    match start {
        SessionStart::New { session_id, name } => {
            args.extend(["--session-id".to_owned(), session_id.clone()]);
            push_name(&mut args, name.as_deref());
        }
        SessionStart::Resume { session_id } => {
            args.extend(["--resume".to_owned(), session_id.clone()]);
        }
        SessionStart::Fork { from, session_id } => {
            args.extend([
                "--resume".to_owned(),
                from.clone(),
                "--fork-session".to_owned(),
                "--session-id".to_owned(),
                session_id.clone(),
            ]);
        }
        SessionStart::Worktree {
            worktree,
            session_id,
            name,
        } => {
            args.extend([
                "--worktree".to_owned(),
                worktree.clone(),
                "--session-id".to_owned(),
                session_id.clone(),
            ]);
            push_name(&mut args, name.as_deref());
        }
    }
    for folder in &options.extra_folders {
        args.push("--add-dir".to_owned());
        args.push(folder.to_string_lossy().into_owned());
    }
    if let Some(settings) = &options.hooks_settings {
        args.push("--settings".to_owned());
        args.push(settings.to_string_lossy().into_owned());
    }
    args.extend(options.default_flags.iter().cloned());
    args
}

fn push_name(args: &mut Vec<String>, name: Option<&str>) {
    if let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) {
        args.push("--name".to_owned());
        args.push(name.to_owned());
    }
}

/// The full launch for a `claude` session in `cwd` (the project's main
/// folder).
pub fn claude_launch(
    claude: &Path,
    cwd: &Path,
    start: &SessionStart,
    options: &ClaudeOptions,
    env: &EnvMap,
) -> LaunchSpec {
    let mut spec =
        LaunchSpec::program(claude, claude_args(start, options), Some(cwd.to_path_buf()))
            .with_child_env(env);
    if let Some(socket) = &options.hook_socket {
        spec = spec.with_env(HOOK_SOCKET_ENV, socket.to_string_lossy());
    }
    spec
}

/// A new random session id (UUID v4).
pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Validate and normalise a session name typed by the user: trimmed,
/// non-empty, one line, at most 100 characters.
pub fn normalize_name(input: &str) -> Result<String, &'static str> {
    let name = input.trim();
    if name.is_empty() {
        return Err("The name can't be empty.");
    }
    if name.contains(['\n', '\r']) {
        return Err("The name must be a single line.");
    }
    if name.chars().count() > 100 {
        return Err("The name can be at most 100 characters.");
    }
    Ok(name.to_owned())
}

/// Validate a worktree name: letters, digits, `-`, `_` and `.`, not starting
/// with `.` or `-`.
pub fn normalize_worktree_name(input: &str) -> Result<String, &'static str> {
    let name = input.trim();
    if name.is_empty() {
        return Err("The worktree name can't be empty.");
    }
    if name.starts_with(['.', '-']) {
        return Err("The worktree name can't start with '.' or '-'.");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    {
        return Err("Use only letters, digits, '-', '_' and '.'.");
    }
    Ok(name.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opts() -> ClaudeOptions {
        ClaudeOptions {
            extra_folders: vec!["/x/shared lib".into(), "/y".into()],
            default_flags: vec!["--permission-mode".into(), "plan".into()],
            hooks_settings: None,
            hook_socket: None,
        }
    }

    #[test]
    fn new_session_args() {
        let start = SessionStart::New {
            session_id: "id-1".into(),
            name: Some("  My name ".into()),
        };
        assert_eq!(
            claude_args(&start, &opts()),
            vec![
                "--session-id",
                "id-1",
                "--name",
                "My name",
                "--add-dir",
                "/x/shared lib",
                "--add-dir",
                "/y",
                "--permission-mode",
                "plan"
            ]
        );
    }

    #[test]
    fn new_session_without_name() {
        let start = SessionStart::New {
            session_id: "id-1".into(),
            name: Some("   ".into()),
        };
        let args = claude_args(&start, &ClaudeOptions::default());
        assert_eq!(args, vec!["--session-id", "id-1"]);
    }

    #[test]
    fn resume_passes_add_dir_again() {
        let start = SessionStart::Resume {
            session_id: "id-2".into(),
        };
        let args = claude_args(&start, &opts());
        assert_eq!(&args[..2], &["--resume", "id-2"]);
        assert!(args.windows(2).any(|w| w == ["--add-dir", "/y"]));
        assert!(!args.contains(&"--session-id".to_owned()));
    }

    #[test]
    fn fork_and_worktree_args() {
        let fork = SessionStart::Fork {
            from: "a".into(),
            session_id: "b".into(),
        };
        assert_eq!(
            claude_args(&fork, &ClaudeOptions::default()),
            vec!["--resume", "a", "--fork-session", "--session-id", "b"]
        );
        let wt = SessionStart::Worktree {
            worktree: "feature".into(),
            session_id: "c".into(),
            name: None,
        };
        assert_eq!(
            claude_args(&wt, &ClaudeOptions::default()),
            vec!["--worktree", "feature", "--session-id", "c"]
        );
        assert_eq!(wt.session_id(), "c");
    }

    #[test]
    fn hooks_settings_and_socket() {
        let options = ClaudeOptions {
            hooks_settings: Some("/app/hooks-settings.json".into()),
            hook_socket: Some("/app/run/hooks.sock".into()),
            ..ClaudeOptions::default()
        };
        let start = SessionStart::Resume {
            session_id: "s".into(),
        };
        let mut env = EnvMap::new();
        env.insert("PATH".into(), "/bin".into());
        env.insert("TERM".into(), "dumb".into());
        let spec = claude_launch(
            Path::new("/bin/claude"),
            Path::new("/p"),
            &start,
            &options,
            &env,
        );
        assert!(
            spec.args
                .windows(2)
                .any(|w| w == ["--settings", "/app/hooks-settings.json"])
        );
        assert!(
            spec.env
                .contains(&(HOOK_SOCKET_ENV.into(), "/app/run/hooks.sock".into()))
        );
        assert!(spec.env.contains(&("PATH".into(), "/bin".into())));
        assert!(!spec.env.iter().any(|(k, _)| k == "TERM"));
        assert!(spec.env_remove.contains(&"CLAUDECODE".to_owned()));
        assert_eq!(spec.cwd.as_deref(), Some(Path::new("/p")));
        assert!(!spec.login);
    }

    #[test]
    fn names_are_validated() {
        assert_eq!(normalize_name("  hi  "), Ok("hi".to_owned()));
        assert!(normalize_name("   ").is_err());
        assert!(normalize_name("a\nb").is_err());
        assert!(normalize_name(&"x".repeat(101)).is_err());
        assert!(normalize_name(&"é".repeat(100)).is_ok());
    }

    #[test]
    fn worktree_names_are_validated() {
        assert_eq!(normalize_worktree_name("feat-1"), Ok("feat-1".to_owned()));
        assert!(normalize_worktree_name("../x").is_err());
        assert!(normalize_worktree_name("a b").is_err());
        assert!(normalize_worktree_name("-x").is_err());
    }

    #[test]
    fn session_ids_are_uuids() {
        let id = new_session_id();
        assert!(uuid::Uuid::parse_str(&id).is_ok());
    }
}
