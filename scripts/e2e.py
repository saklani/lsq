#!/usr/bin/env python3
"""Mechanical end-to-end smoke test for lsq against real projects.

Builds lsq, clones a Rust project (ripgrep -> rust-analyzer) and a TypeScript
project (ky -> tsgo) if missing, runs the verbs, and asserts on the results.
Exits non-zero if any check fails. Run: `python3 scripts/e2e.py`.

Two assertions guard regressions found while testing at scale:
  * ripgrep cold `refs` must return many cross-file results (ContentModified
    retry must wait out rust-analyzer's cold index).
  * ky cold `refs` must be FAST (<5s) — tsgo is a pull server and must not wait
    20s for a publishDiagnostics that never comes.
"""
import json
import os
import re
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


def reset_daemon():
    """Kill any running daemon and clear sockets so the next call is truly cold."""
    subprocess.run(["pkill", "-f", "lsq __daemon"], stderr=subprocess.DEVNULL)
    time.sleep(0.4)
    tmp = os.environ.get("TMPDIR", "/tmp")
    for f in os.listdir(tmp):
        if f.startswith("lsq-") and f.endswith(".sock"):
            try:
                os.remove(os.path.join(tmp, f))
            except OSError:
                pass


def run(cwd, *args):
    t = time.time()
    p = subprocess.run([LSQ, *args], cwd=cwd, env=ENV,
                       stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    return time.time() - t, p.stdout, p.stderr


def run_json(cwd, *args):
    el, out, err = run(cwd, *args)
    try:
        return el, json.loads(out)
    except json.JSONDecodeError:
        return el, None


def main():
    print("building lsq...")
    subprocess.run(["cargo", "build", "-q"], cwd=ROOT, check=True)
    rg = ensure_repo("ripgrep")
    ky = ensure_repo("ky")

    # ---- ripgrep: rust-analyzer, at scale ----
    print("\nripgrep (rust-analyzer):")
    reset_daemon()
    _, j = run_json(rg, "symbols", "crates/matcher/src/lib.rs")
    check("symbols returns many", bool(j) and j.get("count", 0) >= 20,
          f"count={j.get('count') if j else None}")

    el, j = run_json(rg, "refs", "crates/matcher/src/lib.rs@Match")
    files = len({l["file"] for l in (j or {}).get("locations", [])})
    check("cold refs Match: nonempty + cross-file (ContentModified retry works)",
          bool(j) and j.get("count", 0) >= 20 and files > 1,
          f"count={j.get('count') if j else None} files={files} cold={el:.1f}s")

    el2, _ = run_json(rg, "refs", "crates/matcher/src/lib.rs@Match")
    check("warm refs fast (<1s)", el2 < 1.0, f"{el2:.3f}s")

    _, out, err = run(rg, "rename", "crates/matcher/src/lib.rs@Match", "Mtch")
    m = re.search(r"(\d+) edit\(s\) across (\d+) file", err)
    check("rename dry-run produces cross-file edits",
          bool(m) and int(m.group(1)) >= 20 and int(m.group(2)) > 1,
          err.strip().splitlines()[-1] if err.strip() else "(no summary)")

    _, j = run_json(rg, "impl", "crates/matcher/src/lib.rs@Matcher")
    files = len({l["file"] for l in (j or {}).get("locations", [])})
    check("impl: trait implementations across crates",
          bool(j) and j.get("count", 0) >= 2 and files > 1,
          f"count={j.get('count') if j else None} files={files}")

    # type-def resolves the TYPE of a value (a type-name selector correctly yields 0)
    _, j = run_json(rg, "type-def", "crates/searcher/src/searcher/mod.rs:1054:13")
    check("type-def: resolves a value's type",
          bool(j) and j.get("count", 0) >= 1,
          f"count={j.get('count') if j else None}")

    _, j = run_json(rg, "search", "Matcher")
    files = len({s["file"] for s in (j or {}).get("symbols", [])})
    check("search: finds symbols by name across crates",
          bool(j) and j.get("count", 0) >= 5 and files > 1,
          f"count={j.get('count') if j else None} files={files}")

    _, j = run_json(rg, "callers", "crates/matcher/src/lib.rs@new")
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
        # fix prints the diff to stdout and the action list/summary to stderr (like rename).
        _, out, err = run(rg, "fix", "crates/matcher/src/lib.rs:1:25")
        check("fix: offers a quick-fix that removes the unused import",
              "code action(s)" in err and "-use std::collections::BTreeMap;" in out,
              err.strip().splitlines()[-1] if err.strip() else "(no summary)")
    finally:
        with open(fixfile, "w") as fh:
            fh.write(original)

    # ---- ky: tsgo, the 20s-waste regression guard ----
    print("\nky (tsgo):")
    reset_daemon()
    el, j = run_json(ky, "refs", "source/core/Ky.ts@Ky")
    check("cold refs Ky: nonempty", bool(j) and j.get("count", 0) >= 2,
          f"count={j.get('count') if j else None}")
    check("cold refs Ky: FAST (<5s, not 20s)", el < 5.0, f"cold={el:.2f}s")

    el, j = run_json(ky, "diagnostics", "source/index.ts")
    check("diagnostics returns JSON, fast", j is not None and el < 5.0, f"{el:.2f}s")

    _, j = run_json(ky, "type", "source/core/Ky.ts@Ky")
    check("type returns hover info", bool(j and j.get("info")),
          repr((j or {}).get("info", ""))[:40])

    _, j = run_json(ky, "search", "Ky")
    check("search (tsgo): finds symbols by name", bool(j) and j.get("count", 0) >= 2,
          f"count={j.get('count') if j else None}")

    reset_daemon()  # leave no daemon running
    print(f"\n{_passed} passed, {_failed} failed")
    sys.exit(1 if _failed else 0)


if __name__ == "__main__":
    main()
