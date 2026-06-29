#!/usr/bin/env python3
"""Mechanical end-to-end smoke test for lsq's MCP server against real projects.

lsq is an MCP stdio server (no CLI). This harness builds it, clones a Rust project
(ripgrep -> rust-analyzer) and a TypeScript project (ky -> tsgo) if missing, spawns one
`lsq` server rooted at each repo, drives it over the MCP protocol (initialize -> tools/call),
and asserts on the JSON each tool returns. Exits non-zero if any check fails.
Run: `python3 scripts/e2e.py`.

Two assertions guard regressions found while testing at scale:
  * ripgrep `refs` must return many cross-file results (the ContentModified retry must wait
    out rust-analyzer's cold index).
  * ky `refs` must be FAST (<5s) — tsgo is a pull server and must not wait 20s for a
    publishDiagnostics that never comes.
"""
import json
import os
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LSQ = os.path.join(ROOT, "target", "debug", "lsq")
WORK = "/tmp/lsq-e2e"
ENV = dict(os.environ, PATH=os.path.join(ROOT, "node_modules/.bin") + ":" + os.environ.get("PATH", ""))
REPOS = {
    "ripgrep": "https://github.com/BurntSushi/ripgrep",
    "ky": "https://github.com/sindresorhus/ky",
}

_passed = 0
_failed = 0


def check(name, ok, detail=""):
    global _passed, _failed
    if ok:
        _passed += 1
    else:
        _failed += 1
    mark = "\033[32mPASS\033[0m" if ok else "\033[31mFAIL\033[0m"
    print(f"  [{mark}] {name}" + (f" — {detail}" if detail else ""))


