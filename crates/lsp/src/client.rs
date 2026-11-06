use crossbeam_channel::Sender;
use lsp_server::{Connection, Message, Notification, Request, RequestId, Response};
use lsp_types::{
    Diagnostic, DidChangeTextDocumentParams,
    DidOpenTextDocumentParams, InitializeParams, InitializeResult,
    PublishDiagnosticsParams,
    DocumentSymbolParams,
    TextDocumentIdentifier, TextDocumentItem,
    TextDocumentPositionParams, Uri, VersionedTextDocumentIdentifier,
    WorkspaceFolder,
};
use std::collections::HashMap;
use std::io::{BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

/// Client-side result type.
pub type LspResult<T> = Result<T, std::io::Error>;

/// A minimal LSP client speaking JSON-RPC over the
/// server's stdio. Diagnostics are collected and
/// drained by the GUI thread; requests (hover,
/// goto-definition,...) are correlated by id.
pub struct LspClient {
    sender: Sender<Message>,
    diagnostics: Arc<Mutex<Vec<PublishDiagnosticsParams>>>,
    pending: Arc<Mutex<HashMap<RequestId, Sender<Response>>>>,
    next_id: i32,
    _io: thread::JoinHandle<()>,
    _pump: thread::JoinHandle<()>,
    /// 那个服务器进程本身。`Command::spawn` 返回的 `Child` 一旦被丢掉，Windows
    /// 只关掉父进程手里的句柄，服务器就没人管了 —— 客户端的存活期就是它的存活期，
    /// 所以进程跟着 [`LspClient`] 一起走（见 `Drop`）。
    child: Child,
}

impl LspClient {
    /// Spawns `cmd` with piped stdio and starts
    /// the JSON-RPC connection threads.
    pub fn start(cmd: &str, args: &[String]) -> LspResult<Self> {
        let mut command = Command::new(cmd);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // The editor has no console; a console server would otherwise get a
        // DOS window of its own on startup.
        dg_core::process::no_console_window(&mut command);
        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        // lsp-server 0.7 exposes Message::read/write
        // and Connection's channels publicly, so the
        // client transports are wired by hand.
        let (sender, receiver) = crossbeam_channel::bounded::<Message>(0);
        let (writer_tx, writer_rx) =
            crossbeam_channel::bounded::<Message>(0);

        let writer = thread::spawn(move || {
            let mut w = BufWriter::new(stdin);
            while let Ok(msg) = writer_rx.recv() {
                if msg.write(&mut w).is_err() {
                    break;
                }
                if w.flush().is_err() {
                    break;
                }
            }
        });

        let _reader = thread::spawn(move || {
            let mut r = BufReader::new(stdout);
            while let Ok(Some(msg)) = Message::read(&mut r) {
                if sender.send(msg).is_err() {
                    break;
                }
            }
        });

        let connection = Connection {
            sender: writer_tx,
            receiver,
        };

        let diagnostics = Arc::new(Mutex::new(Vec::new()));
        let pending: Arc<Mutex<HashMap<RequestId, Sender<Response>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let diag = diagnostics.clone();
        let pend = pending.clone();
        let pump = thread::spawn(move || {
            for msg in &connection.receiver {
                match msg {
                    Message::Notification(n) => {
                        handle_notification(&n, &diag);
                    }
                    Message::Response(r) => {
                        let mut map = pend.lock().unwrap();
                        if let Some(tx) = map.remove(&r.id) {
                            let _ = tx.send(r);
                        }
                    }
                    Message::Request(_r) => {
                        // server->client requests (e.g.
                        // workspace/applyEdit) are
                        // ignored in this skeleton.
                    }
                }
            }
        });

        Ok(Self {
            sender: connection.sender.clone(),
            diagnostics,
            pending,
            next_id: 1,
            _io: writer,
            _pump: pump,
            child,
        })
    }

    /// Performs the initialize handshake.
    pub fn initialize(&mut self, root: &Path) -> LspResult<InitializeResult> {
        // Canonicalize so the path→URI conversion
        // never fails on a relative path.
        let root = root
            .canonicalize()
            .unwrap_or_else(|_| root.to_path_buf());
        let uri = uri_from_path(&root)
            .ok_or_else(|| io_err("invalid root path"))?;
        let params = InitializeParams {
            workspace_folders: Some(vec![WorkspaceFolder {
                uri,
                name: root.to_string_lossy().into_owned(),
            }]),
            ..Default::default()
        };
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = crossbeam_channel::bounded(1);
        {
            let mut map = self.pending.lock().unwrap();
            map.insert(RequestId::from(id), tx);
        }
        let req = Request {
            id: RequestId::from(id),
            method: "initialize".into(),
            params: serde_json::to_value(params)
                .map_err(|e| io_err(e.to_string()))?,
        };
        self.sender
            .send(Message::Request(req))
            .map_err(|e| io_err(e.to_string()))?;
        let resp = rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|e| io_err(e.to_string()))?;
        match resp.response_result {
            Ok(v) => serde_json::from_value(v)
                .map_err(|e| io_err(e.to_string())),
            Err(e) => Err(io_err(e.message)),
        }
    }

    /// Completes the initialize handshake:
    /// the `initialized` notification the
    /// server waits for before it processes
    /// didOpen / diagnostics.
    pub fn initialized(&self) {
        let _ = self.send_notification(
            "initialized",
            serde_json::Value::Null,
        );
    }

    pub fn did_open(&self, uri: &str, language: &str, text: &str) {
        let params = DidOpenTextDocumentParams {
            text_document: TextDocumentItem {
                uri: uri.parse().unwrap(),
                language_id: language.into(),
                version: 1,
                text: text.into(),
            },
        };
        let _ = self.send_notification(
            "textDocument/didOpen",
            params,
        );
    }

    pub fn did_change(
        &self,
        uri: &str,
        version: i32,
        content_changes: Vec<lsp_types::TextDocumentContentChangeEvent>,
    ) {
        let params = DidChangeTextDocumentParams {
            text_document: VersionedTextDocumentIdentifier {
                uri: uri.parse().unwrap(),
                version,
            },
            content_changes,
        };
        let _ = self.send_notification(
            "textDocument/didChange",
            params,
        );
    }

    /// Sends a request and waits up to `timeout` for
    /// the response.
    pub fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
        timeout: Duration,
    ) -> Option<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = crossbeam_channel::bounded(1);
        {
            let mut map = self.pending.lock().unwrap();
            map.insert(RequestId::from(id), tx);
        }
        let req = Request {
            id: RequestId::from(id),
            method: method.into(),
            params,
        };
        if self
            .sender
            .send(Message::Request(req))
            .is_err()
        {
            return None;
        }
        let resp = rx.recv_timeout(timeout).ok()?;
        resp.response_result.ok()
    }

    /// Drains collected diagnostics.
    /// Completion items at the current
    /// position (textDocument/completion).
    pub fn completion(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Option<serde_json::Value> {
        let params = serde_json::to_value(
            lsp_types::CompletionParams {
                text_document_position:
                    lsp_types::TextDocumentPositionParams {
                        text_document:
                            lsp_types::TextDocumentIdentifier {
                                uri: uri.parse().unwrap(),
                            },
                        position: lsp_types::Position {
                            line,
                            character,
                        },
                    },
                work_done_progress_params: Default::default(),
                partial_result_params: Default::default(),
                context: None,
            },
        )
        .ok()?;
        self.request(
            "textDocument/completion",
            params,
            Duration::from_secs(5),
        )
    }

    /// Definition locations for the symbol
    /// at the current position
    /// (textDocument/definition).
    pub fn goto_definition(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Option<serde_json::Value> {
        self.request(
            "textDocument/definition",
            hover_params(uri, line, character),
            Duration::from_secs(5),
        )
    }

    /// Hover info at the current position
    /// (textDocument/hover).
    pub fn hover(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Option<serde_json::Value> {
        self.request(
            "textDocument/hover",
            hover_params(uri, line, character),
            Duration::from_secs(5),
        )
    }

    /// Signature help at the current
    /// position (textDocument/signatureHelp)
    /// — the calltip source.
    pub fn signature_help(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Option<serde_json::Value> {
        let params = serde_json::to_value(
            lsp_types::SignatureHelpParams {
                text_document_position_params:
                    lsp_types::TextDocumentPositionParams {
                        text_document:
                            lsp_types::TextDocumentIdentifier {
                                uri: uri.parse().unwrap(),
                            },
                        position: lsp_types::Position {
                            line,
                            character,
                        },
                    },
                work_done_progress_params:
                    Default::default(),
                context: None,
            },
        )
        .ok()?;
        self.request(
            "textDocument/signatureHelp",
            params,
            Duration::from_secs(5),
        )
    }

    /// Outline of the current document
    /// (textDocument/documentSymbol).
    pub fn document_symbols(
        &mut self,
        uri: &str,
    ) -> Option<serde_json::Value> {
        let params = serde_json::to_value(
            DocumentSymbolParams {
                text_document: TextDocumentIdentifier {
                    uri: uri.parse().unwrap(),
                },
                work_done_progress_params:
                    Default::default(),
                partial_result_params:
                    Default::default(),
            },
        )
        .unwrap();
        self.request(
            "textDocument/documentSymbol",
            params,
            Duration::from_secs(5),
        )
    }

    /// All references to the symbol
    /// at the caret (textDocument/
    /// references). Returns an array
    /// of Location objects.
    pub fn references(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Option<serde_json::Value> {
        // ReferenceParams is
        // TextDocumentPositionParams +
        // a context; the server accepts
        // the position part alone for
        // the common case.
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": true },
        });
        self.request(
            "textDocument/references",
            params,
            Duration::from_secs(5),
        )
    }

    /// Rename the symbol at the
    /// caret to `new_name` across
    /// the workspace (textDocument/
    /// rename). Returns a WorkspaceEdit.
    pub fn rename(
        &mut self,
        uri: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Option<serde_json::Value> {
        let params = serde_json::json!({
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "newName": new_name,
        });
        self.request(
            "textDocument/rename",
            params,
            Duration::from_secs(5),
        )
    }

    pub fn take_diagnostics(&self) -> Vec<PublishDiagnosticsParams> {
        let mut d = self.diagnostics.lock().unwrap();
        std::mem::take(&mut *d)
    }

    /// 按协议道别，但只等 `grace` 那么久：一个正在建索引的服务器回话可以
    /// 很慢，而 UI 线程不能等它。等不到也照发 `exit`，剩下的由 `Drop` 收尾。
    pub fn shutdown(&mut self, grace: Duration) {
        let _ = self.request(
            "shutdown",
            serde_json::Value::Null,
            grace,
        );
        let _ = self.send_notification("exit", serde_json::Value::Null);
    }

    fn send_notification(
        &self,
        method: &str,
        params: impl serde::Serialize,
    ) -> Result<(), serde_json::Error> {
        let n = Notification {
            method: method.into(),
            params: serde_json::to_value(params)?,
        };
        let _ = self.sender.send(Message::Notification(n));
        Ok(())
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // 走到这儿还没自己退出的进程就是漏网的：换语言、关文档、退出、
        // 半路失败的握手，每一条都收在这里。`kill` 之后必须 `wait`，
        // 否则进程表里留一个僵尸。
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn handle_notification(
    n: &Notification,
    diagnostics: &Arc<Mutex<Vec<PublishDiagnosticsParams>>>,
) {
    if n.method == "textDocument/publishDiagnostics" {
        if let Ok(params) =
            serde_json::from_value::<PublishDiagnosticsParams>(
                n.params.clone(),
            )
        {
            diagnostics.lock().unwrap().push(params);
        }
    }
}

/// Builds a hover request payload for the current
/// cursor position.
pub fn hover_params(
    uri: &str,
    line: u32,
    character: u32,
) -> serde_json::Value {
    serde_json::to_value(lsp_types::HoverParams {
        text_document_position_params: TextDocumentPositionParams {
            text_document: TextDocumentIdentifier {
                uri: uri.parse().unwrap(),
            },
            position: lsp_types::Position { line, character },
        },
        work_done_progress_params: Default::default(),
    })
    .unwrap()
}

/// Severity ordering for diagnostics.
pub fn severity_of(d: &Diagnostic) -> u8 {
    d.severity
        .and_then(|s| serde_json::to_value(s).ok())
        .and_then(|v| v.as_i64())
        .map(|v| v.clamp(1, 4) as u8)
        .unwrap_or(0)
}

fn io_err(msg: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, msg.into())
}

/// Percent-encodes a filesystem path into a
/// `file://` URI. lsp-types 0.97's `Uri`
/// (fluent-uri based) has no path conversion.
fn uri_from_path(path: &Path) -> Option<Uri> {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut out = String::from(if text.starts_with('/') {
        "file://"
    } else {
        "file:///"
    });
    for &b in text.as_bytes() {
        match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9'
            | b'-' | b'_' | b'.' | b'~' | b'/' | b':'
            | b'@' | b'!' | b'$' | b'&' | b'\'' | b'('
            | b')' | b'*' | b'+' | b',' | b';' | b'=' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out.parse().ok()
}
