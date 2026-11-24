//! Language servers for open editors (next phases, 4.3).
//!
//! - One server per (project, server command). A server starts when the
//!   first file in its language opens, and gets `shutdown`/`exit` after 10
//!   minutes with no open files.
//! - After a crash it restarts with backoff, at most 3 times.
//! - Documents are kept in sync with `didOpen`, incremental `didChange`,
//!   `didSave` and `didClose`; Claude's edits to files that are not open
//!   reach the server as `workspace/didChangeWatchedFiles`.
//! - The editor talks to it through gpui-component's provider traits
//!   ([`LspProvider`]).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use anveg_core::claude_env;
use anveg_core::fswatch::FsWatchService;
use anveg_lsp::lsp_types::{self, notification as n, request as r};
use anveg_lsp::{LanguageServer, ServerCommand, ServerNotification};
use anyhow::{Result, anyhow};
use futures::StreamExt as _;
use gpui_kit::component::Rope;
use gpui_kit::component::input::{
    CompletionProvider, DefinitionProvider, HoverProvider, RopeExt as _,
};
use gpui_kit::{App, AppContext as _, Context, EventEmitter, Task, WeakEntity, Window};
use uuid::Uuid;

/// Opens a definition's file at (line, column), both 1-based.
pub type OpenLocation = Rc<dyn Fn(PathBuf, u32, u32, &mut Window, &mut App)>;

/// Stop a server this long after its last file closed.
pub const IDLE_STOP: Duration = Duration::from_secs(10 * 60);
/// Restarts after crashes, at most.
pub const MAX_RESTARTS: u32 = 3;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ServerKey {
    pub project: Uuid,
    pub command: Vec<String>,
}

/// A server's state, for the editor's status line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerStatus {
    Starting,
    Ready,
    /// Not installed (with what to install), or crashed too often.
    Unavailable(String),
}

struct OpenDoc {
    language_id: String,
    version: i32,
    text: String,
    /// Editors showing it (a file can be open in two panes).
    refs: usize,
}

struct Server {
    status: ServerStatus,
    server: Option<Arc<LanguageServer>>,
    roots: Vec<PathBuf>,
    docs: HashMap<PathBuf, OpenDoc>,
    idle_task: Option<Task<()>>,
    restarts: u32,
    stopping: bool,
    _watch: Vec<anveg_core::fswatch::Subscription>,
    _tasks: Vec<Task<()>>,
}

/// What changed, for editors to pick up.
#[derive(Clone, Debug)]
pub enum LspEvent {
    Diagnostics(PathBuf),
    Status(ServerKey),
}

enum FromServer {
    Notification(ServerNotification),
    Exited(ServerKey),
    FilesChanged(ServerKey, Vec<PathBuf>),
}

pub struct LspManager {
    servers: HashMap<ServerKey, Server>,
    diagnostics: HashMap<PathBuf, Vec<lsp_types::Diagnostic>>,
    tx: futures::channel::mpsc::UnboundedSender<FromServer>,
    _rx_task: Task<()>,
}

impl EventEmitter<LspEvent> for LspManager {}

/// What an editor needs to open a file with a server.
#[derive(Clone, Debug)]
pub struct OpenRequest {
    pub project: Uuid,
    pub roots: Vec<PathBuf>,
    pub command: Vec<String>,
    pub env: Option<claude_env::EnvMap>,
    pub language_id: String,
    pub path: PathBuf,
    pub text: String,
}

