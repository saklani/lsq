//! A *warm* backend session: an LSP server initialized once and kept alive, plus the
//! per-verb operations run against it. Responses are deserialized into typed `lsp_types`
//! structs rather than hand-parsed JSON.

use super::client::LspClient;
use super::{Backend, path_to_uri};
use crate::position::{LspPos, UserPos};
use crate::selector::Selector;
use anyhow::{Context, Result, anyhow};
use lsp_types::{
    CallHierarchyIncomingCall, Diagnostic, DiagnosticSeverity, DocumentChangeOperation,
    DocumentChanges, DocumentSymbol, DocumentSymbolResponse, GotoDefinitionResponse, Hover,
    HoverContents, Location, MarkedString, NumberOrString, OneOf, Position, SymbolKind, TextEdit,
    Uri, WorkspaceEdit, WorkspaceSymbolResponse,
};
use serde_json::{Value, json};
use similar::TextDiff;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use url::Url;

pub struct Session {
    client: LspClient,
    backend: Box<dyn Backend>,
    opened: HashSet<String>,
    version: i64,
    root: PathBuf,
}

impl Session {
    /// Spawn the backend and complete `initialize`. The FIRST operation still pays indexing
    /// warmup; everything after reuses this client.
    pub fn start(backend: Box<dyn Backend>, root: &Path) -> Result<Session> {
        let root_uri = path_to_uri(root);
        let mut client = LspClient::spawn(backend.as_ref())?;

        client.request(
            "initialize",
            json!({
                "processId": std::process::id(),
                "rootUri": root_uri,
                "capabilities": {
                    "textDocument": {
                        "publishDiagnostics": {},
                        "synchronization": {},
                        // hierarchical symbols so `selectionRange` lands ON the identifier
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

        Ok(Session {
            client,
            backend,
            opened: HashSet::new(),
            version: 1,
            root: root.to_path_buf(),
        })
    }

    /// Make the server's view of `file` match disk: `didOpen` first time, `didChange` after.
    fn sync_doc(&mut self, file: &Path) -> Result<String> {
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
            let language_id = self.backend.language_id();
            self.client.notify(
                "textDocument/didOpen",
                json!({
                    "textDocument": {
                        "uri": uri,
                        "languageId": language_id,
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
    pub fn diagnostics(&mut self, file: &Path) -> Result<Value> {
        let uri = self.sync_doc(file)?;
        let raw = self.backend.collect_diagnostics(&mut self.client, &uri)?;

        // typed + deduped
        let mut diags: Vec<Diagnostic> =
            raw.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect();
        let mut seen = HashSet::new();
        diags.retain(|d| {
            seen.insert((
                d.range.start.line,
                d.range.start.character,
                format!("{:?}", d.code),
                d.message.clone(),
            ))
        });

        Ok(shape_diagnostics(file, &diags))
    }

    /// Locations from a position query: references / definition / implementation /
    /// typeDefinition. ONE implementation, parameterized by the LSP method.
    pub fn locations(
        &mut self,
        sel: &Selector,
        file: &Path,
        method: &str,
        include_decl: bool,
    ) -> Result<Value> {
        let uri = self.sync_doc(file)?;
        self.backend.await_ready(&mut self.client, &uri)?;
        let text = std::fs::read_to_string(file)?;
        let (pos, _) = self.locate(sel, file, &text, &uri)?;

        let mut params = json!({
            "textDocument": { "uri": uri },
            "position": { "line": pos.line0, "character": pos.char0_utf16 }
        });
        if method == "textDocument/references" {
            params["context"] = json!({ "includeDeclaration": include_decl });
        }
        let res = self.client.request(method, params, Duration::from_secs(30))?;
        Ok(shape_locations(&self.root, &parse_locations(res)))
    }

    /// Type / hover info at a selector.
    pub fn hover(&mut self, sel: &Selector, file: &Path) -> Result<Value> {
        let uri = self.sync_doc(file)?;
        self.backend.await_ready(&mut self.client, &uri)?;
        let text = std::fs::read_to_string(file)?;
        let (pos, _) = self.locate(sel, file, &text, &uri)?;
        let res = self.client.request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": pos.line0, "character": pos.char0_utf16 }
            }),
            Duration::from_secs(20),
        )?;
        let info = serde_json::from_value::<Hover>(res)
            .ok()
            .map(|h| extract_hover(&h))
            .unwrap_or_default();
        let line_text = nth_line(&text, pos.line0 + 1).unwrap_or_default();
        let user = pos.to_user(&line_text);
        Ok(json!({
            "file": file.display().to_string(),
            "position": { "line": user.line1, "col": user.col1 },
            "info": info,
        }))
    }

    /// Every symbol defined in a file.
    pub fn symbols(&mut self, file: &Path) -> Result<Value> {
        let uri = self.sync_doc(file)?;
        Ok(shape_symbols(&self.document_symbols(&uri)?))
    }

    /// Workspace-wide symbol search (`workspace/symbol`). Not file-scoped — finds where a
    /// symbol named `query` is *defined* anywhere in the project. The ContentModified retry
    /// in `request` waits out cold indexing.
    pub fn workspace_symbols(&mut self, query: &str) -> Result<Value> {
        // `workspace/symbol` needs the project loaded but has no file to gate on, so open a
        // representative file first (tsgo loads the project lazily on didOpen). Indexing
        // servers (rust-analyzer) also return an EMPTY list while cold rather than an error,
        // so for those we poll until results appear.
        self.ensure_project_loaded()?;
        let poll = self.backend.needs_index_poll();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let res = self.client.request(
                "workspace/symbol",
                json!({ "query": query }),
                Duration::from_secs(60),
            )?;
            let shaped = shape_workspace_symbols(&self.root, res);
            let count = shaped.get("count").and_then(Value::as_u64).unwrap_or(0);
            if count > 0 || !poll || Instant::now() >= deadline {
                return Ok(shaped);
            }
            std::thread::sleep(Duration::from_millis(300));
        }
    }

    /// Open one representative source file (if none is open yet) so the server loads the
    /// project — required before workspace-wide queries. No-op once any file is open.
    fn ensure_project_loaded(&mut self) -> Result<()> {
        if !self.opened.is_empty() {
            return Ok(());
        }
        let lang_id = self.backend.language_id();
        if let Some(file) = first_source_file(&self.root, lang_id) {
            let uri = self.sync_doc(&file)?;
            self.backend.await_ready(&mut self.client, &uri)?;
        }
        Ok(())
    }

    /// Callers — incoming calls of the function at `sel` (`callHierarchy/incomingCalls`).
    pub fn callers(&mut self, sel: &Selector, file: &Path) -> Result<Value> {
        let uri = self.sync_doc(file)?;
        self.backend.await_ready(&mut self.client, &uri)?;
        let text = std::fs::read_to_string(file)?;
        let (pos, _) = self.locate(sel, file, &text, &uri)?;

        let prepared = self.client.request(
            "textDocument/prepareCallHierarchy",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": pos.line0, "character": pos.char0_utf16 }
            }),
            Duration::from_secs(30),
        )?;
        // Pass the server's own CallHierarchyItem straight back (don't reconstruct it).
        let Some(item) = prepared.as_array().and_then(|a| a.first()).cloned() else {
            return Ok(json!({ "count": 0, "callers": [] }));
        };
        let res = self.client.request(
            "callHierarchy/incomingCalls",
            json!({ "item": item }),
            Duration::from_secs(30),
        )?;
        Ok(shape_callers(&self.root, res))
    }

    /// Resolve a [`Selector`] to an exact position (+ the symbol, for the name form).
    pub fn resolve(&mut self, sel: &Selector, file: &Path) -> Result<Value> {
        let uri = self.sync_doc(file)?;
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

    /// Rename the symbol at `sel` to `new_name`. Returns the WorkspaceEdit as a unified diff
    /// (+ counts); writes to disk when `apply` is set.
    pub fn rename(
        &mut self,
        sel: &Selector,
        file: &Path,
        new_name: &str,
        apply: bool,
    ) -> Result<Value> {
        let uri = self.sync_doc(file)?;
        self.backend.await_ready(&mut self.client, &uri)?;
        let text = std::fs::read_to_string(file)?;
        let (pos, _) = self.locate(sel, file, &text, &uri)?;

        let res = self.client.request(
            "textDocument/rename",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": pos.line0, "character": pos.char0_utf16 },
                "newName": new_name
            }),
            Duration::from_secs(30),
        )?;
        if res.is_null() {
            return Err(anyhow!("the symbol at this position cannot be renamed"));
        }
        let edit: WorkspaceEdit =
            serde_json::from_value(res).context("decoding rename WorkspaceEdit")?;
        let (file_edits, resource_ops) = collect_changes(&edit);

        let mut diff = String::new();
        let mut modified: Vec<(String, String)> = Vec::new();
        let mut total_edits = 0usize;
        for (uri, edits) in &file_edits {
            if edits.is_empty() {
                continue;
            }
            total_edits += edits.len();
            let path = uri_to_path(uri);
            let old = std::fs::read_to_string(&path).unwrap_or_default();
            let new = apply_edits(&old, edits);
            diff.push_str(&unified_diff(&self.rel(&path), &old, &new));
            modified.push((path, new));
        }

        if apply {
            for (path, new) in &modified {
                std::fs::write(path, new)?;
            }
            for (uri, _) in &file_edits {
                let key = uri.as_str().to_string();
                if self.opened.contains(&key) {
                    if let Ok(t) = std::fs::read_to_string(uri_to_path(uri)) {
                        self.version += 1;
                        let _ = self.client.notify(
                            "textDocument/didChange",
                            json!({
                                "textDocument": { "uri": key, "version": self.version },
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

    /// Selector → LSP position. Shared by `resolve`, `rename`, `locations`, `hover`.
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
                let resp = self.document_symbols(uri)?;
                let (pos, kind, found) = find_symbol(&resp, name)
                    .ok_or_else(|| anyhow!("no symbol named `{name}` in {}", file.display()))?;
                let p = LspPos { line0: pos.line, char0_utf16: pos.character };
                Ok((p, json!({ "name": found, "kind": kind_name(kind) })))
            }
        }
    }

    fn document_symbols(&mut self, uri: &str) -> Result<DocumentSymbolResponse> {
        let res = self.client.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
            Duration::from_secs(20),
        )?;
        if res.is_null() {
            return Ok(DocumentSymbolResponse::Nested(vec![]));
        }
        serde_json::from_value(res).context("decoding documentSymbol response")
    }

    fn rel(&self, path: &str) -> String {
        rel_path(&self.root, path)
    }
}

// ---- shaping (typed in, JSON out) ---------------------------------------------------

fn shape_diagnostics(file: &Path, diags: &[Diagnostic]) -> Value {
    let mut items: Vec<(u64, u64, Value)> = diags
        .iter()
        .map(|d| {
            let sev = severity_num(d.severity);
            let line = d.range.start.line as u64 + 1;
            let col = d.range.start.character as u64 + 1;
            let code = match &d.code {
                Some(NumberOrString::Number(n)) => json!(n),
                Some(NumberOrString::String(s)) => json!(s),
                None => Value::Null,
            };
            let row = json!({
                "severity": severity_name(sev),
                "line": line,
                "col": col,
                "code": code,
                "source": d.source.clone(),
                "message": d.message.clone(),
            });
            (sev, line, row)
        })
        .collect();
    items.sort_by_key(|(sev, line, _)| (*sev, *line));

    let rows: Vec<Value> = items.into_iter().map(|(_, _, r)| r).collect();
    json!({ "file": file.display().to_string(), "count": rows.len(), "diagnostics": rows })
}

/// Flatten a definition/references result into a flat list of `Location`.
fn parse_locations(res: Value) -> Vec<Location> {
    if res.is_null() {
        return vec![];
    }
    match serde_json::from_value::<GotoDefinitionResponse>(res) {
        Ok(GotoDefinitionResponse::Scalar(l)) => vec![l],
        Ok(GotoDefinitionResponse::Array(ls)) => ls,
        Ok(GotoDefinitionResponse::Link(links)) => links
            .into_iter()
            .map(|l| Location { uri: l.target_uri, range: l.target_selection_range })
            .collect(),
        Err(_) => vec![],
    }
}

/// Shape locations into a capped, root-relative report carrying the source line.
fn shape_locations(root: &Path, locs: &[Location]) -> Value {
    const CAP: usize = 100;
    let mut cache: HashMap<String, Vec<String>> = HashMap::new();
    let rows: Vec<Value> = locs
        .iter()
        .take(CAP)
        .map(|loc| {
            let path = uri_to_path(&loc.uri);
            let line0 = loc.range.start.line;
            let col0 = loc.range.start.character;
            let lines = cache.entry(path.clone()).or_insert_with(|| {
                std::fs::read_to_string(&path)
                    .map(|t| t.lines().map(str::to_string).collect())
                    .unwrap_or_default()
            });
            let text = lines.get(line0 as usize).map(|s| s.trim_end().to_string()).unwrap_or_default();
            json!({ "file": rel_path(root, &path), "line": line0 + 1, "col": col0 + 1, "text": text })
        })
        .collect();
    json!({
        "count": locs.len(),
        "shown": rows.len(),
        "suppressed": locs.len().saturating_sub(rows.len()),
        "locations": rows,
    })
}

/// Shape a `workspace/symbol` response into name/kind/location rows.
#[allow(deprecated)] // SymbolInformation (the Flat variant) is deprecated in LSP but still
fn shape_workspace_symbols(root: &Path, res: Value) -> Value {
    const CAP: usize = 200;
    let mut rows: Vec<Value> = Vec::new();
    if let Ok(resp) = serde_json::from_value::<WorkspaceSymbolResponse>(res) {
        match resp {
            WorkspaceSymbolResponse::Flat(syms) => {
                for s in syms.into_iter().take(CAP) {
                    rows.push(sym_row(
                        root,
                        &s.name,
                        s.kind,
                        &s.location.uri,
                        s.location.range.start.line,
                        s.container_name,
                    ));
                }
            }
            WorkspaceSymbolResponse::Nested(syms) => {
                for s in syms.into_iter().take(CAP) {
                    let (uri, line) = match &s.location {
                        OneOf::Left(loc) => (loc.uri.clone(), loc.range.start.line),
                        OneOf::Right(wl) => (wl.uri.clone(), 0),
                    };
                    rows.push(sym_row(root, &s.name, s.kind, &uri, line, s.container_name));
                }
            }
        }
    }
    json!({ "count": rows.len(), "symbols": rows })
}

fn sym_row(
    root: &Path,
    name: &str,
    kind: SymbolKind,
    uri: &Uri,
    line: u32,
    container: Option<String>,
) -> Value {
    json!({
        "name": name,
        "kind": kind_name(kind),
        "file": rel_path(root, &uri_to_path(uri)),
        "line": line + 1,
        "container": container,
    })
}

/// Shape `callHierarchy/incomingCalls` into caller rows.
fn shape_callers(root: &Path, res: Value) -> Value {
    const CAP: usize = 100;
    let calls: Vec<CallHierarchyIncomingCall> = serde_json::from_value(res).unwrap_or_default();
    let rows: Vec<Value> = calls
        .iter()
        .take(CAP)
        .map(|c| {
            let f = &c.from;
            json!({
                "name": f.name,
                "kind": kind_name(f.kind),
                "file": rel_path(root, &uri_to_path(&f.uri)),
                "line": f.selection_range.start.line + 1,
                "detail": f.detail,
            })
        })
        .collect();
    json!({ "count": calls.len(), "callers": rows })
}

fn extract_hover(h: &Hover) -> String {
    match &h.contents {
        HoverContents::Scalar(ms) => marked_string(ms),
        HoverContents::Array(arr) => arr.iter().map(marked_string).collect::<Vec<_>>().join("\n"),
        HoverContents::Markup(mc) => mc.value.clone(),
    }
}

fn marked_string(ms: &MarkedString) -> String {
    match ms {
        MarkedString::String(s) => s.clone(),
        MarkedString::LanguageString(ls) => ls.value.clone(),
    }
}

fn shape_symbols(resp: &DocumentSymbolResponse) -> Value {
    let mut out = Vec::new();
    match resp {
        DocumentSymbolResponse::Nested(syms) => collect_symbols(syms, &mut out),
        DocumentSymbolResponse::Flat(syms) => {
            for s in syms {
                out.push(symbol_row(&s.name, s.kind, s.location.range.start));
            }
        }
    }
    json!({ "count": out.len(), "symbols": out })
}

fn collect_symbols(syms: &[DocumentSymbol], out: &mut Vec<Value>) {
    for s in syms {
        out.push(symbol_row(&s.name, s.kind, s.selection_range.start));
        if let Some(children) = &s.children {
            collect_symbols(children, out);
        }
    }
}

fn symbol_row(name: &str, kind: SymbolKind, pos: Position) -> Value {
    json!({ "name": name, "kind": kind_name(kind), "line": pos.line + 1, "col": pos.character + 1 })
}

/// Find a symbol by exact name in either symbol response shape.
fn find_symbol(resp: &DocumentSymbolResponse, name: &str) -> Option<(Position, SymbolKind, String)> {
    match resp {
        DocumentSymbolResponse::Nested(syms) => find_nested(syms, name),
        DocumentSymbolResponse::Flat(syms) => syms
            .iter()
            .find(|s| s.name == name)
            .map(|s| (s.location.range.start, s.kind, s.name.clone())),
    }
}

fn find_nested(syms: &[DocumentSymbol], name: &str) -> Option<(Position, SymbolKind, String)> {
    for s in syms {
        if s.name == name {
            return Some((s.selection_range.start, s.kind, s.name.clone()));
        }
        if let Some(children) = &s.children {
            if let Some(found) = find_nested(children, name) {
                return Some(found);
            }
        }
    }
    None
}

// ---- WorkspaceEdit → text edits + diff ----------------------------------------------

/// Split a WorkspaceEdit into (per-file text edits, serialized resource operations).
fn collect_changes(edit: &WorkspaceEdit) -> (Vec<(Uri, Vec<TextEdit>)>, Vec<Value>) {
    let mut files = Vec::new();
    let mut ops = Vec::new();

    if let Some(changes) = &edit.changes {
        for (uri, edits) in changes {
            files.push((uri.clone(), edits.clone()));
        }
    }
    if let Some(dc) = &edit.document_changes {
        match dc {
            DocumentChanges::Edits(tdes) => {
                for tde in tdes {
                    files.push((tde.text_document.uri.clone(), text_edits(&tde.edits)));
                }
            }
            DocumentChanges::Operations(operations) => {
                for op in operations {
                    match op {
                        DocumentChangeOperation::Edit(tde) => {
                            files.push((tde.text_document.uri.clone(), text_edits(&tde.edits)));
                        }
                        DocumentChangeOperation::Op(resop) => {
                            if let Ok(v) = serde_json::to_value(resop) {
                                ops.push(v);
                            }
                        }
                    }
                }
            }
        }
    }
    (files, ops)
}

fn text_edits(edits: &[OneOf<TextEdit, lsp_types::AnnotatedTextEdit>]) -> Vec<TextEdit> {
    edits
        .iter()
        .map(|e| match e {
            OneOf::Left(te) => te.clone(),
            OneOf::Right(a) => a.text_edit.clone(),
        })
        .collect()
}

/// Apply TextEdits to `text`, right-to-left so earlier byte offsets stay valid.
fn apply_edits(text: &str, edits: &[TextEdit]) -> String {
    let mut sorted: Vec<&TextEdit> = edits.iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse((e.range.start.line, e.range.start.character)));

    let mut s = text.to_string();
    for e in sorted {
        let start = byte_offset(text, e.range.start.line, e.range.start.character);
        let end = byte_offset(text, e.range.end.line, e.range.end.character);
        if start <= end && end <= s.len() {
            s.replace_range(start..end, &e.new_text);
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

// ---- small helpers ------------------------------------------------------------------

fn unified_diff(rel: &str, old: &str, new: &str) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{rel}"), &format!("b/{rel}"))
        .to_string()
}

/// Convert an LSP `Uri` to a filesystem path string (percent-decoding via `url`).
fn uri_to_path(uri: &Uri) -> String {
    Url::parse(uri.as_str())
        .ok()
        .and_then(|u| u.to_file_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| uri.as_str().to_string())
}

/// The first source file under `root` matching the language (bounded DFS, skips build dirs).
fn first_source_file(root: &Path, lang_id: &str) -> Option<PathBuf> {
    let exts: &[&str] = match lang_id {
        "rust" => &["rs"],
        "typescript" => &["ts", "tsx", "mts", "cts"],
        _ => &[],
    };
    let skip = ["target", "node_modules", ".git", "dist", "build", ".cargo"];
    let mut stack = vec![root.to_path_buf()];
    let mut budget = 5000;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            if budget == 0 {
                return None;
            }
            budget -= 1;
            let path = entry.path();
            if path.is_dir() {
                let skipped = path.file_name().is_some_and(|n| skip.iter().any(|s| n == *s));
                if !skipped {
                    stack.push(path);
                }
            } else if path.extension().and_then(|x| x.to_str()).is_some_and(|x| exts.contains(&x)) {
                return Some(path);
            }
        }
    }
    None
}

/// A workspace-root-relative path (falls back to the absolute path if outside root).
fn rel_path(root: &Path, path: &str) -> String {
    Path::new(path)
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
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
    line.chars().take_while(|c| c.is_whitespace()).count() as u32 + 1
}

fn severity_num(s: Option<DiagnosticSeverity>) -> u64 {
    s.and_then(|s| serde_json::to_value(s).ok())
        .and_then(|v| v.as_u64())
        .unwrap_or(1)
}

fn severity_name(n: u64) -> &'static str {
    match n {
        1 => "error",
        2 => "warning",
        3 => "information",
        4 => "hint",
        _ => "unknown",
    }
}

fn kind_name(k: SymbolKind) -> &'static str {
    let n = serde_json::to_value(k).ok().and_then(|v| v.as_u64()).unwrap_or(0);
    match n {
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
    }
}
