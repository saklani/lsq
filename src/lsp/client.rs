//! A minimal, synchronous LSP client over a child process's stdio.
//!
//! This is the Rust port of the Python prototype: spawn the server, a background thread
//! frames messages off stdout into a channel, and `request`/`notify`/`pump` drive the
//! protocol. It is one-shot for now (M1); the daemon (M2) will keep one of these warm
//! and reuse it across client calls.

use crate::jsonrpc;
use anyhow::{Context, Result, anyhow};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::BufReader;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use super::Backend;

/// Outcome of a single request attempt. `ContentModified` (-32801) is the server telling
/// us it's busy (typically still indexing) and the request should be retried.
enum ReqErr {
    ContentModified,
    Other(anyhow::Error),
}

pub struct LspClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    next_id: i64,
    /// Latest `publishDiagnostics` payload per document URI (push-model servers).
    pub published: HashMap<String, Vec<Value>>,
}

impl LspClient {
    /// Spawn the backend and start draining its stdout on a background thread.
    pub fn spawn(backend: &dyn Backend) -> Result<Self> {
        let mut child = Command::new(backend.program())
            .args(backend.args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawning backend `{}`", backend.program()))?;

        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");

        // Reader thread: continuously frame messages so the server's stdout never blocks.
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            while let Ok(Some(msg)) = jsonrpc::read_message(&mut reader) {
                if tx.send(msg).is_err() {
                    break; // client dropped
                }
            }
        });

        Ok(Self {
            child,
            stdin,
            rx,
            next_id: 0,
            published: HashMap::new(),
        })
    }

    /// Fire-and-forget notification.
    pub fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        jsonrpc::write_message(
            &mut self.stdin,
            &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        )?;
        Ok(())
    }

    /// Send a request and block until its response. Retries on `ContentModified` (the
    /// server is busy indexing) until it settles or the retry window elapses — this is what
    /// makes the first semantic query on a big, cold project succeed instead of erroring.
    pub fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let retry_deadline = Instant::now() + Duration::from_secs(120);
        loop {
            match self.request_once(method, params.clone(), timeout) {
                Ok(v) => return Ok(v),
                Err(ReqErr::ContentModified) if Instant::now() < retry_deadline => {
                    std::thread::sleep(Duration::from_millis(150));
                }
                Err(ReqErr::ContentModified) => {
                    return Err(anyhow!(
                        "`{method}`: server still busy (content modified) after retry window"
                    ));
                }
                Err(ReqErr::Other(e)) => return Err(e),
            }
        }
    }

    /// One request attempt. Interleaved notifications are stored; interleaved server→client
    /// requests are answered minimally so the server never blocks on us.
    fn request_once(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, ReqErr> {
        self.next_id += 1;
        let id = self.next_id;
        jsonrpc::write_message(
            &mut self.stdin,
            &json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        )
        .map_err(|e| ReqErr::Other(e.into()))?;

        let deadline = Instant::now() + timeout;
        loop {
            let remaining = match deadline.checked_duration_since(Instant::now()) {
                Some(r) => r,
                None => {
                    return Err(ReqErr::Other(anyhow!(
                        "timeout waiting for `{method}` response"
                    )));
                }
            };
            let msg = match self.rx.recv_timeout(remaining) {
                Ok(msg) => msg,
                Err(RecvTimeoutError::Timeout) => {
                    return Err(ReqErr::Other(anyhow!(
                        "timeout waiting for `{method}` response"
                    )));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(ReqErr::Other(anyhow!(
                        "backend exited before responding to `{method}`"
                    )));
                }
            };

            self.handle_incoming(&msg).map_err(ReqErr::Other)?;

            if msg.get("id").and_then(Value::as_i64) == Some(id) {
                if let Some(err) = msg.get("error") {
                    if err.get("code").and_then(Value::as_i64) == Some(-32801) {
                        return Err(ReqErr::ContentModified);
                    }
                    return Err(ReqErr::Other(anyhow!("LSP error for `{method}`: {err}")));
                }
                return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    /// Pump incoming messages for up to `dur`, storing notifications. Used to collect the
    /// async `publishDiagnostics` that push-model servers (rust-analyzer) emit after
    /// `didOpen` once they've indexed.
    pub fn pump(&mut self, dur: Duration) -> Result<()> {
        let deadline = Instant::now() + dur;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match self.rx.recv_timeout(remaining) {
                Ok(msg) => self.handle_incoming(&msg)?,
                Err(_) => break, // timed out / disconnected
            }
        }
        Ok(())
    }

    fn handle_incoming(&mut self, msg: &Value) -> Result<()> {
        let method = msg.get("method").and_then(Value::as_str);
        match method {
            // push diagnostics — keep the latest per URI
            Some("textDocument/publishDiagnostics") => {
                if let Some(params) = msg.get("params") {
                    let uri = params
                        .get("uri")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let diags = params
                        .get("diagnostics")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    self.published.insert(uri, diags);
                }
            }
            // a server→client REQUEST (has both method and id) — answer minimally
            Some(m) if msg.get("id").is_some() => {
                let id = msg.get("id").cloned().unwrap();
                let result = match m {
                    // rust-analyzer asks for config; one null per requested item = defaults
                    "workspace/configuration" => {
                        let n = msg
                            .pointer("/params/items")
                            .and_then(Value::as_array)
                            .map_or(0, Vec::len);
                        Value::Array(vec![Value::Null; n])
                    }
                    // registerCapability, workDoneProgress/create, etc. → acknowledge
                    _ => Value::Null,
                };
                jsonrpc::write_message(
                    &mut self.stdin,
                    &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
                )?;
            }
            // other notifications ($/progress, logMessage, …) ignored
            _ => {}
        }
        Ok(())
    }
}

impl Drop for LspClient {
    fn drop(&mut self) {
        // Best-effort terminate + reap so we don't leak child language servers.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
