//! A small Language Server Protocol client (next phases, 4.3).
//!
//! JSON-RPC 2.0 over a server's stdin and stdout, with `lsp-types`. One
//! reader thread per server parses frames; responses complete the futures
//! returned by [`LanguageServer::request`], server-to-client requests get
//! simple default answers, and notifications (diagnostics and so on) go to
//! the callback given at start. GUI-free: the futures can be awaited on any
//! executor.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::str::FromStr as _;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use futures::channel::oneshot;
use lsp_types::{
    ClientCapabilities, InitializeParams, InitializeResult, Position, Range,
    ServerCapabilities, TextDocumentContentChangeEvent, TextDocumentSyncCapability,
    TextDocumentSyncKind, Uri, WorkspaceFolder,
};
use serde_json::{Value, json};

pub use lsp_types;

/// A notification the server sent.
#[derive(Clone, Debug)]
pub struct ServerNotification {
    pub method: String,
    pub params: Value,
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value>>>>>;

/// How to start a server.
#[derive(Clone, Debug)]
pub struct ServerCommand {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The whole environment; `None` inherits the app's.
    pub env: Option<Vec<(String, String)>>,
}

/// A running language server.
pub struct LanguageServer {
    child: Mutex<Child>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Pending,
    next_id: AtomicI64,
    capabilities: Mutex<ServerCapabilities>,
    name: String,
}

impl std::fmt::Debug for LanguageServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LanguageServer")
            .field("name", &self.name)
            .finish()
    }
}