impl LspManager {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (tx, mut rx) = futures::channel::mpsc::unbounded::<FromServer>();
        let rx_task = cx.spawn(async move |this, cx| {
            while let Some(message) = rx.next().await {
                if this
                    .update(cx, |this, cx| this.on_message(message, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        Self {
            servers: HashMap::new(),
            diagnostics: HashMap::new(),
            tx,
            _rx_task: rx_task,
        }
    }

    pub fn status(&self, key: &ServerKey) -> Option<ServerStatus> {
        self.servers.get(key).map(|s| s.status.clone())
    }

    pub fn diagnostics(&self, path: &Path) -> &[lsp_types::Diagnostic] {
        self.diagnostics.get(path).map(Vec::as_slice).unwrap_or(&[])
    }

    fn server(&self, key: &ServerKey) -> Option<Arc<LanguageServer>> {
        self.servers
            .get(key)
            .filter(|s| s.status == ServerStatus::Ready)
            .and_then(|s| s.server.clone())
    }

    /// An editor opened `req.path`. Starts the server when needed.
    pub fn open(&mut self, req: OpenRequest, watch: Option<Arc<FsWatchService>>, cx: &mut Context<Self>) -> ServerKey {
        let key = ServerKey {
            project: req.project,
            command: req.command.clone(),
        };
        if !self.servers.contains_key(&key) {
            self.servers.insert(
                key.clone(),
                Server {
                    status: ServerStatus::Starting,
                    server: None,
                    roots: req.roots.clone(),
                    docs: HashMap::new(),
                    idle_task: None,
                    restarts: 0,
                    stopping: false,
                    _watch: Vec::new(),
                    _tasks: Vec::new(),
                },
            );
            self.start(&key, req.env.clone(), watch, cx);
        }
        let ready = self.server(&key);
        let Some(entry) = self.servers.get_mut(&key) else {
            return key;
        };
        entry.idle_task = None;
        let doc = entry.docs.entry(req.path.clone()).or_insert(OpenDoc {
            language_id: req.language_id.clone(),
            version: 0,
            text: req.text.clone(),
            refs: 0,
        });
        doc.refs += 1;
        if doc.refs == 1
            && let Some(server) = ready
        {
            doc.version = 1;
            doc.text = req.text;
            send_open(&server, &req.path, doc);
        }
        key
    }

    fn start(
        &mut self,
        key: &ServerKey,
        env: Option<claude_env::EnvMap>,
        watch: Option<Arc<FsWatchService>>,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.servers.get_mut(key) else {
            return;
        };
        let Some((program, args)) = key.command.split_first() else {
            entry.status = ServerStatus::Unavailable("No server command".into());
            return;
        };
        let path_var = env.as_ref().and_then(|e| e.get("PATH")).map(String::as_str);
        let resolved = if program.contains('/') {
            Some(PathBuf::from(program)).filter(|p| p.is_file())
        } else {
            claude_env::find_in_path(program, path_var)
        };
        let Some(resolved) = resolved else {
            entry.status = ServerStatus::Unavailable(format!("{program} is not installed"));
            cx.emit(LspEvent::Status(key.clone()));
            return;
        };
        let cwd = entry.roots.first().cloned().unwrap_or_else(std::env::temp_dir);
        let command = ServerCommand {
            program: resolved,
            args: args.to_vec(),
            cwd,
            env: env.map(|e| e.into_iter().collect()),
        };
        let tx = self.tx.clone();
        let tx_exit = self.tx.clone();
        let k2 = key.clone();
        let spawned = LanguageServer::spawn(
            &command,
            move |n| {
                let _ = tx.unbounded_send(FromServer::Notification(n));
            },
            move || {
                let _ = tx_exit.unbounded_send(FromServer::Exited(k2));
            },
        );
        let server = match spawned {
            Ok(s) => s,
            Err(error) => {
                entry.status = ServerStatus::Unavailable(error.to_string());
                cx.emit(LspEvent::Status(key.clone()));
                return;
            }
        };
        tracing::info!(server = %server.name(), "language server started");
        entry.server = Some(server.clone());
        entry.status = ServerStatus::Starting;
        entry.stopping = false;
        // Claude edits files that are not open: tell the server.
        if let Some(watch) = watch {
            entry._watch = entry
                .roots
                .iter()
                .map(|root| {
                    let tx = self.tx.clone();
                    let key = key.clone();
                    watch.subscribe(root, true, move |paths| {
                        let paths: Vec<PathBuf> = paths
                            .iter()
                            .filter(|p| !is_noise(p))
                            .cloned()
                            .collect();
                        if !paths.is_empty() {
                            let _ = tx.unbounded_send(FromServer::FilesChanged(key.clone(), paths));
                        }
                    })
                })
                .collect();
        }
        let roots = entry.roots.clone();
        let task_key = key.clone();
        let init = cx.background_spawn(async move { server.initialize(&roots).await });
        let task = cx.spawn(async move |this, cx| {
            let result = init.await;
            let _ = this.update(cx, |this, cx| this.initialized(&task_key, result, cx));
        });
        if let Some(entry) = self.servers.get_mut(key) {
            entry._tasks = vec![task];
        }
    }

    fn initialized(
        &mut self,
        key: &ServerKey,
        result: Result<lsp_types::InitializeResult>,
        cx: &mut Context<Self>,
    ) {
        let Some(entry) = self.servers.get_mut(key) else {
            return;
        };
        match result {
            Ok(_) => {
                entry.status = ServerStatus::Ready;
                if let Some(server) = entry.server.clone() {
                    for (path, doc) in entry.docs.iter_mut() {
                        doc.version = 1;
                        send_open(&server, path, doc);
                    }
                }
            }
            Err(error) => {
                tracing::warn!(%error, "language server failed to initialize");
                entry.status = ServerStatus::Unavailable(format!("failed to start: {error}"));
                if let Some(server) = entry.server.take() {
                    entry.stopping = true;
                    cx.background_spawn(async move { server.kill_after(Duration::ZERO) })
                        .detach();
                }
            }
        }
        cx.emit(LspEvent::Status(key.clone()));
    }

    /// The editor's text changed.
    pub fn change(&mut self, key: &ServerKey, path: &Path, text: &str) {
        let server = self.server(key);
        let Some(doc) = self
            .servers
            .get_mut(key)
            .and_then(|s| s.docs.get_mut(path))
        else {
            return;
        };
        if doc.text == text {
            return;
        }
        if let Some(server) = server
            && doc.version > 0
        {
            doc.version += 1;
            let change = if server.incremental_sync() {
                anveg_lsp::incremental_change(&doc.text, text)
            } else {
                lsp_types::TextDocumentContentChangeEvent {
                    range: None,
                    range_length: None,
                    text: text.to_owned(),
                }
            };
            if let Some(uri) = anveg_lsp::path_to_uri(path) {
                let _ = server.notify::<n::DidChangeTextDocument>(lsp_types::DidChangeTextDocumentParams {
                    text_document: lsp_types::VersionedTextDocumentIdentifier {
                        uri,
                        version: doc.version,
                    },
                    content_changes: vec![change],
                });
            }
        }
        doc.text = text.to_owned();
    }

    pub fn saved(&mut self, key: &ServerKey, path: &Path) {
        if let (Some(server), Some(uri)) = (self.server(key), anveg_lsp::path_to_uri(path)) {
            let _ = server.notify::<n::DidSaveTextDocument>(lsp_types::DidSaveTextDocumentParams {
                text_document: lsp_types::TextDocumentIdentifier { uri },
                text: None,
            });
        }
    }

    /// An editor closed `path`.
    pub fn close(&mut self, key: &ServerKey, path: &Path, cx: &mut Context<Self>) {
        let server = self.server(key);
        let Some(entry) = self.servers.get_mut(key) else {
            return;
        };
        let gone = match entry.docs.get_mut(path) {
            Some(doc) => {
                doc.refs = doc.refs.saturating_sub(1);
                doc.refs == 0
            }
            None => false,
        };
        if gone {
            entry.docs.remove(path);
            self.diagnostics.remove(path);
            if let (Some(server), Some(uri)) = (server, anveg_lsp::path_to_uri(path)) {
                let _ = server.notify::<n::DidCloseTextDocument>(lsp_types::DidCloseTextDocumentParams {
                    text_document: lsp_types::TextDocumentIdentifier { uri },
                });
            }
        }
        if entry.docs.is_empty() && entry.idle_task.is_none() {
            let key = key.clone();
            entry.idle_task = Some(cx.spawn(async move |this, cx| {
                cx.background_executor().timer(IDLE_STOP).await;
                let _ = this.update(cx, |this, cx| this.stop(&key, cx));
            }));
        }
    }

    /// `shutdown` + `exit` (idle, or the app is quitting).
    pub fn stop(&mut self, key: &ServerKey, cx: &mut Context<Self>) {
        let Some(mut entry) = self.servers.remove(key) else {
            return;
        };
        entry.stopping = true;
        if let Some(server) = entry.server.take() {
            tracing::info!(server = %server.name(), "stopping idle language server");
            cx.background_spawn(async move { server.shutdown().await })
                .detach();
        }
    }

    pub fn stop_all(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<ServerKey> = self.servers.keys().cloned().collect();
        for key in keys {
            self.stop(&key, cx);
        }
    }

    fn on_message(&mut self, message: FromServer, cx: &mut Context<Self>) {
        match message {
            FromServer::Notification(note) => {
                if note.method == anveg_lsp::methods::PUBLISH_DIAGNOSTICS
                    && let Ok(p) =
                        serde_json::from_value::<lsp_types::PublishDiagnosticsParams>(note.params)
                    && let Some(path) = anveg_lsp::uri_to_path(&p.uri)
                {
                    self.diagnostics.insert(path.clone(), p.diagnostics);
                    cx.emit(LspEvent::Diagnostics(path));
                }
            }
            FromServer::FilesChanged(key, paths) => {
                if let Some(server) = self.server(&key) {
                    let changes = paths
                        .iter()
                        .filter_map(|p| {
                            Some(lsp_types::FileEvent {
                                uri: anveg_lsp::path_to_uri(p)?,
                                typ: if p.exists() {
                                    lsp_types::FileChangeType::CHANGED
                                } else {
                                    lsp_types::FileChangeType::DELETED
                                },
                            })
                        })
                        .collect();
                    let _ = server.notify::<n::DidChangeWatchedFiles>(
                        lsp_types::DidChangeWatchedFilesParams { changes },
                    );
                }
            }
            FromServer::Exited(key) => self.exited(key, cx),
        }
    }

    fn exited(&mut self, key: ServerKey, cx: &mut Context<Self>) {
        let Some(entry) = self.servers.get_mut(&key) else {
            return;
        };
        if entry.stopping || matches!(entry.status, ServerStatus::Unavailable(_)) {
            return;
        }
        entry.server = None;
        entry.restarts += 1;
        if entry.restarts > MAX_RESTARTS {
            entry.status = ServerStatus::Unavailable("crashed too often".into());
            cx.emit(LspEvent::Status(key));
            return;
        }
        tracing::warn!(restarts = entry.restarts, "language server exited; restarting");
        entry.status = ServerStatus::Starting;
        for doc in entry.docs.values_mut() {
            doc.version = 0;
        }
        let delay = Duration::from_secs(1 << (entry.restarts - 1));
        cx.emit(LspEvent::Status(key.clone()));
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let _ = this.update(cx, |this, cx| {
                let (env, watch) = crate::model::model_env_and_watch(cx);
                this.start(&key, env, watch, cx);
            });
        })
        .detach();
    }

    // -----------------------------------------------------------------
    // Requests
    // -----------------------------------------------------------------

    fn position_params(
        &self,
        path: &Path,
        text: &Rope,
        offset: usize,
    ) -> Option<lsp_types::TextDocumentPositionParams> {
        Some(lsp_types::TextDocumentPositionParams {
            text_document: lsp_types::TextDocumentIdentifier {
                uri: anveg_lsp::path_to_uri(path)?,
            },
            position: text.offset_to_position(offset),
        })
    }

    /// Format the whole document: the edits to apply.
    pub fn format(
        &self,
        key: &ServerKey,
        path: &Path,
        tab_size: u32,
        spaces: bool,
        cx: &App,
    ) -> Task<Result<Vec<lsp_types::TextEdit>>> {
        let Some(server) = self.server(key) else {
            return Task::ready(Err(anyhow!("The language server isn't running.")));
        };
        if server.capabilities().document_formatting_provider.is_none() {
            return Task::ready(Err(anyhow!("This language server can't format documents.")));
        }
        let Some(uri) = anveg_lsp::path_to_uri(path) else {
            return Task::ready(Ok(Vec::new()));
        };
        let request = server.request::<r::Formatting>(lsp_types::DocumentFormattingParams {
            text_document: lsp_types::TextDocumentIdentifier { uri },
            options: lsp_types::FormattingOptions {
                tab_size,
                insert_spaces: spaces,
                ..Default::default()
            },
            work_done_progress_params: Default::default(),
        });
        cx.background_spawn(async move { Ok(request.await?.unwrap_or_default()) })
    }
}

fn send_open(server: &LanguageServer, path: &Path, doc: &OpenDoc) {
    let Some(uri) = anveg_lsp::path_to_uri(path) else {
        return;
    };
    let _ = server.notify::<n::DidOpenTextDocument>(lsp_types::DidOpenTextDocumentParams {
        text_document: lsp_types::TextDocumentItem {
            uri,
            language_id: doc.language_id.clone(),
            version: doc.version,
            text: doc.text.clone(),
        },
    });
}

/// Build output and VCS internals are not worth telling a server about.
fn is_noise(path: &Path) -> bool {
    path.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some(".git" | "target" | "node_modules" | ".venv" | "__pycache__")
        )
    })
}

