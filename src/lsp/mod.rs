//! The LSP backend seam + the LSP client.
//!
//! `mod.rs` decides WHICH language server to run and how to launch it; [`client`] is the
//! actual JSON-RPC client that drives it. The rest of lsq stays ignorant of the choice.

pub mod client;
pub mod session;

use anyhow::{Result, anyhow};
use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Which language server a [`ResolvedBackend`] refers to.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum Backend {
    /// tsgo — `tsgo --lsp -stdio`. Preferred for TypeScript (native, fast).
    Tsgo,
    /// typescript-language-server — `... --stdio`. TypeScript fallback.
    TypescriptLanguageServer,
    /// rust-analyzer — speaks LSP on stdio with no flags. The dogfood backend.
    RustAnalyzer,
}

/// A source language we can resolve a backend for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    TypeScript,
}

impl Language {
    /// Infer the language from a file extension.
    pub fn from_path(p: &Path) -> Option<Language> {
        match p.extension().and_then(|e| e.to_str()) {
            Some("rs") => Some(Language::Rust),
            Some("ts" | "tsx" | "mts" | "cts") => Some(Language::TypeScript),
            _ => None,
        }
    }

    /// The LSP `languageId` used in `didOpen`.
    pub fn language_id(self) -> &'static str {
        match self {
            Language::Rust => "rust",
            Language::TypeScript => "typescript",
        }
    }

    /// Candidate backends in preference order.
    fn candidates(self) -> Vec<Backend> {
        match self {
            Language::Rust => vec![Backend::RustAnalyzer],
            Language::TypeScript => vec![Backend::Tsgo, Backend::TypescriptLanguageServer],
        }
    }
}

/// A concrete, launchable backend: which kind, how to start its LSP server, its version.
#[derive(Debug, Clone, Serialize)]
pub struct ResolvedBackend {
    pub kind: Backend,
    pub program: String,
    pub args: Vec<String>,
    pub version: String,
}

impl ResolvedBackend {
    /// A JSON description for `lsq backend`.
    pub fn describe(&self) -> Value {
        json!({
            "kind": self.kind,
            "program": self.program,
            "args": self.args,
            "version": self.version,
            "lsp_stdio_cmd": format!("{} {}", self.program, self.args.join(" ")),
        })
    }

    /// Backend-specific `initializationOptions` for the `initialize` request.
    ///
    /// For rust-analyzer we deliberately strip the heavy/crash-prone machinery
    /// (proc-macro server, build scripts, cargo-check-on-save) — we only need its
    /// native analysis to refactor our own symbols. Returns `Null` when there's nothing.
    pub fn initialization_options(&self) -> Value {
        match self.kind {
            Backend::RustAnalyzer => json!({
                "procMacro": { "enable": false },
                "cargo": { "buildScripts": { "enable": false } },
                "checkOnSave": false,
                "check": { "workspace": false }
            }),
            _ => Value::Null,
        }
    }
}

/// Resolve the preferred available backend for `lang`.
pub fn resolve_for(lang: Language) -> Result<ResolvedBackend> {
    for kind in lang.candidates() {
        if let Some(b) = find(kind) {
            return Ok(b);
        }
    }
    Err(anyhow!("no LSP backend installed for {lang:?}"))
}

/// Every backend we can find on this machine (for `lsq backend`).
pub fn detect_all() -> Vec<ResolvedBackend> {
    [
        Backend::RustAnalyzer,
        Backend::Tsgo,
        Backend::TypescriptLanguageServer,
    ]
    .into_iter()
    .filter_map(find)
    .collect()
}

fn find(kind: Backend) -> Option<ResolvedBackend> {
    let (bin, args): (&str, Vec<String>) = match kind {
        Backend::Tsgo => ("tsgo", vec!["--lsp".into(), "-stdio".into()]),
        Backend::TypescriptLanguageServer => {
            ("typescript-language-server", vec!["--stdio".into()])
        }
        Backend::RustAnalyzer => ("rust-analyzer", vec![]),
    };
    let program = which(bin)?;
    let version = run_version(&program)?;
    Some(ResolvedBackend { kind, program, args, version })
}

/// Locate an executable: PATH first, then a project-local `node_modules/.bin`.
fn which(bin: &str) -> Option<String> {
    if let Ok(path) = std::env::var("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(bin);
            if candidate.is_file() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }
    for base in ["node_modules/.bin", "scratch/node_modules/.bin"] {
        let candidate = PathBuf::from(base).join(bin);
        if candidate.is_file() {
            return Some(candidate.to_string_lossy().into_owned());
        }
    }
    None
}

fn run_version(program: &str) -> Option<String> {
    let out = Command::new(program).arg("--version").output().ok()?;
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Minimal `file://` URI (absolute path, no percent-encoding yet — fine for the prototype).
pub fn path_to_uri(p: &Path) -> String {
    format!("file://{}", p.to_string_lossy())
}
