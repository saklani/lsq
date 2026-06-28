//! A *warm* backend session: an LSP server initialized once and kept alive, plus the
//! per-verb operations run against it. This is what the daemon holds so `initialize` +
//! project indexing is paid once, not per call.

use super::client::LspClient;
use super::{Language, ResolvedBackend, path_to_uri};
use crate::position::{LspPos, UserPos};
use crate::selector::Selector;
use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use similar::TextDiff;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct Session {
    client: LspClient,
    supports_pull: bool,
    opened: HashSet<String>,
    version: i64,
    root: PathBuf,
}

impl Session {
    /// Spawn the backend and complete the `initialize` handshake. The FIRST operation on a
    /// fresh session still pays indexing warmup; everything after reuses this client.
    pub fn start(backend: ResolvedBackend, root: &Path) -> Result<Session> {
        let root_uri = path_to_uri(root);
        let mut client = LspClient::spawn(&backend)?;

        let init = client.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "publishDiagnostics": {},
                        "synchronization": {},
                        // hierarchical symbols so `selectionRange` lands ON the identifier
                        // (not the line start) — required for rename to target the name.
                        "documentSymbol": { "hierarchicalDocumentSymbolSupport": true }
                    },
                    "workspace": { "configuration": true, "workspaceFolders": true }
                },
                "initializationOptions": backend.initialization_options(),
                "workspaceFolders": [ { "uri": root_uri, "name": "workspace" } ]
            }),
            Duration::from_secs(60),
        )?;
        client.notify("initialized", json!({}))?;

        let supports_pull = init.pointer("/capabilities/diagnosticProvider").is_some();
        Ok(Session {
            client,
            supports_pull,
            opened: HashSet::new(),
            version: 1,
            root: root.to_path_buf(),
        })
    }

    /// Make the server's view of `file` match disk: `didOpen` first time, `didChange` after.
    fn sync_doc(&mut self, file: &Path, lang: Language) -> Result<String> {
        let uri = path_to_uri(file);
        let text = std::fs::read_to_string(file)?;
        self.version += 1;

        if self.opened.contains(&uri) {
            self.client.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": uri, "version": self.version },
                    "contentChanges": [ { "text": text } ]
                }),
            )?;
        } else {
            self.client.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": lang.language_id(),
                        "version": self.version,
                        "text": text
                    }
                }),
            )?;
            self.opened.insert(uri.clone());
        }
        Ok(uri)
    }

    /// Diagnostics for `file`.
    pub fn diagnostics(&mut self, file: &Path, lang: Language) -> Result<Value> {
        let uri = self.sync_doc(file, lang)?;
        let mut diags: Vec<Value> = Vec::new();

        // pull model (tsgo)
        if self.supports_pull {
            if let Ok(report) = self.client.request(
                "textDocument/diagnostic",
                json!({ "textDocument": { "uri": uri } }),
                Duration::from_secs(30),
            ) {
                if let Some(items) = report.get("items").and_then(Value::as_array) {
                    diags.extend(items.iter().cloned());
                }
            }
        }

        // push model (rust-analyzer): COLD waits for the first publish (indexing); WARM
        // settles briefly to absorb the post-didChange publish.
        let had_prior = self.client.published.contains_key(&uri);
        let cold_deadline = Instant::now() + Duration::from_secs(20);
        let warm_settle = Instant::now() + Duration::from_millis(800);
        loop {
            self.client.pump(Duration::from_millis(100))?;
            let now = Instant::now();
            if had_prior {
                if now >= warm_settle {
                    break;
                }
            } else if self.client.published.contains_key(&uri) || now >= cold_deadline {
                break;
            }
        }
        if let Some(pushed) = self.client.published.get(&uri) {
            diags.extend(pushed.iter().cloned());
        }

        // dedupe (rust-analyzer answers both pull and push)
        let mut seen = HashSet::new();
        diags.retain(|d| {
            let key = (
                d.pointer("/range/start/line").and_then(Value::as_u64).unwrap_or(0),
                d.pointer("/range/start/character").and_then(Value::as_u64).unwrap_or(0),
                d.get("code").map(ToString::to_string).unwrap_or_default(),
                d.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
            );
            seen.insert(key)
        });

        Ok(shape(file, &diags))
    }

    /// Resolve a [`Selector`] to an exact position (+ the symbol, for the name form).
    pub fn resolve(&mut self, sel: &Selector, file: &Path, lang: Language) -> Result<Value> {
        let uri = self.sync_doc(file, lang)?;
        let text = std::fs::read_to_string(file)?;
        let (pos, symbol) = self.locate(sel, file, &text, &uri)?;

        let line_text = nth_line(&text, pos.line0 + 1).unwrap_or_default();
        let user = pos.to_user(&line_text);

        Ok(json!({
            "file": file.display().to_string(),
            "symbol": symbol,
            "position": { "line": user.line1, "col": user.col1 },
            "lsp": { "line": pos.line0, "character": pos.char0_utf16 },
        }))
    }

    /// Rename the symbol at `sel` to `new_name`. Returns the WorkspaceEdit rendered as a
    /// unified diff (+ counts); writes the edits to disk when `apply` is set.
    pub fn rename(
        &mut self,
        sel: &Selector,
        file: &Path,
        lang: Language,
        new_name: &str,
        apply: bool,
    ) -> Result<Value> {
        let uri = self.sync_doc(file, lang)?;
        // rename needs SEMANTIC cross-reference analysis, not just syntax — so wait until
        // the server has analyzed this file (a cold server is still indexing).
        self.await_analysis(&uri)?;
        let text = std::fs::read_to_string(file)?;
        let (pos, _symbol) = self.locate(sel, file, &text, &uri)?;

        let edit = self.client.request(
            "textDocument/rename",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": pos.line0, "character": pos.char0_utf16 },
                "newName": new_name
            }),
            Duration::from_secs(30),
        )?;
        if edit.is_null() {
            return Err(anyhow!("the symbol at this position cannot be renamed"));
        }

        let (file_edits, resource_ops) = collect_changes(&edit);

        let mut diff = String::new();
        let mut modified: Vec<(String, String)> = Vec::new(); // (path, new_text)
        let mut total_edits = 0usize;
        for (uri, edits) in &file_edits {
            if edits.is_empty() {
                continue;
            }
            total_edits += edits.len();
            let path = uri_to_path(uri);
            let old = std::fs::read_to_string(&path).unwrap_or_default();
            let new = apply_edits(&old, edits);
            let rel = self.rel(&path);
            diff.push_str(&unified_diff(&rel, &old, &new));
            modified.push((path, new));
        }

        if apply {
            for (path, new) in &modified {
                std::fs::write(path, new)?;
            }
            // keep the server's view consistent with disk
            for (uri, _) in &file_edits {
                if self.opened.contains(uri) {
                    if let Ok(t) = std::fs::read_to_string(uri_to_path(uri)) {
                        self.version += 1;
                        let _ = self.client.notify(
                            "textDocument/didChange",
                            json!({
                                "textDocument": { "uri": uri, "version": self.version },
                                "contentChanges": [ { "text": t } ]
                            }),
                        );
                    }
                }
            }
        }

        Ok(json!({
            "new_name": new_name,
            "files_changed": modified.len(),
            "edits": total_edits,
            "applied": apply,
            "resource_operations": resource_ops,
            "diff": diff,
        }))
    }

    /// Selector → LSP position. Shared by `resolve` and `rename`.
    fn locate(
        &mut self,
        sel: &Selector,
        file: &Path,
        text: &str,
        uri: &str,
    ) -> Result<(LspPos, Value)> {
        match sel {
            Selector::Position { line, col, .. } => {
                let line_text = nth_line(text, *line)?;
                let p = UserPos::new(*line, *col)
                    .to_lsp(&line_text)
                    .ok_or_else(|| anyhow!("column {col} is out of range on line {line}"))?;
                Ok((p, Value::Null))
            }
            Selector::Line { line, .. } => {
                let line_text = nth_line(text, *line)?;
                let col = first_nonspace_col(&line_text);
                let p = UserPos::new(*line, col).to_lsp(&line_text).unwrap();
                Ok((p, Value::Null))
            }
            Selector::Name { name, .. } => {
                let syms = self.document_symbols(uri)?;
                let found = find_symbol(&syms, name)
                    .ok_or_else(|| anyhow!("no symbol named `{name}` in {}", file.display()))?;
                let start = found
                    .pointer("/selectionRange/start")
                    .or_else(|| found.pointer("/location/range/start"))
                    .or_else(|| found.pointer("/range/start"))
                    .ok_or_else(|| anyhow!("symbol `{name}` has no position"))?;
                let p = LspPos {
                    line0: start.get("line").and_then(Value::as_u64).unwrap_or(0) as u32,
                    char0_utf16: start.get("character").and_then(Value::as_u64).unwrap_or(0) as u32,
                };
                let sym = json!({
                    "name": found.get("name").cloned().unwrap_or(Value::Null),
                    "kind": symbol_kind(found.get("kind")),
                });
                Ok((p, sym))
            }
        }
    }

    /// Block until the server has analyzed `uri` (its first `publishDiagnostics` arrives),
    /// which means the project is indexed enough for semantic queries. No-op once warm.
    fn await_analysis(&mut self, uri: &str) -> Result<()> {
        if self.client.published.contains_key(uri) {
            return Ok(());
        }
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            self.client.pump(Duration::from_millis(100))?;
            if self.client.published.contains_key(uri) {
                break;
            }
        }
        Ok(())
    }

    fn document_symbols(&mut self, uri: &str) -> Result<Vec<Value>> {
        let res = self.client.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
            Duration::from_secs(20),
        )?;
        Ok(res.as_array().cloned().unwrap_or_default())
    }

    fn rel(&self, path: &str) -> String {
        Path::new(path)
            .strip_prefix(&self.root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|_| path.to_string())
    }
}

