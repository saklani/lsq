//! LSP plumbing: language detection, backend selection (see [`backend`]), the JSON-RPC
//! [`client`], and the warm [`session`]. Which server to run and how it behaves lives in
//! [`backend`]; the rest of lsq stays ignorant of the choice.

pub mod backend;
pub mod client;
pub mod session;

pub use backend::{Backend, resolve_for};

use std::path::{Path, PathBuf};

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
}

/// Locate an executable: PATH first, then a project-local `node_modules/.bin`.
pub(crate) fn which(bin: &str) -> Option<String> {
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

/// A `file://` URI for an absolute path, with correct percent-encoding (via `url`).
pub fn path_to_uri(p: &Path) -> String {
    url::Url::from_file_path(p)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{}", p.to_string_lossy()))
}
