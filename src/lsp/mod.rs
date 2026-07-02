//! LSP plumbing: language detection, per-language [`adapter`] selection, the JSON-RPC
//! [`client`] connection, and the warm [`session`]. Which server to run and how it behaves
//! lives in [`adapter`]; the rest of lsq stays ignorant of the choice.

pub mod adapter;
pub mod client;
pub mod session;
mod transform;

pub use adapter::{Adapter, resolve_for};

use std::path::{Path, PathBuf};

/// A source language we can resolve a client for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Language {
    Rust,
    TypeScript,
    Python,
}

impl Language {
    /// Infer the language from a file extension.
    pub fn from_path(p: &Path) -> Option<Language> {
        match p.extension().and_then(|e| e.to_str()) {
            Some("rs") => Some(Language::Rust),
            Some("ts" | "tsx" | "mts" | "cts") => Some(Language::TypeScript),
            Some("py" | "pyi") => Some(Language::Python),
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
