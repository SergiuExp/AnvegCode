//! Signing in to GitHub: the OAuth device flow, a pasted personal access
//! token, where the token is kept (the OS keychain, never a file or the
//! log), and the `GIT_ASKPASS` helper that lets git use it.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use serde::Deserialize;

use crate::github_api::{Request, Secret, Transport, USER_AGENT};

/// The scopes the app asks for: repositories (incl. private), organizations
/// (to list them as owners), and workflows (re-run CI; push changes to
/// workflow files).
pub const SCOPES: &str = "repo read:org workflow";

// ---------------------------------------------------------------------
// Where the token lives
// ---------------------------------------------------------------------

pub trait TokenStore: Send + Sync {
    fn get(&self) -> Result<Option<Secret>>;
    fn set(&self, token: &Secret) -> Result<()>;
    fn delete(&self) -> Result<()>;
}

/// The macOS Keychain (the kernel keyring on Linux, for development).
pub struct KeychainStore {
    service: String,
    account: String,
}

impl KeychainStore {
    /// `service` differs for development builds, so they never touch the
    /// installed app's token.
    pub fn new(service: &str) -> Self {
        Self {
            service: service.to_owned(),
            account: "github".to_owned(),
        }
    }

    fn entry(&self) -> Result<keyring::Entry> {
        keyring::Entry::new(&self.service, &self.account).map_err(|e| anyhow!("keychain: {e}"))
    }
}

impl TokenStore for KeychainStore {
    fn get(&self) -> Result<Option<Secret>> {
        match self.entry()?.get_password() {
            Ok(t) if !t.is_empty() => Ok(Some(Secret::new(t))),
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(anyhow!("Could not read the GitHub token from the keychain: {e}")),
        }
    }

    fn set(&self, token: &Secret) -> Result<()> {
        self.entry()?
            .set_password(token.expose())
            .map_err(|e| anyhow!("Could not save the GitHub token in the keychain: {e}"))
    }

    fn delete(&self) -> Result<()> {
        match self.entry()?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(anyhow!("Could not remove the GitHub token from the keychain: {e}")),
        }
    }
}

/// For tests.
#[derive(Default)]
pub struct MemoryStore(std::sync::Mutex<Option<Secret>>);

impl TokenStore for MemoryStore {
    fn get(&self) -> Result<Option<Secret>> {
        Ok(self.0.lock().map_err(|_| anyhow!("lock"))?.clone())
    }

    fn set(&self, token: &Secret) -> Result<()> {
        *self.0.lock().map_err(|_| anyhow!("lock"))? = Some(token.clone());
        Ok(())
    }

    fn delete(&self) -> Result<()> {
        *self.0.lock().map_err(|_| anyhow!("lock"))? = None;
        Ok(())
    }
}

/// A pasted token, tidied: surrounding spaces and a `Bearer `/`token `
/// prefix are dropped. `None` when it cannot be a token.
pub fn clean_token(text: &str) -> Option<Secret> {
    let t = text.trim();
    let t = t
        .strip_prefix("Bearer ")
        .or_else(|| t.strip_prefix("token "))
        .unwrap_or(t)
        .trim();
    (t.len() >= 20 && !t.contains(char::is_whitespace)).then(|| Secret::new(t))
}

// ---------------------------------------------------------------------
// OAuth device flow
// ---------------------------------------------------------------------

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct DeviceCode {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    /// Seconds.
    pub expires_in: u64,
    /// Seconds between polls.
    pub interval: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Poll {
    Token(Secret),
    /// Not yet approved: poll again after the interval.
    Pending,
    /// Polling too fast: add 5 seconds to the interval.
    SlowDown,
    Denied,
    Expired,
    Failed(String),
}

fn form_post(transport: &dyn Transport, url: &str, form: &[(&str, &str)]) -> Result<serde_json::Value> {
    let body = form
        .iter()
        .map(|(k, v)| format!("{}={}", encode(k), encode(v)))
        .collect::<Vec<_>>()
        .join("&");
    let response = transport.send(&Request {
        method: "POST",
        url: url.to_owned(),
        headers: vec![
            ("Accept".into(), "application/json".into()),
            ("Content-Type".into(), "application/x-www-form-urlencoded".into()),
            ("User-Agent".into(), USER_AGENT.into()),
        ],
        body: Some(body.into_bytes()),
    })?;
    serde_json::from_slice(&response.body).with_context(|| {
        format!("GitHub answered HTTP {} without JSON", response.status)
    })
}

/// Percent-encoding for form values.
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else if b == b' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Step 1: a code for the user to enter at `verification_uri`.
pub fn request_device_code(transport: &dyn Transport, client_id: &str) -> Result<DeviceCode> {
    let v = form_post(
        transport,
        "https://github.com/login/device/code",
        &[("client_id", client_id), ("scope", SCOPES)],
    )?;
    if let Some(e) = v["error"].as_str() {
        let detail = v["error_description"].as_str().unwrap_or(e);
        return Err(anyhow!("GitHub refused to start the sign-in: {detail}"));
    }
    let mut code: DeviceCode = serde_json::from_value(v)?;
    if code.interval == 0 {
        code.interval = 5;
    }
    if code.device_code.is_empty() || code.user_code.is_empty() {
        return Err(anyhow!("GitHub's answer had no device code."));
    }
    Ok(code)
}

/// Step 2, repeated every `interval` seconds until it is not `Pending`.
pub fn poll_device_token(transport: &dyn Transport, client_id: &str, device_code: &str) -> Poll {
    let v = match form_post(
        transport,
        "https://github.com/login/oauth/access_token",
        &[
            ("client_id", client_id),
            ("device_code", device_code),
            ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
        ],
    ) {
        Ok(v) => v,
        Err(e) => return Poll::Failed(format!("{e:#}")),
    };
    parse_poll(&v)
}

pub fn parse_poll(v: &serde_json::Value) -> Poll {
    if let Some(token) = v["access_token"].as_str().filter(|t| !t.is_empty()) {
        return Poll::Token(Secret::new(token));
    }
    match v["error"].as_str() {
        Some("authorization_pending") => Poll::Pending,
        Some("slow_down") => Poll::SlowDown,
        Some("access_denied") => Poll::Denied,
        Some("expired_token") => Poll::Expired,
        Some(other) => Poll::Failed(
            v["error_description"]
                .as_str()
                .unwrap_or(other)
                .to_owned(),
        ),
        None => Poll::Failed("GitHub's answer had no token.".to_owned()),
    }
}

// ---------------------------------------------------------------------
// git
// ---------------------------------------------------------------------

/// The askpass script: git runs it with its prompt as the argument. It
/// answers only github.com prompts: `x-access-token` as the user name, the
/// token (from the environment) as the password. Other hosts get nothing,
/// so the token never goes elsewhere.
pub const ASKPASS_SCRIPT: &str = r#"#!/bin/sh
# Anveg Code: answers git's https prompts for github.com with the token in
# $ANVEG_GIT_TOKEN (set only on git commands the app runs). Nothing is stored.
case "$1" in
  *github.com*)
    case "$1" in
      Username*) echo "x-access-token" ;;
      *) printf '%s\n' "$ANVEG_GIT_TOKEN" ;;
    esac ;;
  *) exit 1 ;;
