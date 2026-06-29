# lsq

Agent-native semantic code queries for **TypeScript** and **Rust**, exposed as an **MCP
server**. IDE-grade operations — references, rename, diagnostics, call hierarchy, code
actions — delivered over the Model Context Protocol as structured, token-budgeted JSON.

`lsq` is an LSP **client** wearing an MCP **face**: it drives a real language server (tsgo
for TypeScript, rust-analyzer for Rust) and holds the session warm **in-process**, so you
pay project indexing once — not on every call. There is no separate daemon and no CLI; the
single binary *is* the MCP server.

> **Status:** prototype. The 12 tools below work; `fix` is reliable against rust-analyzer
> (tsgo's code-action support is still partial).

## Why

For an agent, semantic operations are a correctness *and* token multiplier. A rename via
`grep | sed` is a multi-pass, error-prone dance — reason about which hits are the real
symbol vs. comments/strings, edit, re-grep, notice the missed import. A semantic rename is
**one call that returns a diff** touching the binding and only the binding. The host's
built-in LSP tooling is read-only; `lsq` adds the *writes* (`rename`, `fix`) and a warm,
shaped layer on top.

## Tools

| Tool | What it does |
|---|---|
| `refs` | all references to the symbol at a selector |
| `def` / `impl` / `type-def` | go to definition / implementation(s) / type definition |
| `type` | type / hover info at a selector |
| `symbols` | symbols defined in a file |
| `callers` | incoming calls of a function (call hierarchy) |
| `diagnostics` | ranked diagnostics for a file |
| `rename` | semantic rename across the workspace → unified diff (`apply` to write) |
| `fix` | code actions (quick-fixes / refactors) → diff (`apply` to write) |
| `resolve` | resolve a selector to an exact position |

### Selectors

Address a symbol without computing UTF-16 offsets by hand:

- `FILE:LINE:COL` — explicit 1-based position
- `FILE:LINE` — the first symbol on a line
- `FILE@NAME` — the symbol named `NAME` (located via the language server)

## Setup

Build the binary:

```
cargo build --release        # → target/release/lsq
```

Register it with any MCP client. A project-scoped `.mcp.json` is included:

```json
{ "mcpServers": { "lsq": { "command": "target/release/lsq", "args": ["mcp"] } } }
```

(The `mcp` argument is conventional and ignored — the binary always serves MCP on stdio.)
Run the server from the workspace root; it roots its sessions at the launch directory.

You also need a language server reachable on the server's `PATH`:

- TypeScript: `bun add -g @typescript/native-preview` (provides `tsgo`; it's a Node shim, so
  `node` must be on `PATH` too)
- Rust: `rustup component add rust-analyzer`

## Architecture

```
MCP client ──stdio JSON-RPC──►  lsq (mcp.rs)        the only front-end
                                    │ spawn_blocking
                                    ▼
                                engine.rs           dispatch + warm Session per language
                                    ├── lsp/session  selectors, typed lsp_types, output shaping
                                    └── lsp/backend  tsgo / rust-analyzer / typescript-language-server
```

- **Backends** live behind a `Backend` trait — launch args, init options, readiness, and the
  diagnostics model (push vs. pull) are encapsulated per server; the engine never branches on
  which one is running. rust-analyzer is launched lean (proc-macro, build-scripts, and
  check-on-save disabled).
- **Warm in-process sessions:** the MCP server is long-lived, so it keeps one `Session` per
  language and reuses it across tool calls — `initialize` + indexing is paid once.
- **Typed positions:** `UserPos` (1-based char) and `LspPos` (0-based UTF-16) are distinct
  types, so the two coordinate systems can't be confused.

## Build & test

```
cargo build
cargo clippy -- -D warnings
cargo test                  # unit tests
python3 scripts/e2e.py      # end-to-end: drives the MCP server against ripgrep + ky
```

---

An exploration / prototype. Unrelated to the other `lsq` packages on npm / crates.io.
