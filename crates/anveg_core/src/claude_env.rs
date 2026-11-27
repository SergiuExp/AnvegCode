//! The environment child processes run in, and how the app finds and asks
//! about `claude` (build plan, section 5.1).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

/// Environment variables as name → value.
pub type EnvMap = BTreeMap<String, String>;

/// The marker printed before `env -0`, so shell banners and noise before it
/// are ignored.
const ENV_MARKER: &str = "__ANVEG_ENV_BEGIN_7f3a__";

/// Variables a Claude Code session sets for its own child processes. When
/// Anveg Code is started from inside a Claude Code terminal they are
/// inherited, and a `claude` that sees them turns transcript saving off
/// (observed with Claude Code 2.1.330). They are removed from every child.
pub const INHERITED_CLAUDE_MARKERS: &[&str] = &[
    "CLAUDECODE",
    "CLAUDE_CODE_CHILD_SESSION",
    "CLAUDE_CODE_SESSION_ID",
    "CLAUDE_CODE_ENTRYPOINT",
    "CLAUDE_PID",
];

/// The user's login shell: `$SHELL`, else `/bin/zsh` (the macOS default).
pub fn user_shell() -> PathBuf {
    std::env::var_os("SHELL")
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/bin/zsh"))
}

/// Run `cmd` and collect its stdout, killing it after `timeout`.
pub fn output_with_timeout(mut cmd: Command, timeout: Duration) -> Result<(i32, Vec<u8>)> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = cmd.spawn().context("could not start")?;
    let mut stdout = child.stdout.take().context("no stdout")?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("timed out after {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = reader.join().unwrap_or_default();
    Ok((status.code().unwrap_or(-1), out))
}

/// Parse `env -0` output that follows [`ENV_MARKER`].
pub fn parse_env_output(bytes: &[u8]) -> Option<EnvMap> {
    let marker = ENV_MARKER.as_bytes();
    let start = bytes.windows(marker.len()).rposition(|w| w == marker)? + marker.len();
    let mut env = EnvMap::new();
    for entry in bytes[start..].split(|b| *b == 0) {
        let entry = String::from_utf8_lossy(entry);
        // Strip a newline the marker's `printf` might leave.
        let entry = entry.trim_start_matches('\n');
        if let Some((name, value)) = entry.split_once('=')
            && !name.is_empty()
            && !name.contains(char::is_whitespace)
        {
            env.insert(name.to_owned(), value.to_owned());
        }
    }
    (!env.is_empty()).then_some(env)
}

/// Capture the login shell's environment, as a terminal would see it.
///
/// Tries `$SHELL -l -i -c …` first, then without `-i`. Returns `None` when
/// both fail; callers then use the app's own environment.
pub fn capture_login_env(shell: &Path, timeout: Duration) -> Option<EnvMap> {
    let script = format!("printf '%s' '{ENV_MARKER}'; command env -0");
    for interactive in [true, false] {
        let mut cmd = Command::new(shell);
        cmd.arg("-l");
        if interactive {
            cmd.arg("-i");
        }
        cmd.arg("-c").arg(&script);
        for name in INHERITED_CLAUDE_MARKERS {
            cmd.env_remove(name);
        }
        match output_with_timeout(cmd, timeout) {
            Ok((_, out)) => {
                if let Some(env) = parse_env_output(&out) {
                    tracing::info!(
                        interactive,
                        vars = env.len(),
                        "captured login-shell environment"
                    );
                    return Some(env);
                }
                tracing::warn!(interactive, "login shell printed no environment");
            }
            Err(error) => {
                tracing::warn!(interactive, %error, "login-shell environment capture failed")
            }
        }
    }
    None
}

/// The environment for child processes: the captured login environment
/// (or the app's own), without the inherited Claude Code markers.
pub fn child_env(captured: Option<&EnvMap>) -> EnvMap {
    let mut env: EnvMap = match captured {
        Some(env) => env.clone(),
        None => std::env::vars().collect(),
    };
    for name in INHERITED_CLAUDE_MARKERS {
        env.remove(*name);
    }
    env
}