def ensure_repo(name):
    path = os.path.join(WORK, name)
    if not os.path.isdir(path):
        os.makedirs(WORK, exist_ok=True)
        print(f"  cloning {name}...")
        subprocess.run(
            ["git", "clone", "--depth", "1", REPOS[name], path],
            check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
    return path


class Mcp:
    """A minimal MCP stdio client driving one `lsq` server rooted at `cwd`."""

    def __init__(self, cwd):
        self.p = subprocess.Popen(
            [LSQ], cwd=cwd, env=ENV,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        )
        self._id = 0
        self._req("initialize", {
            "protocolVersion": "2024-11-05", "capabilities": {},
            "clientInfo": {"name": "e2e", "version": "0"},
        })
        self._notify("notifications/initialized", {})

    def _send(self, obj):
        self.p.stdin.write((json.dumps(obj) + "\n").encode())
        self.p.stdin.flush()

    def _notify(self, method, params):
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def _req(self, method, params):
        self._id += 1
        rid = self._id
        self._send({"jsonrpc": "2.0", "id": rid, "method": method, "params": params})
        while True:  # skip any interleaved notifications; match our id
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError("server closed the connection")
            msg = json.loads(line)
            if msg.get("id") == rid:
                return msg

    def call(self, tool, **args):
        """Invoke a tool; return (elapsed_seconds, parsed_json | None)."""
        t = time.time()
        resp = self._req("tools/call", {"name": tool, "arguments": args})
        el = time.time() - t
        try:
            text = resp["result"]["content"][0]["text"]
            return el, json.loads(text)
        except (KeyError, IndexError, json.JSONDecodeError, TypeError):
            return el, None

    def close(self):
        self.p.kill()


def main():
    print("building lsq...")
    subprocess.run(["cargo", "build", "-q"], cwd=ROOT, check=True)
    rg = ensure_repo("ripgrep")
    ky = ensure_repo("ky")

    # ---- ripgrep: rust-analyzer, at scale ----
    print("\nripgrep (rust-analyzer):")
    srv = Mcp(rg)
    try:
        _, j = srv.call("symbols", file="crates/matcher/src/lib.rs")  # first call warms the session
        check("symbols returns many", bool(j) and j.get("count", 0) >= 20,
              f"count={j.get('count') if j else None}")

        el, j = srv.call("refs", selector="crates/matcher/src/lib.rs@Match")
        files = len({l["file"] for l in (j or {}).get("locations", [])})
        check("refs Match: nonempty + cross-file (ContentModified retry works)",
              bool(j) and j.get("count", 0) >= 20 and files > 1,
              f"count={j.get('count') if j else None} files={files} {el:.1f}s")

        el2, _ = srv.call("refs", selector="crates/matcher/src/lib.rs@Match")
        check("warm refs fast (<1s)", el2 < 1.0, f"{el2:.3f}s")

        _, j = srv.call("rename", selector="crates/matcher/src/lib.rs@Match", new_name="Mtch")
        check("rename dry-run produces cross-file edits",
              bool(j) and j.get("edits", 0) >= 20 and j.get("files_changed", 0) > 1,
              f"edits={j.get('edits') if j else None} files={j.get('files_changed') if j else None}")

        _, j = srv.call("impl", selector="crates/matcher/src/lib.rs@Matcher")
        files = len({l["file"] for l in (j or {}).get("locations", [])})
        check("impl: trait implementations across crates",
              bool(j) and j.get("count", 0) >= 2 and files > 1,
              f"count={j.get('count') if j else None} files={files}")

        # type-def resolves the TYPE of a value (a type-name selector correctly yields 0)
        _, j = srv.call("type-def", selector="crates/searcher/src/searcher/mod.rs:1054:13")
        check("type-def: resolves a value's type",
              bool(j) and j.get("count", 0) >= 1,
              f"count={j.get('count') if j else None}")

        _, j = srv.call("callers", selector="crates/matcher/src/lib.rs@new")
        check("callers: incoming calls of a function",
              bool(j) and j.get("count", 0) >= 3,
              f"count={j.get('count') if j else None}")

        # fix: inject an unused import, assert a quick-fix removes it (dry-run), then restore.
        fixfile = os.path.join(rg, "crates/matcher/src/lib.rs")
        with open(fixfile) as fh:
            original = fh.read()
        try:
            with open(fixfile, "w") as fh:
                fh.write("use std::collections::BTreeMap;\n" + original)
            _, j = srv.call("fix", selector="crates/matcher/src/lib.rs:1:25")
            diff = (j or {}).get("diff", "")
            check("fix: offers a quick-fix that removes the unused import",
                  bool(j) and j.get("count", 0) >= 1 and "-use std::collections::BTreeMap;" in diff,
                  f"count={j.get('count') if j else None} title={(j or {}).get('title')!r}")
        finally:
            with open(fixfile, "w") as fh:
                fh.write(original)

        # check (dirty-buffer): a speculative edit's diagnostics surface, disk stays untouched.
        chkfile = os.path.join(rg, "crates/matcher/src/lib.rs")
        before = open(chkfile).read()
        _, j = srv.call("check", file="crates/matcher/src/lib.rs",
                        content=before + '\nfn __lsq_check() { let _x: u32 = "no"; }\n')
        codes = [str(d.get("code")) for d in (j or {}).get("diagnostics", [])]
        check("check: speculative edit surfaces the introduced type error (E0308)",
              bool(j) and "E0308" in codes, f"codes={codes[:5]}")
        check("check: leaves the file on disk untouched", open(chkfile).read() == before)
    finally:
        srv.close()

    # ---- ky: tsgo, the 20s-waste regression guard ----
    print("\nky (tsgo):")
    srv = Mcp(ky)
    try:
        el, j = srv.call("refs", selector="source/core/Ky.ts@Ky")  # first call = cold warmup + query
        check("cold refs Ky: nonempty", bool(j) and j.get("count", 0) >= 2,
              f"count={j.get('count') if j else None}")
        check("cold refs Ky: FAST (<5s, not 20s)", el < 5.0, f"cold={el:.2f}s")

        el, j = srv.call("diagnostics", file="source/index.ts")
        check("diagnostics returns JSON, fast", j is not None and el < 5.0, f"{el:.2f}s")

        _, j = srv.call("type", selector="source/core/Ky.ts@Ky")
        check("type returns hover info", bool(j and j.get("info")),
              repr((j or {}).get("info", ""))[:40])

        chkfile = os.path.join(ky, "source/index.ts")
        before = open(chkfile).read()
        _, j = srv.call("check", file="source/index.ts",
                        content=before + '\nconst __lsqChk: number = "no";\n')
        codes = [d.get("code") for d in (j or {}).get("diagnostics", [])]
        check("check (tsgo): speculative edit surfaces the type error (2322)",
              bool(j) and 2322 in codes, f"codes={codes[:5]}")
        check("check (tsgo): disk untouched", open(chkfile).read() == before)
    finally:
        srv.close()

    print(f"\n{_passed} passed, {_failed} failed")
    sys.exit(1 if _failed else 0)


if __name__ == "__main__":
    main()