/// Shape raw LSP diagnostics into a compact, severity-ranked report.
fn shape(file: &Path, diags: &[Value]) -> Value {
    let mut items: Vec<(u64, u64, Value)> = diags
        .iter()
        .map(|d| {
            let sev = d.get("severity").and_then(Value::as_u64).unwrap_or(1);
            let line = d.pointer("/range/start/line").and_then(Value::as_u64).unwrap_or(0) + 1;
            let col = d.pointer("/range/start/character").and_then(Value::as_u64).unwrap_or(0) + 1;
            let row = json!({
                "severity": severity_name(sev),
                "line": line,
                "col": col,
                "code": d.get("code").cloned().unwrap_or(Value::Null),
                "source": d.get("source").cloned().unwrap_or(Value::Null),
                "message": d.get("message").cloned().unwrap_or(Value::Null),
            });
            (sev, line, row)
        })
        .collect();
    items.sort_by_key(|(sev, line, _)| (*sev, *line));

    let rows: Vec<Value> = items.into_iter().map(|(_, _, r)| r).collect();
    json!({
        "file": file.display().to_string(),
        "count": rows.len(),
        "diagnostics": rows,
    })
}

/// Split a WorkspaceEdit into (per-file text edits, resource operations).
fn collect_changes(edit: &Value) -> (Vec<(String, Vec<Value>)>, Vec<Value>) {
    let mut files = Vec::new();
    let mut resource_ops = Vec::new();

    if let Some(changes) = edit.get("changes").and_then(Value::as_object) {
        for (uri, edits) in changes {
            files.push((uri.clone(), edits.as_array().cloned().unwrap_or_default()));
        }
    }
    if let Some(dcs) = edit.get("documentChanges").and_then(Value::as_array) {
        for dc in dcs {
            if let Some(td) = dc.get("textDocument") {
                let uri = td.get("uri").and_then(Value::as_str).unwrap_or_default().to_string();
                let edits = dc.get("edits").and_then(Value::as_array).cloned().unwrap_or_default();
                files.push((uri, edits));
            } else if dc.get("kind").is_some() {
                // CreateFile / RenameFile / DeleteFile — reported, not auto-applied (yet).
                resource_ops.push(dc.clone());
            }
        }
    }
    (files, resource_ops)
}

