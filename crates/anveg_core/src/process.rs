//! `ProcessRunner`: runs `git`, `gh` and similar tools (next phases, 0.2).
//!
//! - Runs with the captured login environment and a working directory.
//! - Has a timeout, can be cancelled, captures stdout and stderr, and can
//!   decode JSON output.
//! - Blocking: call it from a background thread, never the UI thread.
//! - Commands that change a repository take that repository's lock, so they
//!   run one at a time per repository (no `index.lock` conflicts).

use std::collections::HashMap;
use std::io::{Read, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use serde::de::DeserializeOwned;

use crate::claude_env::EnvMap;
use crate::op_log::{Level, OpLog};

/// How long a command may run unless it says otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Receives stderr progress lines as they arrive.
pub type Progress = Arc<dyn Fn(&str) + Send + Sync>;

/// A shared flag that cancels a running command.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// A command to run.
#[derive(Clone, Debug)]
pub struct Cmd {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// The whole environment for the child. `None` inherits the app's.
    pub env: Option<EnvMap>,
    /// Extra variables on top of `env`.
    pub extra_env: Vec<(String, String)>,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Duration,
    /// Take the repository lock at this path while running.
    pub lock: Option<PathBuf>,
    /// Record the command, its result and a summary of its output here.
    pub log: Option<OpLog>,
}

impl Cmd {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: None,
            extra_env: Vec::new(),
            stdin: None,
            timeout: DEFAULT_TIMEOUT,
            lock: None,
            log: None,
        }
    }

    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn cwd(mut self, dir: impl Into<PathBuf>) -> Self {
        self.cwd = Some(dir.into());
        self
    }

    pub fn env(mut self, env: Option<&EnvMap>) -> Self {
        self.env = env.cloned();
        self
    }

    pub fn var(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.push((name.into(), value.into()));
        self
    }

    pub fn stdin(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.stdin = Some(bytes.into());
        self
    }

    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Run while holding the lock for the repository at `repo`.
    pub fn locked(mut self, repo: impl Into<PathBuf>) -> Self {
        self.lock = Some(repo.into());
        self
    }

    /// Record this command in `log` when it runs (see [`run_with`]).
    pub fn logged(mut self, log: Option<&OpLog>) -> Self {
        self.log = log.cloned();
        self
    }

    /// The command line, for logs and error messages.
    pub fn display(&self) -> String {
        let name = self
            .program
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        std::iter::once(name)
            .chain(self.args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// What a finished command produced.
#[derive(Clone, Debug, Default)]
pub struct Output {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

impl Output {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn stdout_str(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_str(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }

    /// stderr, or stdout when stderr is empty: what a tool said about a
    /// failure.
    pub fn message(&self) -> String {
        let err = self.stderr_str();
        let text = if err.trim().is_empty() {
            self.stdout_str()
        } else {
            err
        };
        text.trim().to_owned()
    }

    /// Fail unless the exit code was 0.
    pub fn check(self) -> Result<Self> {
        if self.success() {
            Ok(self)
        } else {
            let message = self.message();
            if message.is_empty() {
                bail!("exited with code {:?}", self.code)
            }
            bail!("{message}")
        }
    }

    /// Decode stdout as JSON.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.stdout).context("unexpected output")
    }
}

/// Why a command did not finish normally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stopped {
    TimedOut,
    Cancelled,
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TimedOut => write!(f, "timed out"),
            Self::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl std::error::Error for Stopped {}

/// Whether an error came from a cancelled command.
pub fn is_cancelled(error: &anyhow::Error) -> bool {
    error.downcast_ref::<Stopped>() == Some(&Stopped::Cancelled)
}

fn repo_lock(path: &Path) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>> = OnceLock::new();
    let locks = LOCKS.get_or_init(Mutex::default);
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    match locks.lock() {
        Ok(mut map) => map.entry(key).or_default().clone(),
        Err(_) => Arc::default(),
    }
}

/// Run `cmd` to completion. Blocking.
pub fn run(cmd: &Cmd) -> Result<Output> {
    run_with(cmd, None, None)
}

/// Run `cmd`, cancellable through `cancel`. `progress` receives each line
/// (or `\r`-terminated progress update) of stderr as it arrives.
pub fn run_with(
    cmd: &Cmd,
    cancel: Option<&CancelToken>,
    progress: Option<Progress>,
) -> Result<Output> {
    let Some(log) = &cmd.log else {
        return run_unlogged(cmd, cancel, progress);
    };
    let label = log_label(cmd);
    log.push(Level::Start, label.clone());
    let started = Instant::now();
    let result = run_unlogged(cmd, cancel, progress);
    let ms = started.elapsed().as_millis();
    match &result {
        Ok(out) if out.success() => {
            for line in output_summary(out) {
                log.detail(line);
            }
            log.push(Level::Ok, format!("{label} ({ms} ms)"));
        }
        Ok(out) => {
            let message = match out.message() {
                m if m.is_empty() => format!("exited with code {:?}", out.code),
                m => capped(&last_states(&m)).join("\n"),
            };
            log.push(Level::Failed, format!("{label}: {} ({ms} ms)", redact_urls(&message)));
        }
        Err(e) => log.push(Level::Failed, format!("{label}: {} ({ms} ms)", redact_urls(&format!("{e:#}")))),
    }
    result
}

/// Lines of output kept in the log after a command.
const SUMMARY_LINES: usize = 12;

/// "repo: git commit -m \"Fix it\"": the folder it ran in, the program, and
/// its arguments (git's own `-c` settings left out, long ones shortened,
/// passwords in URLs hidden).
fn log_label(cmd: &Cmd) -> String {
    let program = cmd
        .program
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut args = cmd.args.as_slice();
    while program == "git" && args.first().is_some_and(|a| a == "-c") && args.len() >= 2 {
        args = &args[2..];
    }
    let mut line = program;
    for arg in args {
        let mut a: String = arg.chars().take(60).collect();
        if arg.chars().count() > 60 {
            a.push('…');
        }
        let a = a.replace('\n', " ");
        line.push(' ');
        if a.is_empty() || a.contains(char::is_whitespace) || a.contains('"') {
            line.push_str(&format!("\"{}\"", a.replace('"', "\\\"")));
        } else {
            line.push_str(&a);
        }
    }
    let line = redact_urls(&line);
    match cmd.cwd.as_deref().and_then(Path::file_name) {
        Some(dir) => format!("{}: {line}", dir.to_string_lossy()),
        None => line,
    }
}

/// What a command printed (stdout, then stderr), one line per line with
/// progress updates reduced to their final state, at most
/// [`SUMMARY_LINES`].
fn output_summary(out: &Output) -> Vec<String> {
    let text = format!("{}\n{}", out.stdout_str(), out.stderr_str());
    capped(&last_states(&text))
        .into_iter()
        .map(|l| redact_urls(&l))
        .collect()
}

/// Each line's final state: git redraws progress with `\r`.
fn last_states(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.split('\r').map(str::trim).rfind(|s| !s.is_empty()))
        .map(str::to_owned)
        .collect()
}

fn capped(lines: &[String]) -> Vec<String> {
    let mut out: Vec<String> = lines.iter().take(SUMMARY_LINES).cloned().collect();
    if lines.len() > SUMMARY_LINES {
        out.push(format!("… {} more lines", lines.len() - SUMMARY_LINES));
    }
    out
}

/// Hide the user and password in URLs: `https://me:tok@host/x` becomes
/// `https://***@host/x`.
pub fn redact_urls(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find("://") {
        let (head, tail) = rest.split_at(i + 3);
        out.push_str(head);
        let end = tail
            .find(|c: char| c == '/' || c.is_whitespace() || c == '\'' || c == '"')
            .unwrap_or(tail.len());
        let authority = &tail[..end];
        match authority.rfind('@') {
            Some(at) => {
                out.push_str("***");
                out.push_str(&authority[at..]);
            }
            None => out.push_str(authority),
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn run_unlogged(
    cmd: &Cmd,
    cancel: Option<&CancelToken>,
    progress: Option<Progress>,
) -> Result<Output> {
    let lock = cmd.lock.as_deref().map(repo_lock);
    let _guard = match &lock {
        Some(lock) => Some(lock.lock().map_err(|_| anyhow!("repository lock poisoned"))?),
        None => None,
    };

    let mut command = Command::new(&cmd.program);
    command
        .args(&cmd.args)
        .stdin(if cmd.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = &cmd.cwd {
        command.current_dir(dir);
    }
    if let Some(env) = &cmd.env {
        command.env_clear();
        command.envs(env);
    }
    for (k, v) in &cmd.extra_env {
        command.env(k, v);
    }
    let mut child = command
        .spawn()
        .with_context(|| format!("could not run {}", cmd.display()))?;

    if let (Some(bytes), Some(mut stdin)) = (cmd.stdin.clone(), child.stdin.take()) {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&bytes);
        });
    }
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut s) = stdout {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let Some(mut s) = stderr else {
            return buf;
        };
        let mut chunk = [0u8; 4096];
        let mut line = Vec::new();
        loop {
            match s.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(progress) = &progress {
                        for &b in &chunk[..n] {
                            if b == b'\n' || b == b'\r' {
                                if !line.is_empty() {
                                    progress(&String::from_utf8_lossy(&line));
                                    line.clear();
                                }
                            } else {
                                line.push(b);
                            }
                        }
                    }
                }
            }
        }
        buf
    });

    let started = Instant::now();
    let mut stopped = None;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if cancel.is_some_and(CancelToken::is_cancelled) {
            stopped = Some(Stopped::Cancelled);
        } else if started.elapsed() > cmd.timeout {
            stopped = Some(Stopped::TimedOut);
        }
        if stopped.is_some() {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let stdout = out_thread.join().unwrap_or_default();
    let stderr = err_thread.join().unwrap_or_default();
    if let Some(stopped) = stopped {
        tracing::debug!(cmd = %cmd.display(), %stopped, "command stopped");
        return Err(stopped.into());
    }
    Ok(Output {
        code: status.and_then(|s| s.code()),
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logged_commands() -> Result<()> {
        let log = OpLog::new();
        let dir = tempfile::tempdir()?;
        let ok = Cmd::new("sh")
            .args(["-c", "printf 'Counting 1%%\\rCounting 100%%, done.\\n' >&2; echo https://me:tok@h/x"])
            .cwd(dir.path())
            .logged(Some(&log));
        run(&ok)?;
        let failed = Cmd::new("sh").args(["-c", "echo nope >&2; exit 1"]).logged(Some(&log));
        assert!(!run(&failed)?.success());
        let text = log.text();
        let name = dir.path().file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        assert!(text.contains(&format!("▶ {name}: sh -c \"printf")), "{text}");
        assert!(text.contains("Counting 100%, done."), "{text}");
        assert!(!text.contains("Counting 1%\n"), "{text}");
        assert!(text.contains("https://***@h/x") && !text.contains("tok"), "{text}");
        assert!(text.contains("✗ sh -c \"echo nope >&2; exit 1\": nope ("), "{text}");
        // Unlogged commands leave nothing.
        let before = log.len();
        run(&Cmd::new("sh").args(["-c", "true"]))?;
        assert_eq!(log.len(), before);
        Ok(())
    }

    #[test]
    fn hides_url_passwords() {
        assert_eq!(redact_urls("https://u:p@github.com/o/r.git"), "https://***@github.com/o/r.git");
        assert_eq!(redact_urls("https://github.com/o/r"), "https://github.com/o/r");
        assert_eq!(redact_urls("a ssh://git@h:22/x b http://t@h"), "a ssh://***@h:22/x b http://***@h");
    }

    #[test]
    fn captures_output_and_exit_code() -> Result<()> {
        let out = run(&Cmd::new("sh").args(["-c", "echo out; echo err >&2; exit 3"]))?;
        assert_eq!(out.code, Some(3));
        assert_eq!(out.stdout_str(), "out\n");
        assert_eq!(out.stderr_str(), "err\n");
        assert!(out.check().is_err());
        Ok(())
    }

    #[test]
    fn passes_stdin_cwd_and_env() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let out = run(&Cmd::new("sh")
            .args(["-c", "cat; pwd; echo $FOO"])
            .cwd(tmp.path())
            .var("FOO", "bar")
            .stdin("in\n"))?
        .check()?;
        let text = out.stdout_str();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "in");
        assert!(lines[1].ends_with(tmp.path().file_name().and_then(|n| n.to_str()).unwrap_or("?")));
        assert_eq!(lines[2], "bar");
        Ok(())
    }

    #[test]
    fn times_out_and_cancels() {
        let slow = Cmd::new("sleep").arg("5").timeout(Duration::from_millis(100));
        let started = Instant::now();
        let err = run(&slow).err();
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(err.is_some_and(|e| e.downcast_ref::<Stopped>() == Some(&Stopped::TimedOut)));

        let cancel = CancelToken::new();
        let c2 = cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            c2.cancel();
        });
        let err = run_with(&Cmd::new("sleep").arg("5"), Some(&cancel), None).err();
        assert!(err.is_some_and(|e| is_cancelled(&e)));
    }

    #[test]
    fn decodes_json_and_reports_progress() -> Result<()> {
        #[derive(serde::Deserialize)]
        struct V {
            a: u32,
        }
        let out = run(&Cmd::new("sh").args(["-c", r#"printf '{"a": 7}'"#]))?;
        assert_eq!(out.json::<V>()?.a, 7);

        let seen = Arc::new(Mutex::new(Vec::<String>::new()));
        let s2 = seen.clone();
        run_with(
            &Cmd::new("sh").args(["-c", r"printf 'one\rtwo\nthree\n' >&2"]),
            None,
            Some(Arc::new(move |line: &str| {
                if let Ok(mut s) = s2.lock() {
                    s.push(line.to_owned());
                }
            })),
        )?;
        let seen = seen.lock().map(|s| s.clone()).unwrap_or_default();
        assert_eq!(seen, ["one", "two", "three"]);
        Ok(())
    }

    #[test]
    fn locked_commands_run_one_at_a_time() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let marker = tmp.path().join("busy");
        let script = format!(
            "if [ -e {m} ]; then echo overlap; fi; touch {m}; sleep 0.1; rm {m}",
            m = marker.display()
        );
        let handles: Vec<_> = (0..3)
            .map(|_| {
                let cmd = Cmd::new("sh").args(["-c", &script]).locked(tmp.path());
                std::thread::spawn(move || run(&cmd))
            })
            .collect();
        for h in handles {
            let out = h.join().map_err(|_| anyhow!("thread panicked"))??;
            assert!(!out.stdout_str().contains("overlap"));
        }
        Ok(())
    }
}