/// The editor's side: completion, hover and go to definition through the
/// manager. Created per editor.
pub struct LspProvider {
    pub manager: WeakEntity<LspManager>,
    pub key: ServerKey,
    pub path: PathBuf,
    /// Asked to open a definition in another file.
    pub open_location: OpenLocation,
}

impl LspProvider {
    /// Make sure the server has the editor's latest text, then return it.
    fn synced(&self, text: &Rope, cx: &mut App) -> Option<Arc<LanguageServer>> {
        let manager = self.manager.upgrade()?;
        let current = text.to_string();
        manager.update(cx, |m, _| {
            m.change(&self.key, &self.path, &current);
            m.server(&self.key)
        })
    }

    fn params(&self, text: &Rope, offset: usize, cx: &App) -> Option<lsp_types::TextDocumentPositionParams> {
        self.manager
            .upgrade()?
            .read(cx)
            .position_params(&self.path, text, offset)
    }
}

impl CompletionProvider for LspProvider {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        trigger: lsp_types::CompletionContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<lsp_types::CompletionResponse>> {
        let empty = || lsp_types::CompletionResponse::Array(Vec::new());
        let (Some(server), Some(position)) = (self.synced(text, cx), self.params(text, offset, cx))
        else {
            return Task::ready(Ok(empty()));
        };
        if server.capabilities().completion_provider.is_none() {
            return Task::ready(Ok(empty()));
        }
        let request = server.request::<r::Completion>(lsp_types::CompletionParams {
            text_document_position: position,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
            context: Some(trigger),
        });
        cx.background_spawn(async move { Ok(request.await?.unwrap_or_else(empty)) })
    }