esac
"#;

/// Write the askpass script to `dir/git-askpass.sh` (0700). Its path.
pub fn write_askpass(dir: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = dir.join("git-askpass.sh");
    let current = std::fs::read_to_string(&path).unwrap_or_default();
    if current != ASKPASS_SCRIPT {
        std::fs::write(&path, ASKPASS_SCRIPT)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::github_api::Response;

    struct Canned {
        body: String,
        sent: Mutex<Vec<Request>>,
    }

    impl Transport for Canned {
        fn send(&self, r: &Request) -> Result<Response> {
            if let Ok(mut s) = self.sent.lock() {
                s.push(r.clone());
            }
            Ok(Response {
                status: 200,
                headers: Vec::new(),
                body: self.body.as_bytes().to_vec(),
            })
        }
    }

    fn canned(body: &str) -> Canned {
        Canned {
            body: body.to_owned(),
            sent: Mutex::new(Vec::new()),
        }
    }

    #[test]
    fn device_flow() -> Result<()> {
        let t = canned(r#"{"device_code":"dc","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#);
        let code = request_device_code(&t, "Iv1.abc")?;
        assert_eq!(code.user_code, "ABCD-1234");
        let sent = t.sent.lock().map_err(|_| anyhow!("lock"))?;
        let body = String::from_utf8_lossy(sent[0].body.as_deref().unwrap_or_default()).into_owned();
        assert_eq!(body, "client_id=Iv1.abc&scope=repo+read%3Aorg+workflow");
        drop(sent);

        let t = canned(r#"{"error":"authorization_pending"}"#);
        assert_eq!(poll_device_token(&t, "Iv1.abc", "dc"), Poll::Pending);
        let t = canned(r#"{"access_token":"gho_xyz","token_type":"bearer","scope":"repo"}"#);
        assert_eq!(poll_device_token(&t, "Iv1.abc", "dc"), Poll::Token(Secret::new("gho_xyz")));
        assert_eq!(parse_poll(&serde_json::json!({"error":"slow_down"})), Poll::SlowDown);
        assert_eq!(parse_poll(&serde_json::json!({"error":"access_denied"})), Poll::Denied);
        assert_eq!(parse_poll(&serde_json::json!({"error":"expired_token"})), Poll::Expired);
        let t = canned(r#"{"error":"unauthorized_client","error_description":"Device flow is not enabled"}"#);
        assert!(request_device_code(&t, "x").is_err_and(|e| e.to_string().contains("not enabled")));
        Ok(())
    }

    #[test]
    fn tokens() -> Result<()> {
        assert_eq!(clean_token("  ghp_0123456789abcdefghijABCD \n"), Some(Secret::new("ghp_0123456789abcdefghijABCD")));
        assert_eq!(clean_token("Bearer github_pat_0123456789abcdef_x"), Some(Secret::new("github_pat_0123456789abcdef_x")));
        assert_eq!(clean_token("short"), None);
        assert_eq!(clean_token("has a space in it but is long"), None);
        let store = MemoryStore::default();
        assert_eq!(store.get()?, None);
        store.set(&Secret::new("t"))?;
        assert_eq!(store.get()?, Some(Secret::new("t")));
        store.delete()?;
        assert_eq!(store.get()?, None);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn askpass_answers_only_github() -> Result<()> {
        let tmp = tempfile::tempdir()?;
        let script = write_askpass(tmp.path())?;
        let run = |prompt: &str| -> Result<(bool, String)> {
            let out = crate::process::run(
                &crate::process::Cmd::new(&script)
                    .arg(prompt)
                    .var("ANVEG_GIT_TOKEN", "sekret"),
            )?;
            Ok((out.success(), out.stdout_str()))
        };
        assert_eq!(run("Username for 'https://github.com': ")?, (true, "x-access-token\n".into()));
        assert_eq!(run("Password for 'https://x-access-token@github.com': ")?, (true, "sekret\n".into()));
        assert!(!run("Password for 'https://gitlab.com': ")?.0);
        Ok(())
    }
}