/// Apply LSP TextEdits to `text`, returning the new text. Edits are applied right-to-left
/// so earlier byte offsets stay valid.
fn apply_edits(text: &str, edits: &[Value]) -> String {
    let mut sorted: Vec<&Value> = edits.iter().collect();
    sorted.sort_by_key(|e| {
        std::cmp::Reverse((
            e.pointer("/range/start/line").and_then(Value::as_u64).unwrap_or(0),
            e.pointer("/range/start/character").and_then(Value::as_u64).unwrap_or(0),
        ))
    });

    let mut s = text.to_string();
    for e in sorted {
        let start = byte_offset(
            text,
            e.pointer("/range/start/line").and_then(Value::as_u64).unwrap_or(0) as u32,
            e.pointer("/range/start/character").and_then(Value::as_u64).unwrap_or(0) as u32,
        );
        let end = byte_offset(
            text,
            e.pointer("/range/end/line").and_then(Value::as_u64).unwrap_or(0) as u32,
            e.pointer("/range/end/character").and_then(Value::as_u64).unwrap_or(0) as u32,
        );
        let new_text = e.get("newText").and_then(Value::as_str).unwrap_or("");
        if start <= end && end <= s.len() {
            s.replace_range(start..end, new_text);
        }
    }
    s
}

/// Byte offset of an LSP (0-based line, UTF-16 char) position within `text`.
fn byte_offset(text: &str, line0: u32, char0_utf16: u32) -> usize {
    let mut offset = 0usize;
    let mut cur_line = 0u32;
    for line in text.split_inclusive('\n') {
        if cur_line == line0 {
            let mut utf16 = 0u32;
            for ch in line.chars() {
                if utf16 >= char0_utf16 {
                    break;
                }
                utf16 += ch.len_utf16() as u32;
                offset += ch.len_utf8();
            }
            return offset;
        }
        offset += line.len();
        cur_line += 1;
    }
    offset
}

