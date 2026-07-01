//! The pure transform layer: raw LSP responses / edits in, agent-facing JSON / text out.
//!
//! Stateless — no server, no `self`. This is the counterpart to the stateful
//! [`Session`](super::session::Session): the session runs the LSP conversation, and hands the
//! raw responses here to be shaped into answers (and to turn a `WorkspaceEdit` into applied
//! text + a diff). Being server-less is what lets the unit tests exercise it directly.

use lsp_types::{
    AnnotatedTextEdit, CallHierarchyItem, Diagnostic, DiagnosticSeverity, DocumentChangeOperation,
    DocumentChanges, DocumentSymbol, DocumentSymbolResponse, GotoDefinitionResponse, Hover,
    HoverContents, Location, MarkedString, NumberOrString, OneOf, Position, SymbolKind, TextEdit,
    Uri, WorkspaceEdit, WorkspaceSymbolResponse,
};
use serde_json::{Value, json};
use similar::TextDiff;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use url::Url;

// ---- shaping (typed in, JSON out) ---------------------------------------------------

/// Raw diagnostic JSON → typed, deduped, shaped. Shared by `diagnostics` and `check`.
pub(crate) fn shape_raw_diagnostics(file: &Path, raw: Vec<Value>) -> Value {
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
    shape_diagnostics(file, &diags)
}

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
pub(crate) fn parse_locations(res: Value) -> Vec<Location> {
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
pub(crate) fn shape_locations(root: &Path, locs: &[Location]) -> Value {
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
#[allow(deprecated)] // SymbolInformation (the Flat variant) is deprecated in LSP but still sent
pub(crate) fn shape_workspace_symbols(root: &Path, res: Value) -> Value {
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

/// Shape call-hierarchy items into rows under `key` (shared by `callers` and `callees`; each
/// unwraps its own `from`/`to` edge to a `CallHierarchyItem` before calling this).
pub(crate) fn shape_call_items(root: &Path, items: &[&CallHierarchyItem], key: &str) -> Value {
    const CAP: usize = 100;
    let rows: Vec<Value> = items
        .iter()
        .take(CAP)
        .map(|f| {
            json!({
                "name": f.name,
                "kind": kind_name(f.kind),
                "file": rel_path(root, &uri_to_path(&f.uri)),
                "line": f.selection_range.start.line + 1,
                "detail": f.detail,
            })
        })
        .collect();
    let mut out = json!({ "count": items.len() });
    out[key] = Value::Array(rows);
    out
}

pub(crate) fn extract_hover(h: &Hover) -> String {
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

pub(crate) fn shape_symbols(resp: &DocumentSymbolResponse) -> Value {
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
pub(crate) fn find_symbol(
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
pub(crate) fn collect_changes(edit: &WorkspaceEdit) -> (Vec<(Uri, Vec<TextEdit>)>, Vec<Value>) {
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

fn text_edits(edits: &[OneOf<TextEdit, AnnotatedTextEdit>]) -> Vec<TextEdit> {
    edits
        .iter()
        .map(|e| match e {
            OneOf::Left(te) => te.clone(),
            OneOf::Right(a) => a.text_edit.clone(),
        })
        .collect()
}

/// Apply TextEdits to `text`, right-to-left so earlier byte offsets stay valid.
pub(crate) fn apply_edits(text: &str, edits: &[TextEdit]) -> String {
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

/// A unified diff between `old` and `new`, headered with the (relative) path.
pub(crate) fn unified_diff(rel: &str, old: &str, new: &str) -> String {
    TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{rel}"), &format!("b/{rel}"))
        .to_string()
}

// ---- path + label helpers -----------------------------------------------------------

/// Convert an LSP `Uri` to a filesystem path string (percent-decoding via `url`).
pub(crate) fn uri_to_path(uri: &Uri) -> String {
    Url::parse(uri.as_str())
        .ok()
        .and_then(|u| u.to_file_path().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| uri.as_str().to_string())
}

/// A workspace-root-relative path (falls back to the absolute path if outside root).
pub(crate) fn rel_path(root: &Path, path: &str) -> String {
    Path::new(path)
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
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

pub(crate) fn kind_name(k: SymbolKind) -> &'static str {
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

// ---- tests: the pure transform layer (LSP JSON -> agent JSON), no server needed ----

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::CallHierarchyIncomingCall;

    fn text_edit(sl: u32, sc: u32, el: u32, ec: u32, new: &str) -> TextEdit {
        serde_json::from_value(json!({
            "range": { "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } },
            "newText": new
        }))
        .unwrap()
    }

    fn diag(line: u32, sev: u8, code: &str, msg: &str) -> Value {
        json!({
            "range": { "start": { "line": line, "character": 0 }, "end": { "line": line, "character": 1 } },
            "severity": sev, "code": code, "message": msg
        })
    }

    // --- byte_offset: the UTF-16 -> byte math that anchors every edit ---

    #[test]
    fn byte_offset_ascii_lines() {
        let t = "hello\nworld";
        assert_eq!(byte_offset(t, 0, 0), 0);
        assert_eq!(byte_offset(t, 1, 0), 6); // after "hello\n"
        assert_eq!(byte_offset(t, 1, 2), 8); // 'r' in world
    }

    #[test]
    fn byte_offset_counts_utf16_not_chars() {
        // 😀 is 1 char, 2 UTF-16 units, 4 UTF-8 bytes
        let t = "a😀b";
        assert_eq!(byte_offset(t, 0, 1), 1); // start of the emoji
        assert_eq!(byte_offset(t, 0, 3), 5); // 'b', after a(1) + 😀(4) bytes
    }

    // --- apply_edits: rename/fix correctness, incl. right-to-left application ---

    #[test]
    fn apply_edits_single_replacement() {
        assert_eq!(
            apply_edits("let x = 1;", &[text_edit(0, 4, 0, 5, "yy")]),
            "let yy = 1;"
        );
    }

    #[test]
    fn apply_edits_multiple_on_one_line_stay_aligned() {
        // two edits whose byte offsets would shift if applied left-to-right
        let out = apply_edits(
            "a b c",
            &[text_edit(0, 0, 0, 1, "XX"), text_edit(0, 4, 0, 5, "Z")],
        );
        assert_eq!(out, "XX b Z");
    }

    // --- diagnostics shaping: dedup + severity-then-line ordering ---

    #[test]
    fn diagnostics_dedup_and_sort_errors_first() {
        let raw = vec![
            diag(5, 2, "W", "warn"),
            diag(2, 1, "E", "err"),
            diag(5, 2, "W", "warn"), // exact duplicate
        ];
        let out = shape_raw_diagnostics(Path::new("f.rs"), raw);
        assert_eq!(out["count"], 2); // duplicate removed
        let rows = out["diagnostics"].as_array().unwrap();
        assert_eq!(rows[0]["severity"], "error"); // severity 1 sorts before 2
        assert_eq!(rows[0]["line"], 3); // 0-based line 2 -> 1-based 3
        assert_eq!(rows[1]["severity"], "warning");
    }

    // --- parse_locations: the three goto-response shapes + null ---

    #[test]
    fn parse_locations_handles_all_shapes() {
        assert!(parse_locations(Value::Null).is_empty());
        let one = json!({ "uri": "file:///a.rs", "range": { "start": { "line": 1, "character": 2 }, "end": { "line": 1, "character": 5 } } });
        assert_eq!(parse_locations(one).len(), 1); // Scalar
        let many = json!([
            { "uri": "file:///a.rs", "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 4 } } },
            { "uri": "file:///b.rs", "range": { "start": { "line": 9, "character": 0 }, "end": { "line": 9, "character": 4 } } }
        ]);
        assert_eq!(parse_locations(many).len(), 2); // Array
    }

    // --- shape_call_items: from-edge -> rows, 1-based line from selectionRange ---

    #[test]
    fn shape_call_items_rows_use_selection_range() {
        let calls: Vec<CallHierarchyIncomingCall> = serde_json::from_value(json!([{
            "from": {
                "name": "caller_fn", "kind": 12, "uri": "file:///x.rs",
                "range": { "start": { "line": 9, "character": 0 }, "end": { "line": 12, "character": 1 } },
                "selectionRange": { "start": { "line": 9, "character": 3 }, "end": { "line": 9, "character": 12 } }
            },
            "fromRanges": []
        }]))
        .unwrap();
        let items: Vec<&CallHierarchyItem> = calls.iter().map(|c| &c.from).collect();
        let out = shape_call_items(Path::new("/"), &items, "callers");
        assert_eq!(out["count"], 1);
        assert_eq!(out["callers"][0]["name"], "caller_fn");
        assert_eq!(out["callers"][0]["kind"], "function"); // SymbolKind 12
        assert_eq!(out["callers"][0]["line"], 10); // selection start line 9 + 1
    }

    // --- collect_changes: WorkspaceEdit (changes map) -> per-file edits ---

    #[test]
    fn collect_changes_splits_changes_map() {
        let edit: WorkspaceEdit = serde_json::from_value(json!({
            "changes": {
                "file:///a.rs": [
                    { "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 3 } }, "newText": "foo" }
                ]
            }
        }))
        .unwrap();
        let (files, ops) = collect_changes(&edit);
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].1.len(), 1);
        assert!(ops.is_empty());
    }

    // --- the small mappings we hand-rolled (guard the lsp_types const matching) ---

    #[test]
    fn kind_and_severity_mappings() {
        assert_eq!(kind_name(SymbolKind::STRUCT), "struct");
        assert_eq!(kind_name(SymbolKind::INTERFACE), "interface");
        assert_eq!(kind_name(SymbolKind::FUNCTION), "function");
        assert_eq!(severity_num(Some(DiagnosticSeverity::ERROR)), 1);
        assert_eq!(severity_num(Some(DiagnosticSeverity::HINT)), 4);
        assert_eq!(severity_num(None), 1); // unspecified defaults to error rank
        assert_eq!(severity_name(2), "warning");
    }
}