/// Find an executable named `name` on a `PATH` value.
pub fn find_in_path(name: &str, path_var: Option<&str>) -> Option<PathBuf> {
    let path_var = path_var?;
    std::env::split_paths(OsStr::new(path_var))
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable(candidate))
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::metadata(path)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Where `claude` is, given the Settings override and the child environment.
pub fn resolve_claude(override_path: Option<&Path>, env: &EnvMap) -> Option<PathBuf> {
    if let Some(path) = override_path.filter(|p| !p.as_os_str().is_empty()) {
        return Some(path.to_path_buf());
    }
    find_in_path("claude", env.get("PATH").map(String::as_str)).or_else(|| {
        // Common install locations, for when PATH capture failed.
        let home = env
            .get("HOME")
            .cloned()
            .or_else(|| std::env::var("HOME").ok())?;
        [
            format!("{home}/.local/bin/claude"),
            format!("{home}/.claude/local/claude"),
            "/opt/homebrew/bin/claude".to_owned(),
            "/usr/local/bin/claude".to_owned(),
        ]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| is_executable(p))
    })
}

fn claude_command(claude: &Path, env: &EnvMap, args: &[&str]) -> Command {
    let mut cmd = Command::new(claude);
    cmd.args(args).env_clear().envs(env);
    cmd
}

/// `claude --version`, trimmed (e.g. `2.1.330 (Claude Code)`).
pub fn claude_version(claude: &Path, env: &EnvMap) -> Result<String> {
    let (code, out) = output_with_timeout(
        claude_command(claude, env, &["--version"]),
        Duration::from_secs(10),
    )?;
    if code != 0 {
        bail!("claude --version exited with {code}");
    }
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct AuthStatus {
    config_directory: Option<PathBuf>,
}

/// Claude Code's config directory: `configDirectory` from
/// `claude auth status` (v2.1.268+), else `$CLAUDE_CONFIG_DIR`, else
/// `~/.claude`.
pub fn claude_config_dir(claude: Option<&Path>, env: &EnvMap) -> PathBuf {
    if let Some(claude) = claude {
        let cmd = claude_command(claude, env, &["auth", "status"]);
        match output_with_timeout(cmd, Duration::from_secs(10)) {
            Ok((_, out)) => {
                if let Some(dir) = parse_auth_status_config_dir(&out) {
                    return dir;
                }
            }
            Err(error) => tracing::warn!(%error, "claude auth status failed"),
        }
    }
    fallback_config_dir(env)
}

/// The `configDirectory` field of `claude auth status` output.
pub fn parse_auth_status_config_dir(out: &[u8]) -> Option<PathBuf> {
    // Some versions print text before the JSON object.
    let text = String::from_utf8_lossy(out);
    let start = text.find('{')?;
    let status: AuthStatus = serde_json::from_str(&text[start..]).ok()?;
    status.config_directory
}

/// The user's own status line command, if they set one: from the project's
/// `.claude/settings.local.json`, then `.claude/settings.json`, then
/// `<config dir>/settings.json` (Claude Code's precedence). Only read.
pub fn user_status_line(config_dir: &Path, project_dir: &Path) -> Option<String> {
    let files = [
        project_dir.join(".claude").join("settings.local.json"),
        project_dir.join(".claude").join("settings.json"),
        config_dir.join("settings.json"),
    ];
    files.iter().find_map(|file| {
        let text = std::fs::read_to_string(file).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        let line = value.get("statusLine")?;
        if line.get("type").and_then(|t| t.as_str()).unwrap_or("command") != "command" {
            return None;
        }
        line.get("command")
            .and_then(|c| c.as_str())
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .map(str::to_owned)
    })
}

/// Claude Code deletes sessions untouched for this many days at startup
/// (`cleanupPeriodDays`; 30 when not set). Read with the same precedence as
/// [`user_status_line`]. Only read.
pub fn cleanup_period_days(config_dir: &Path, project_dir: &Path) -> u32 {
    let files = [
        project_dir.join(".claude").join("settings.local.json"),
        project_dir.join(".claude").join("settings.json"),
        config_dir.join("settings.json"),
    ];
    files
        .iter()
        .find_map(|file| {
            let text = std::fs::read_to_string(file).ok()?;
            let value: serde_json::Value = serde_json::from_str(&text).ok()?;
            let days = value.get("cleanupPeriodDays")?.as_u64()?;
            u32::try_from(days).ok()
        })
        .unwrap_or(30)
}

/// `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn fallback_config_dir(env: &EnvMap) -> PathBuf {
    if let Some(dir) = env.get("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return PathBuf::from(dir);
    }
    let home = env
        .get("HOME")
        .cloned()
        .or_else(|| std::env::var("HOME").ok())
        .unwrap_or_else(|| "/".to_owned());
    Path::new(&home).join(".claude")
}

