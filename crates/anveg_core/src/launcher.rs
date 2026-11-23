//! What to run in a terminal. The command-line builders for `claude`
//! (new / resume / fork) arrive in Milestone 5; for now this holds the
//! engine-independent description of a process to start.

use std::path::PathBuf;

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
    /// Extra environment variables, applied on top of the inherited ones.
    pub env: Vec<(String, String)>,
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
            login: false,
        }
    }

    /// Add an environment variable.
    #[must_use]
    pub fn with_env(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((name.into(), value.into()));
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_shell_has_no_program() {
        let spec = LaunchSpec::login_shell(None);
        assert!(spec.program.is_none());
        assert!(spec.login);
    }

    #[test]
    fn program_keeps_arguments_in_order() {
        let spec = LaunchSpec::program("claude", ["--resume", "abc"], Some("/p".into()))
            .with_env("A", "1");
        assert_eq!(
            spec.program.as_deref(),
            Some(std::path::Path::new("claude"))
        );
        assert_eq!(spec.args, vec!["--resume", "abc"]);
        assert_eq!(spec.env, vec![("A".to_owned(), "1".to_owned())]);
        assert!(!spec.login);
    }
}