    fn is_completion_trigger(&self, _offset: usize, new_text: &str, cx: &mut App) -> bool {
        let Some(manager) = self.manager.upgrade() else {
            return false;
        };
        let Some(server) = manager.read(cx).server(&self.key) else {
            return false;
        };
        let triggers: HashSet<String> = server
            .capabilities()
            .completion_provider
            .and_then(|c| c.trigger_characters)
            .unwrap_or_default()
            .into_iter()
            .collect();
        new_text.chars().last().is_some_and(|c| {
            c.is_alphanumeric() || c == '_' || triggers.contains(&c.to_string())
        })
    }
}

impl HoverProvider for LspProvider {
    fn hover(
        &self,
        text: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Option<lsp_types::Hover>>> {
        let (Some(server), Some(position)) = (self.synced(text, cx), self.params(text, offset, cx))
        else {
            return Task::ready(Ok(None));
        };
        if server.capabilities().hover_provider.is_none() {
            return Task::ready(Ok(None));
        }
        let request = server.request::<r::HoverRequest>(lsp_types::HoverParams {
            text_document_position_params: position,
            work_done_progress_params: Default::default(),
        });
        cx.background_spawn(request)
    }
}

impl DefinitionProvider for LspProvider {
    fn definitions(
        &self,
        text: &Rope,
        offset: usize,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<lsp_types::LocationLink>>> {
        let (Some(server), Some(position)) = (self.synced(text, cx), self.params(text, offset, cx))
        else {
            return Task::ready(Ok(Vec::new()));
        };
        if server.capabilities().definition_provider.is_none() {
            return Task::ready(Ok(Vec::new()));
        }
        let request = server.request::<r::GotoDefinition>(lsp_types::GotoDefinitionParams {
            text_document_position_params: position,
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
        cx.background_spawn(async move {
            Ok(match request.await? {
                None => Vec::new(),
                Some(lsp_types::GotoDefinitionResponse::Scalar(l)) => vec![link(l)],
                Some(lsp_types::GotoDefinitionResponse::Array(ls)) => {
                    ls.into_iter().map(link).collect()
                }
                Some(lsp_types::GotoDefinitionResponse::Link(ls)) => ls,
            })
        })
    }
}

fn link(l: lsp_types::Location) -> lsp_types::LocationLink {
    lsp_types::LocationLink {
        origin_selection_range: None,
        target_uri: l.uri,
        target_range: l.range,
        target_selection_range: l.range,
    }
}