impl LanguageServer {
    /// Spawn the server. `on_notification` runs on the reader thread;
    /// `on_exit` runs there once, when the server's stdout closes (it
    /// exited or crashed).
    pub fn spawn(
        command: &ServerCommand,
        on_notification: impl Fn(ServerNotification) + Send + 'static,
        on_exit: impl FnOnce() + Send + 'static,
    ) -> Result<Arc<Self>> {
        let mut cmd = Command::new(&command.program);
        cmd.args(&command.args)
            .current_dir(&command.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(env) = &command.env {
            cmd.env_clear();
            cmd.envs(env.iter().map(|(k, v)| (k, v)));
        }
        let mut child = cmd
            .spawn()
            .with_context(|| format!("could not start {}", command.program.display()))?;
        let stdin = child.stdin.take().context("no stdin")?;
        let stdout = child.stdout.take().context("no stdout")?;
        let stderr = child.stderr.take();
        let name = command
            .program
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let server = Arc::new(Self {
            child: Mutex::new(child),
            stdin: Arc::new(Mutex::new(stdin)),
            pending: Arc::default(),
            next_id: AtomicI64::new(1),
            capabilities: Mutex::new(ServerCapabilities::default()),
            name: name.clone(),
        });

        if let Some(stderr) = stderr {
            let name = name.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    tracing::debug!(server = %name, "{line}");
                }
            });
        }

        let pending = server.pending.clone();
        let writer = server.stdin.clone();
        std::thread::Builder::new()
            .name(format!("lsp-{name}"))
            .spawn(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let message = match read_message(&mut reader) {
                        Ok(Some(m)) => m,
                        Ok(None) => break,
                        Err(error) => {
                            tracing::warn!(server = %name, %error, "bad message from language server");
                            break;
                        }
                    };
                    dispatch(message, &pending, &writer, &on_notification);
                }
                // Fail everything still waiting.
                if let Ok(mut p) = pending.lock() {
                    for (_, tx) in p.drain() {
                        let _ = tx.send(Err(anyhow!("the language server stopped")));
                    }
                }
                on_exit();
            })?;
        Ok(server)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Send a request and wait for its result.
    pub fn request<R: lsp_types::request::Request>(
        &self,
        params: R::Params,
    ) -> impl std::future::Future<Output = Result<R::Result>> + use<R> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        let sent = (|| -> Result<()> {
            if let Ok(mut p) = self.pending.lock() {
                p.insert(id, tx);
            }
            let message = json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": R::METHOD,
                "params": serde_json::to_value(params)?,
            });
            write_message(&self.stdin, &message)
        })();
        if sent.is_err()
            && let Ok(mut p) = self.pending.lock()
        {
            p.remove(&id);
        }
        async move {
            sent?;
            let value = rx.await.map_err(|_| anyhow!("the language server stopped"))??;
            Ok(serde_json::from_value(value)?)
        }
    }

    /// Send a notification.
    pub fn notify<N: lsp_types::notification::Notification>(&self, params: N::Params) -> Result<()> {
        let message = json!({
            "jsonrpc": "2.0",
            "method": N::METHOD,
            "params": serde_json::to_value(params)?,
        });
        write_message(&self.stdin, &message)
    }

    /// `initialize` then `initialized`. `roots` become the workspace folders.
    pub async fn initialize(&self, roots: &[PathBuf]) -> Result<InitializeResult> {
        let folders: Vec<WorkspaceFolder> = roots
            .iter()
            .filter_map(|r| {
                Some(WorkspaceFolder {
                    uri: path_to_uri(r)?,
                    name: r
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                })
            })
            .collect();
        #[allow(deprecated)]
        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: roots.first().and_then(|r| path_to_uri(r)),
            workspace_folders: Some(folders),
            capabilities: client_capabilities(),
            client_info: Some(lsp_types::ClientInfo {
                name: "Anveg Code".into(),
                version: None,
            }),
            ..InitializeParams::default()
        };
        let result = self
            .request::<lsp_types::request::Initialize>(params)
            .await?;
        if let Ok(mut c) = self.capabilities.lock() {
            *c = result.capabilities.clone();
        }
        self.notify::<lsp_types::notification::Initialized>(lsp_types::InitializedParams {})?;
        Ok(result)
    }

    pub fn capabilities(&self) -> ServerCapabilities {
        self.capabilities
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default()
    }

    /// Whether `didChange` may send ranges (otherwise the whole text).
    pub fn incremental_sync(&self) -> bool {
        match self.capabilities().text_document_sync {
            Some(TextDocumentSyncCapability::Kind(k)) => k == TextDocumentSyncKind::INCREMENTAL,
            Some(TextDocumentSyncCapability::Options(o)) => {
                o.change == Some(TextDocumentSyncKind::INCREMENTAL)
            }
            None => false,
        }
    }

    /// `shutdown` then `exit`, then make sure the process is gone.
    pub async fn shutdown(&self) {
        let _ = futures::future::select(
            Box::pin(self.request::<lsp_types::request::Shutdown>(())),
            sleep(Duration::from_secs(2)),
        )
        .await;
        let _ = self.notify::<lsp_types::notification::Exit>(());
        self.kill_after(Duration::from_millis(500));
    }

    /// Kill the process if it has not exited after `grace`.
    pub fn kill_after(&self, grace: Duration) {
        let deadline = std::time::Instant::now() + grace;
        if let Ok(mut child) = self.child.lock() {
            while std::time::Instant::now() < deadline {
                if matches!(child.try_wait(), Ok(Some(_))) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    pub fn is_running(&self) -> bool {
        self.child
            .lock()
            .map(|mut c| matches!(c.try_wait(), Ok(None)))
            .unwrap_or(false)
    }
}

impl Drop for LanguageServer {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock()
            && matches!(child.try_wait(), Ok(None))
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// A future that finishes after `d` (a helper thread sleeps), so the crate
/// does not depend on an executor's timers.
pub fn sleep(d: Duration) -> oneshot::Receiver<()> {
    let (tx, rx) = oneshot::channel();
    std::thread::spawn(move || {
        std::thread::sleep(d);
        let _ = tx.send(());
    });
    rx
}

fn client_capabilities() -> ClientCapabilities {
    serde_json::from_value(json!({
        "workspace": {
            "workspaceFolders": true,
            "configuration": true,
            "didChangeWatchedFiles": { "dynamicRegistration": true }
        },
        "textDocument": {
            "synchronization": { "didSave": true, "willSave": false, "dynamicRegistration": false },
            "publishDiagnostics": { "relatedInformation": true },
            "completion": {
                "completionItem": {
                    "snippetSupport": false,
                    "documentationFormat": ["markdown", "plaintext"],
                    "insertReplaceSupport": false
                },
                "contextSupport": true
            },
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "definition": { "linkSupport": true },
            "formatting": { "dynamicRegistration": false }
        },
        "window": { "workDoneProgress": false }
    }))
    .unwrap_or_default()
}

fn write_message(stdin: &Mutex<ChildStdin>, message: &Value) -> Result<()> {
    let body = serde_json::to_vec(message)?;
    let mut stdin = stdin.lock().map_err(|_| anyhow!("stdin lock poisoned"))?;
    write!(stdin, "Content-Length: {}\r\n\r\n", body.len())?;
    stdin.write_all(&body)?;
    stdin.flush()?;
    Ok(())
}

/// Read one framed message; `None` at end of stream.
pub fn read_message(reader: &mut impl BufRead) -> Result<Option<Value>> {
    let mut length = None;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            if length.is_some() {
                break;
            }
            continue;
        }
        if let Some(v) = line
            .to_ascii_lowercase()
            .strip_prefix("content-length:")
            .map(|v| v.trim().to_owned())
        {
            length = Some(v.parse::<usize>().context("bad Content-Length")?);
        }
    }
    let length = length.context("no Content-Length")?;
    if length > 256 * 1024 * 1024 {
        bail!("message too large");
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body)?;
    Ok(Some(serde_json::from_slice(&body)?))
}

fn dispatch(
    message: Value,
    pending: &Pending,
    writer: &Mutex<ChildStdin>,
    on_notification: &(impl Fn(ServerNotification) + Send),
) {
    let method = message.get("method").and_then(Value::as_str);
    let id = message.get("id").cloned();
    match (method, id) {
        // A response.
        (None, Some(id)) => {
            let Some(id) = id.as_i64() else {
                return;
            };
            let tx = pending.lock().ok().and_then(|mut p| p.remove(&id));
            if let Some(tx) = tx {
                let result = match message.get("error") {
                    Some(error) => Err(anyhow!(
                        "{}",
                        error
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("language server error")
                    )),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(result);
            }
        }
        // A request from the server: answer with defaults.
        (Some(method), Some(id)) => {
            let result = server_request_default(method, message.get("params"));
            let reply = json!({ "jsonrpc": "2.0", "id": id, "result": result });
            let _ = write_message(writer, &reply);
        }
        (Some(method), None) => on_notification(ServerNotification {
            method: method.to_owned(),
            params: message.get("params").cloned().unwrap_or(Value::Null),
        }),
        (None, None) => {}
    }
}

/// What we answer to requests servers send us.
fn server_request_default(method: &str, params: Option<&Value>) -> Value {
    match method {
        // One `null` per requested item: "use your defaults".
        "workspace/configuration" => {
            let n = params
                .and_then(|p| p.get("items"))
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            Value::Array(vec![Value::Null; n])
        }
        "workspace/workspaceFolders" => Value::Null,
        "window/showMessageRequest" => Value::Null,
        // registerCapability, workDoneProgress/create, …: accept.
        _ => Value::Null,
    }
}

// ---------------------------------------------------------------------
// URIs and positions
// ---------------------------------------------------------------------

/// `file://` URI for an absolute path, percent-encoding what needs it.
pub fn path_to_uri(path: &Path) -> Option<Uri> {
    let mut s = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        let keep = b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~');
        if keep {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    Uri::from_str(&s).ok()
}

/// The path of a `file://` URI.
pub fn uri_to_path(uri: &Uri) -> Option<PathBuf> {
    let s = uri.as_str();
    let rest = s.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let mut bytes = Vec::with_capacity(rest.len());
    let raw = rest.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        if raw[i] == b'%' && i + 2 < raw.len() {
            let hex = std::str::from_utf8(&raw[i + 1..i + 3]).ok()?;
            bytes.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            bytes.push(raw[i]);
            i += 1;
        }
    }
    Some(PathBuf::from(String::from_utf8(bytes).ok()?))
}

/// The LSP position (line, UTF-16 column) of byte `offset` in `text`.
pub fn offset_to_position(text: &str, offset: usize) -> Position {
    let offset = offset.min(text.len());
    let before = &text[..floor_char_boundary(text, offset)];
    let line = before.matches('\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |i| i + 1);
    let character = before[line_start..].encode_utf16().count() as u32;
    Position { line, character }
}

/// The byte offset of an LSP position in `text` (clamped).
pub fn position_to_offset(text: &str, pos: Position) -> usize {
    let mut offset = 0;
    for (i, line) in text.split_inclusive('\n').enumerate() {
        if i as u32 == pos.line {
            let mut units = 0u32;
            for (b, ch) in line.char_indices() {
                if units >= pos.character || ch == '\n' {
                    return offset + b;
                }
                units += ch.len_utf16() as u32;
            }
            return offset + line.trim_end_matches('\n').len();
        }
        offset += line.len();
    }
    text.len()
}

fn floor_char_boundary(text: &str, mut i: usize) -> usize {
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The single-range change that turns `old` into `new` (common prefix and
/// suffix removed), for incremental `didChange`.
pub fn incremental_change(old: &str, new: &str) -> TextDocumentContentChangeEvent {
    let prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    let prefix = floor_char_boundary(old, prefix.min(old.len()));
    let prefix = floor_char_boundary(new, prefix);
    let max_suffix = (old.len() - prefix).min(new.len() - prefix);
    let mut suffix = old
        .bytes()
        .rev()
        .zip(new.bytes().rev())
        .take(max_suffix)
        .take_while(|(a, b)| a == b)
        .count();
    while suffix > 0
        && (!old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix))
    {
        suffix -= 1;
    }
    let start = offset_to_position(old, prefix);
    let end = offset_to_position(old, old.len() - suffix);
    TextDocumentContentChangeEvent {
        range: Some(Range { start, end }),
        range_length: None,
        text: new[prefix..new.len() - suffix].to_owned(),
    }
}

/// The LSP language id for a highlighting language name.
pub fn language_id(language: &str, path: &Path) -> String {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match (language, ext) {
        ("typescript", "tsx") | ("tsx", _) => "typescriptreact".into(),
        ("javascript", "jsx") => "javascriptreact".into(),
        (lang, _) => lang.to_owned(),
    }
}

/// Method names the app reacts to.
pub mod methods {
    pub const PUBLISH_DIAGNOSTICS: &str =
        <lsp_types::notification::PublishDiagnostics as lsp_types::notification::Notification>::METHOD;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing() -> Result<()> {
        let body = r#"{"jsonrpc":"2.0","method":"x","params":{}}"#;
        let raw = format!("Content-Length: {}\r\nContent-Type: x\r\n\r\n{body}", body.len());
        let mut reader = BufReader::new(raw.as_bytes());
        let v = read_message(&mut reader)?.ok_or_else(|| anyhow!("none"))?;
        assert_eq!(v["method"], "x");
        assert!(read_message(&mut reader)?.is_none());
        Ok(())
    }

    #[test]
    fn uris() {
        let p = Path::new("/Users/a b/π.rs");
        let uri = path_to_uri(p);
        assert_eq!(
            uri.as_ref().map(|u| u.as_str().to_owned()).as_deref(),
            Some("file:///Users/a%20b/%CF%80.rs")
        );
        assert_eq!(uri.and_then(|u| uri_to_path(&u)), Some(p.to_path_buf()));
    }

    #[test]
    fn positions() {
        let text = "ab\nπx\n";
        assert_eq!(offset_to_position(text, 0), Position::new(0, 0));
        assert_eq!(offset_to_position(text, 3), Position::new(1, 0));
        // π is 2 bytes, 1 UTF-16 unit.
        assert_eq!(offset_to_position(text, 5), Position::new(1, 1));
        assert_eq!(position_to_offset(text, Position::new(1, 1)), 5);
        assert_eq!(position_to_offset(text, Position::new(1, 99)), 6);
        assert_eq!(position_to_offset(text, Position::new(9, 0)), text.len());
    }

    #[test]
    fn incremental_changes() {
        let c = incremental_change("hello world", "hello brave world");
        assert_eq!(c.text, "brave ");
        let r = c.range.unwrap_or_default();
        assert_eq!((r.start, r.end), (Position::new(0, 6), Position::new(0, 6)));

        let c = incremental_change("a\nb\nc", "a\nc");
        let r = c.range.unwrap_or_default();
        assert_eq!(c.text, "");
        assert_eq!((r.start, r.end), (Position::new(1, 0), Position::new(2, 0)));

        let c = incremental_change("ππ", "πxπ");
        assert_eq!(c.text, "x");
        let c = incremental_change("same", "same");
        assert_eq!(c.text, "");
    }

    #[test]
    fn language_ids() {
        assert_eq!(language_id("typescript", Path::new("a.tsx")), "typescriptreact");
        assert_eq!(language_id("rust", Path::new("a.rs")), "rust");
    }
}
