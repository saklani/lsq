# lsq

Agent-native semantic code queries for **TypeScript** and **Rust** — IDE-grade operations
(diagnostics, rename, go-to-symbol) exposed as a fast CLI an agent can invoke like `ripgrep`.

`lsq` is an LSP **client**: it drives a real language server (tsgo for TypeScript,
rust-analyzer for Rust) and returns structured, token-budgeted output. A per-workspace
**daemon** keeps the server warm, so you pay project indexing once — not on every call.

> **Status:** early prototype. Working today: `backend`, `diagnostics`, `resolve`, `rename`.

## Why

For an agent, semantic operations are a correctness *and* token multiplier. A rename via
`grep | sed` is a multi-pass, error-prone dance — reason about which hits are the real
symbol vs. comments/strings, edit, re-grep, notice the missed import. A semantic rename is
**one call that returns a diff** touching the binding and only the binding.

## Commands

```
lsq backend                                 # detect available language servers
lsq diagnostics <file>                      # ranked diagnostics for a file (JSON)
lsq resolve <selector>                      # resolve a selector to an exact position
lsq rename  <selector> <new_name> [--apply] # semantic rename; prints a unified diff
```

### Selectors

Address a symbol without computing UTF-16 offsets by hand:

- `FILE:LINE:COL` — explicit 1-based position
- `FILE:LINE` — the first symbol on a line
- `FILE@NAME` — the symbol named `NAME` (located via the language server)

### It renames itself

```
$ lsq rename src/lsp/mod.rs@ResolvedBackend BackendSpec
# → 11 edits across 4 files, rendered as a unified diff. Pass --apply to write.
```

## Architecture

```
lsq <verb>                 thin client (plain blocking std — fast startup)
   └── unix socket
        └── lsq __daemon    warm, one per workspace root
             ├── LSP client  ──►  tsgo  /  rust-analyzer
             ├── selectors        (name → exact position)
             └── output shaping   (structured JSON, unified diffs)
```

- **Backends:** tsgo (`tsgo --lsp -stdio`) for TypeScript, rust-analyzer for Rust.
  rust-analyzer is launched lean (proc-macro, build-scripts, and check-on-save disabled).
- **Typed positions:** `UserPos` (1-based char) and `LspPos` (0-based UTF-16) are distinct
  types, so the two coordinate systems can't be confused.
- The daemon idles out after 5 minutes and kills its child language servers on exit.

## Build

```
cargo build
cargo test
```

You also need a language server available (on `PATH` or in `node_modules/.bin`):

- TypeScript: `npm i @typescript/native-preview` (provides `tsgo`)
- Rust: `rustup component add rust-analyzer`

## Roadmap

Done: LSP client core, warm daemon, selectors, rename.
Next: `refs` (find references), `type` (type-at-point), `def` (go-to-definition),
publish-version tracking for instant warm calls, a tree-sitter text fallback, and Windows
named-pipe support.

---

An exploration / prototype. Unrelated to the other `lsq` packages on npm / crates.io.
