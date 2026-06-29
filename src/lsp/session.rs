//! A *warm* backend session: an LSP server initialized once and kept alive, plus the
//! per-verb operations run against it. Responses are deserialized into typed `lsp_types`
//! structs rather than hand-parsed JSON.

use super::client::LspClient;
use super::{Backend, path_to_uri};
use crate::position::{LspPos, UserPos};
use crate::selector::Selector;
use anyhow::{Context, Result, anyhow};
use lsp_types::{
    CallHierarchyIncomingCall, CodeAction, CodeActionKind, CodeActionOrCommand, Diagnostic,
    DiagnosticSeverity, DocumentChangeOperation, DocumentChanges, DocumentSymbol,
    DocumentSymbolResponse, GotoDefinitionResponse, Hover, HoverContents, Location, MarkedString,
    NumberOrString, OneOf, Position, SymbolKind, TextEdit, Uri, WorkspaceEdit,
    WorkspaceSymbolResponse,
};
use serde_json::{Value, json};
use similar::TextDiff;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use url::Url;
use walkdir::WalkDir;

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
                        "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                        // code actions as objects (not bare Commands), with deferred-edit
                        // resolution so `fix` can preview/apply the WorkspaceEdit
                        "codeAction": {
                            "codeActionLiteralSupport": {
                                "codeActionKind": {
                                    "valueSet": [
                                        "quickfix", "refactor", "refactor.extract",
                                        "refactor.inline", "refactor.rewrite",
                                        "source", "source.organizeImports", "source.fixAll"
                                    ]
                                }
                            },
                            "resolveSupport": { "properties": ["edit"] },
                            "dataSupport": true
                        }
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
        let mut diags: Vec<Diagnostic> = raw
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect();
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
        let res = self
            .client
            .request(method, params, Duration::from_secs(30))?;
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
                if self.opened.contains(&key)
                    && let Ok(t) = std::fs::read_to_string(uri_to_path(uri))
                {
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

        Ok(json!({
            "new_name": new_name,
            "files_changed": modified.len(),
            "edits": total_edits,
            "applied": apply,
            "resource_operations": resource_ops,
            "diff": diff,
        }))
    }

    /// Code actions (quick-fixes / refactors) available at `sel` — the read-then-write verb
    /// the host's built-in LSP can't do, since it returns mutating `WorkspaceEdit`s. Lists
    /// every actionable code action as a dry-run; with `apply`, writes the edits of the action
    /// at index `pick` (default 0) to disk. The diagnostics overlapping the position drive
    /// quick-fixes, so they're collected and passed as context.
    pub fn fix(&mut self, sel: &Selector, file: &Path, apply: bool, pick: usize) -> Result<Value> {
        let uri = self.sync_doc(file)?;
        self.backend.await_ready(&mut self.client, &uri)?;
        let text = std::fs::read_to_string(file)?;
        let (pos, _) = self.locate(sel, file, &text, &uri)?;

        // Quick-fixes are scoped to the diagnostics under the cursor; pass the ones whose
        // range spans this line as context, and anchor the request range on the first of them
        // (a server only returns an action whose range intersects the request range).
        let raw = self.backend.collect_diagnostics(&mut self.client, &uri)?;
        let here: Vec<Diagnostic> = raw
            .into_iter()
            .filter_map(|v| serde_json::from_value::<Diagnostic>(v).ok())
            .filter(|d| d.range.start.line <= pos.line0 && pos.line0 <= d.range.end.line)
            .collect();
        let range = here
            .first()
            .map(|d| {
                json!({
                    "start": { "line": d.range.start.line, "character": d.range.start.character },
                    "end": { "line": d.range.end.line, "character": d.range.end.character }
                })
            })
            .unwrap_or_else(|| {
                json!({
                    "start": { "line": pos.line0, "character": pos.char0_utf16 },
                    "end": { "line": pos.line0, "character": pos.char0_utf16 }
                })
            });

        let res = self.client.request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": uri },
                "range": range,
                "context": { "diagnostics": here }
            }),
            Duration::from_secs(30),
        )?;

        // Decode typed, keeping CodeActions and dropping bare Commands — only an action that
        // carries (or resolves to) a WorkspaceEdit can be previewed/applied. The untagged
        // `CodeActionOrCommand` routes a bare `{title, command: "id"}` to `Command` because
        // its `command` is a string, not the object a `CodeAction.command` expects.
        let response: Vec<CodeActionOrCommand> = if res.is_null() {
            Vec::new()
        } else {
            serde_json::from_value(res).context("decoding codeAction response")?
        };
        let actions: Vec<CodeAction> = response
            .into_iter()
            .filter_map(|a| match a {
                CodeActionOrCommand::CodeAction(ca) => Some(ca),
                CodeActionOrCommand::Command(_) => None,
            })
            .collect();

        if actions.is_empty() {
            return Ok(json!({
                "count": 0, "actions": [], "applied": false,
                "message": "no code actions available at this position",
            }));
        }

        let listed: Vec<Value> = actions
            .iter()
            .enumerate()
            .map(|(i, ca)| {
                json!({
                    "index": i,
                    "title": ca.title,
                    "kind": ca.kind.as_ref().map(CodeActionKind::as_str),
                    "preferred": ca.is_preferred.unwrap_or(false),
                })
            })
            .collect();

        // Preview/apply the chosen action; resolve its edit lazily if the server deferred it.
        // `CodeAction` carries the server's opaque `data`, so the resolve round-trip stays typed.
        let picked = pick.min(actions.len() - 1);
        let mut action = actions[picked].clone();
        if action.edit.is_none()
            && let Ok(resolved) = self.client.request(
                "codeAction/resolve",
                serde_json::to_value(&action)?,
                Duration::from_secs(30),
            )
            && let Ok(ca) = serde_json::from_value::<CodeAction>(resolved)
        {
            action = ca;
        }

        let mut diff = String::new();
        let mut modified: Vec<(String, String)> = Vec::new();
        let mut total_edits = 0usize;
        let mut resource_ops = Vec::new();
        if let Some(edit) = &action.edit {
            let (file_edits, ops) = collect_changes(edit);
            resource_ops = ops;
            for (u, edits) in &file_edits {
                if edits.is_empty() {
                    continue;
                }
                total_edits += edits.len();
                let path = uri_to_path(u);
                let old = std::fs::read_to_string(&path).unwrap_or_default();
                let new = apply_edits(&old, edits);
                diff.push_str(&unified_diff(&self.rel(&path), &old, &new));
                modified.push((path, new));
            }
        }

        if apply {
            for (path, new) in &modified {
                std::fs::write(path, new)?;
            }
            // Keep the server's view in sync with what we just wrote (mirror `rename`).
            for (path, _) in &modified {
                let key = path_to_uri(Path::new(path));
                if self.opened.contains(&key)
                    && let Ok(t) = std::fs::read_to_string(path)
                {
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

        Ok(json!({
            "count": actions.len(),
            "actions": listed,
            "picked": picked,
            "title": action.title,
            "files_changed": modified.len(),
            "edits": total_edits,
            "applied": apply,
            // Some actions carry only a server-side command (e.g. "run codegen"); we can't
            // apply those as a text edit, so flag it rather than silently producing no diff.
            "command_only": total_edits == 0 && action.command.is_some(),
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
                let p = LspPos {
                    line0: pos.line,
                    char0_utf16: pos.character,
                };
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
            .map(|l| Location {
                uri: l.target_uri,
                range: l.target_selection_range,
            })
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
fn find_symbol(
    resp: &DocumentSymbolResponse,
    name: &str,
) -> Option<(Position, SymbolKind, String)> {
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
        if let Some(children) = &s.children
            && let Some(found) = find_nested(children, name)
        {
            return Some(found);
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
    let line0 = line0 as usize;
    let mut offset = 0usize;
    for (cur_line, line) in text.split_inclusive('\n').enumerate() {
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

/// The first source file under `root` matching the language, skipping build/vendor dirs.
fn first_source_file(root: &Path, lang_id: &str) -> Option<PathBuf> {
    let exts: &[&str] = match lang_id {
        "rust" => &["rs"],
        "typescript" => &["ts", "tsx", "mts", "cts"],
        _ => return None,
    };
    let skip = ["target", "node_modules", ".git", "dist", "build", ".cargo"];
    WalkDir::new(root)
        .into_iter()
        // prune build/vendor subtrees (don't descend into them)
        .filter_entry(|e| !e.file_name().to_str().is_some_and(|n| skip.contains(&n)))
        .filter_map(Result::ok)
        .find(|e| {
            e.file_type().is_file()
                && e.path()
                    .extension()
                    .and_then(|x| x.to_str())
                    .is_some_and(|x| exts.contains(&x))
        })
        .map(walkdir::DirEntry::into_path)
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

/// Severity as a 1..=4 rank (errors first) for sorting. Errors default if unspecified.
fn severity_num(s: Option<DiagnosticSeverity>) -> u64 {
    match s {
        Some(DiagnosticSeverity::WARNING) => 2,
        Some(DiagnosticSeverity::INFORMATION) => 3,
        Some(DiagnosticSeverity::HINT) => 4,
        _ => 1, // ERROR (and the unspecified case)
    }
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
    match k {
        SymbolKind::MODULE => "module",
        SymbolKind::CLASS => "class",
        SymbolKind::METHOD => "method",
        SymbolKind::FIELD => "field",
        SymbolKind::ENUM => "enum",
        SymbolKind::INTERFACE => "interface",
        SymbolKind::FUNCTION => "function",
        SymbolKind::VARIABLE => "variable",
        SymbolKind::CONSTANT => "constant",
        SymbolKind::ENUM_MEMBER => "enum_member",
        SymbolKind::STRUCT => "struct",
        SymbolKind::TYPE_PARAMETER => "type_parameter",
        _ => "unknown",
    }
}