fn unified_diff(rel: &str, old: &str, new: &str) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{rel}"), &format!("b/{rel}"))
        .to_string()
}

fn uri_to_path(uri: &str) -> String {
    uri.strip_prefix("file://").unwrap_or(uri).to_string()
}

/// The text of a 1-based line, without its terminator.
fn nth_line(text: &str, line1: u32) -> Result<String> {
    text.lines()
        .nth((line1 as usize).saturating_sub(1))
        .map(str::to_string)
        .ok_or_else(|| anyhow!("line {line1} is out of range"))
}

/// 1-based column of the first non-whitespace character (or 1 for a blank line).
fn first_nonspace_col(line: &str) -> u32 {
    let mut col = 1;
    for ch in line.chars() {
        if !ch.is_whitespace() {
            break;
        }
        col += 1;
    }
    col
}

/// Depth-first search of a DocumentSymbol / SymbolInformation list for an exact name.
fn find_symbol(syms: &[Value], name: &str) -> Option<Value> {
    for s in syms {
        if s.get("name").and_then(Value::as_str) == Some(name) {
            return Some(s.clone());
        }
        if let Some(children) = s.get("children").and_then(Value::as_array) {
            if let Some(found) = find_symbol(children, name) {
                return Some(found);
            }
        }
    }
    None
}

fn severity_name(sev: u64) -> &'static str {
    match sev {
        1 => "error",
        2 => "warning",
        3 => "information",
        4 => "hint",
        _ => "unknown",
    }
}

/// LSP `SymbolKind` number → readable name (subset; falls back to "unknown").
fn symbol_kind(k: Option<&Value>) -> Value {
    let name = match k.and_then(Value::as_u64).unwrap_or(0) {
        2 => "module",
        5 => "class",
        6 => "method",
        8 => "field",
        10 => "enum",
        11 => "interface",
        12 => "function",
        13 => "variable",
        14 => "constant",
        22 => "enum_member",
        23 => "struct",
        26 => "type_parameter",
        _ => "unknown",
    };
    json!(name)
}