/// One entry of `claude agents --json`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub session_id: String,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub cwd: Option<PathBuf>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    /// When it started, in Unix milliseconds.
    #[serde(default)]
    pub started_at: Option<i64>,
}

/// Parse `claude agents --json` output.
pub fn parse_agents(out: &[u8]) -> Result<Vec<AgentInfo>> {
    let text = String::from_utf8_lossy(out);
    let start = text.find('[').context("no JSON array")?;
    Ok(serde_json::from_str(&text[start..])?)
}

/// Run `claude agents --json` once.
pub fn claude_agents(claude: &Path, env: &EnvMap) -> Result<Vec<AgentInfo>> {
    let (code, out) = output_with_timeout(
        claude_command(claude, env, &["agents", "--json"]),
        Duration::from_secs(10),
    )?;
    if code != 0 {
        bail!("claude agents exited with {code}");
    }
    parse_agents(&out)
}

/// Everything the app knows about the installed Claude Code.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ClaudeInfo {
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub config_dir: PathBuf,
}

/// Detect `claude`, its version and its config directory. Blocking: call on
/// a background thread.
pub fn detect_claude(override_path: Option<&Path>, env: &EnvMap) -> ClaudeInfo {
    let path = resolve_claude(override_path, env);
    let version = path.as_deref().and_then(|p| match claude_version(p, env) {
        Ok(v) => Some(v),
        Err(error) => {
            tracing::warn!(%error, "could not run claude --version");
            None
        }
    });
    // Only ask `claude auth status` when `claude` actually runs.
    let config_dir = claude_config_dir(version.as_ref().and(path.as_deref()), env);
    tracing::info!(?path, ?version, config_dir = %config_dir.display(), "detected Claude Code");
    ClaudeInfo {
        path,
        version,
        config_dir,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_users_status_line() -> anyhow::Result<()> {
        let tmp = tempfile::tempdir()?;
        let config = tmp.path().join("cfg");
        let project = tmp.path().join("proj");
        std::fs::create_dir_all(project.join(".claude"))?;
        std::fs::create_dir_all(&config)?;
        assert_eq!(user_status_line(&config, &project), None);
        std::fs::write(
            config.join("settings.json"),
            r#"{"statusLine":{"type":"command","command":"~/bin/line.sh"}}"#,
        )?;
        assert_eq!(user_status_line(&config, &project).as_deref(), Some("~/bin/line.sh"));
        std::fs::write(
            project.join(".claude").join("settings.json"),
            r#"{"statusLine":{"type":"command","command":"echo project"}}"#,
        )?;
        assert_eq!(user_status_line(&config, &project).as_deref(), Some("echo project"));
        Ok(())
    }

    #[test]
    fn parses_env_after_the_marker_only() {
        let mut out = b"Welcome to my shell!\nPATH=/fake\n".to_vec();
        out.extend_from_slice(ENV_MARKER.as_bytes());
        out.extend_from_slice(b"PATH=/usr/bin:/bin\0HOME=/Users/a\0MULTI=line1\nline2\0\0");
        let env = parse_env_output(&out);
        let env = env.unwrap_or_default();
        assert_eq!(env.get("PATH").map(String::as_str), Some("/usr/bin:/bin"));
        assert_eq!(env.get("HOME").map(String::as_str), Some("/Users/a"));
        assert_eq!(env.get("MULTI").map(String::as_str), Some("line1\nline2"));
        assert_eq!(env.len(), 3);
    }

    #[test]
    fn no_marker_means_no_env() {
        assert_eq!(parse_env_output(b"PATH=/x\0"), None);
    }

    #[test]
    #[cfg(unix)]
    fn captures_a_real_shell() {
        let env = capture_login_env(Path::new("/bin/sh"), Duration::from_secs(5));
        let env = env.unwrap_or_default();
        assert!(env.contains_key("PATH"));
    }

    #[test]
    #[cfg(unix)]
    fn a_hanging_command_times_out() {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg("sleep 5");
        let started = Instant::now();
        assert!(output_with_timeout(cmd, Duration::from_millis(200)).is_err());
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn child_env_drops_claude_markers() {
        let mut env = EnvMap::new();
        env.insert("PATH".into(), "/bin".into());
        env.insert("CLAUDECODE".into(), "1".into());
        env.insert("CLAUDE_CODE_CHILD_SESSION".into(), "1".into());
        let env = child_env(Some(&env));
        assert_eq!(env.len(), 1);
        assert!(env.contains_key("PATH"));
    }

    #[test]
    #[cfg(unix)]
    fn finds_executables_on_path() -> Result<()> {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir()?;
        let bin = dir.path().join("claude");
        std::fs::write(&bin, "#!/bin/sh\necho '9.9.9 (Claude Code)'\n")?;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755))?;
        let path_var = format!("/nonexistent:{}", dir.path().display());
        assert_eq!(find_in_path("claude", Some(&path_var)), Some(bin.clone()));

        let mut env = EnvMap::new();
        env.insert("PATH".into(), path_var);
        assert_eq!(resolve_claude(None, &env), Some(bin.clone()));
        assert_eq!(
            resolve_claude(Some(Path::new("/custom/claude")), &env),
            Some(PathBuf::from("/custom/claude"))
        );
        assert_eq!(claude_version(&bin, &env)?, "9.9.9 (Claude Code)");
        Ok(())
    }

    #[test]
    fn parses_auth_status() {
        let out = br#"{"loggedIn": true, "configDirectory": "/Users/a/.claude", "x": 1}"#;
        assert_eq!(
            parse_auth_status_config_dir(out),
            Some(PathBuf::from("/Users/a/.claude"))
        );
        assert_eq!(
            parse_auth_status_config_dir(br#"{"loggedIn": false}"#),
            None
        );
        assert_eq!(parse_auth_status_config_dir(b"not json"), None);
    }

    #[test]
    fn cleanup_period() -> Result<()> {
        let t = tempfile::tempdir()?;
        let (config, project) = (t.path().join("cfg"), t.path().join("p"));
        std::fs::create_dir_all(project.join(".claude"))?;
        std::fs::create_dir_all(&config)?;
        assert_eq!(cleanup_period_days(&config, &project), 30);
        std::fs::write(config.join("settings.json"), r#"{"cleanupPeriodDays": 90}"#)?;
        assert_eq!(cleanup_period_days(&config, &project), 90);
        std::fs::write(project.join(".claude").join("settings.json"), r#"{"cleanupPeriodDays": 7}"#)?;
        assert_eq!(cleanup_period_days(&config, &project), 7);
        Ok(())
    }

    #[test]
    fn config_dir_fallbacks() {
        let mut env = EnvMap::new();
        env.insert("HOME".into(), "/Users/a".into());
        assert_eq!(fallback_config_dir(&env), PathBuf::from("/Users/a/.claude"));
        env.insert("CLAUDE_CONFIG_DIR".into(), "/cfg".into());
        assert_eq!(fallback_config_dir(&env), PathBuf::from("/cfg"));
    }

    #[test]
    fn parses_agents_json() -> Result<()> {
        // Shape observed from Claude Code 2.1.330.
        let out = br#"[{"pid":101,"cwd":"/home/u/p","kind":"interactive","startedAt":1795459769543,
            "sessionId":"ed029315-3668-5f1f-a011-5a48443b2681","name":"x","status":"busy"}]"#;
        let agents = parse_agents(out)?;
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].session_id, "ed029315-3668-5f1f-a011-5a48443b2681");
        assert_eq!(agents[0].pid, Some(101));
        assert_eq!(agents[0].kind.as_deref(), Some("interactive"));
        assert_eq!(agents[0].status.as_deref(), Some("busy"));
        assert_eq!(agents[0].started_at, Some(1_795_459_769_543));
        Ok(())
    }
}
