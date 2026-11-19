#!/usr/bin/env python3
"""OQ-E harness: can an LLM build working apps on dbmr from the docs alone?

For each task in tasks/E*.md:
  1. Build a prompt = context docs + task.
  2. Get a candidate src/lib.rs (from --reference, --candidate-dir, or --agent-cmd).
  3. Compile it in a fresh crate with the hidden acceptance test; on failure, feed the
     cargo output back to the agent (up to --attempts).
  4. Score: 10 / 7 / 4 points when green on attempt 1 / 2 / 3, else 0.

Modes (pick one):
  --reference              use reference/E*.rs (harness self-test; must score 100%)
  --candidate-dir DIR      use DIR/E*.rs (single attempt; outputs produced elsewhere)
  --agent-cmd "CMD"        CMD is run via the shell with {prompt} and {out} placeholders;
                           it must write the candidate lib.rs to {out}. Example:
                           --agent-cmd "claude -p \"$(cat {prompt})\" --output-format text > {out}"

Usage: PYTHONUTF8=1 python OQ/oq-e/run_oq_e.py --reference [--tasks E1,E3] [--runs 3]
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
DEFAULT_CONTEXT = ["AGENTS.md", "README.md", "crates/dbmr/examples/quickstart.rs"]
POINTS = {1: 10, 2: 7, 3: 4}

CARGO_TOML = """[package]
name = "app"
version = "0.0.0"
edition = "2021"

[workspace]

[dependencies]
dbmr = {{ path = "{dbmr}" }}
axum = "0.8"
tokio = {{ version = "1", features = ["full"] }}
serde_json = "1"

[dev-dependencies]
tower = {{ version = "0.5", features = ["util"] }}
"""


def prompt_for(task: Path, context: list[str], feedback: str | None) -> str:
    parts = ["You are writing `src/lib.rs` for a Rust crate named `app`. Dependencies available: "
             "dbmr (with SQLite), axum 0.8, tokio, serde_json. Output ONLY the contents of src/lib.rs, "
             "no markdown fences, no explanation.\n"]
    for c in context:
        parts.append(f"\n===== {c} =====\n{(REPO / c).read_text(encoding='utf-8')}")
    parts.append(f"\n===== TASK =====\n{task.read_text(encoding='utf-8')}")
    if feedback:
        parts.append(f"\n===== YOUR PREVIOUS lib.rs FAILED. cargo output (tail) =====\n{feedback}\nFix it.")
    return "\n".join(parts)


def strip_fences(src: str) -> str:
    m = re.search(r"```(?:rust)?\n(.*?)```", src, re.S)
    return m.group(1) if m else src


def evaluate(task_id: str, lib_rs: str, work: Path, target: Path) -> tuple[bool, str]:
    crate = work / task_id
    shutil.rmtree(crate, ignore_errors=True)
    (crate / "src").mkdir(parents=True)
    (crate / "tests" / "common").mkdir(parents=True)
    (crate / "Cargo.toml").write_text(CARGO_TOML.format(dbmr=(REPO / "crates/dbmr").as_posix()), encoding="utf-8")
    (crate / "src/lib.rs").write_text(lib_rs, encoding="utf-8")
    shutil.copy(HERE / "acceptance/common.rs", crate / "tests/common/mod.rs")
    shutil.copy(HERE / f"acceptance/{task_id}.rs", crate / "tests/acceptance.rs")
    env = {**os.environ, "CARGO_TARGET_DIR": str(target), "RUSTFLAGS": ""}
    p = subprocess.run(["cargo", "test", "--quiet", "--test", "acceptance"], cwd=crate, env=env,
                       capture_output=True, text=True, encoding="utf-8", errors="replace")
    out = (p.stdout + p.stderr)[-6000:]
    return p.returncode == 0, out


def main() -> None:
    ap = argparse.ArgumentParser()
    mode = ap.add_mutually_exclusive_group(required=True)
    mode.add_argument("--reference", action="store_true")
    mode.add_argument("--candidate-dir")
    mode.add_argument("--agent-cmd")
    ap.add_argument("--tasks", help="comma list, default all")
    ap.add_argument("--runs", type=int, default=1, help="repeat each task (agent mode)")
    ap.add_argument("--attempts", type=int, default=3)
    ap.add_argument("--context", action="append", help="doc files given to the agent (repeatable)")
    ap.add_argument("--work", default=os.path.join(tempfile.gettempdir(), "dbmr-oq-e"))
    ap.add_argument("--out", default=str(HERE / "results.json"))
    a = ap.parse_args()

    tasks = sorted((HERE / "tasks").glob("E*.md"))
    if a.tasks:
        tasks = [t for t in tasks if t.stem in a.tasks.split(",")]
    context = a.context or DEFAULT_CONTEXT
    work, target = Path(a.work), Path(a.work) / "target"
    work.mkdir(parents=True, exist_ok=True)
    attempts = 1 if not a.agent_cmd else a.attempts
    runs = a.runs if a.agent_cmd else 1

    results = []
    for task in tasks:
        for run in range(1, runs + 1):
            feedback, rec = None, {"task": task.stem, "run": run, "attempts": [], "green": False}
            for attempt in range(1, attempts + 1):
                if a.reference:
                    lib = (HERE / f"reference/{task.stem}.rs").read_text(encoding="utf-8")
                elif a.candidate_dir:
                    lib = Path(a.candidate_dir, f"{task.stem}.rs").read_text(encoding="utf-8")
                else:
                    pf, of = work / f"{task.stem}.prompt.txt", work / f"{task.stem}.out.rs"
                    pf.write_text(prompt_for(task, context, feedback), encoding="utf-8")
                    t0 = time.time()
                    subprocess.run(a.agent_cmd.format(prompt=pf, out=of), shell=True, check=False)
                    lib = strip_fences(of.read_text(encoding="utf-8", errors="replace")) if of.exists() else ""
                    rec.setdefault("agent_seconds", []).append(round(time.time() - t0, 1))
                ok, out = evaluate(task.stem, lib, work, target)
                compiled = "error[" not in out and "could not compile" not in out
                rec["attempts"].append({"attempt": attempt, "compiled": compiled, "green": ok,
                                        "tail": "" if ok else out[-1500:]})
                if ok:
                    rec["green"], rec["score"] = True, POINTS.get(attempt, 0)
                    break
                feedback = out
            rec.setdefault("score", 0)
            results.append(rec)
            print(f"{task.stem} run{run}: {'GREEN' if rec['green'] else 'RED'} "
                  f"attempts={len(rec['attempts'])} score={rec['score']}", flush=True)

    total = sum(r["score"] for r in results)
    first_compile = sum(r["attempts"][0]["compiled"] for r in results)
    summary = {
        "mode": "reference" if a.reference else "candidate-dir" if a.candidate_dir else "agent",
        "context": context,
        "score_pct": round(100 * total / (10 * len(results)), 1) if results else 0,
        "green": sum(r["green"] for r in results), "runs": len(results),
        "first_attempt_compile_rate": round(first_compile / len(results), 2) if results else 0,
    }
    Path(a.out).write_text(json.dumps({"summary": summary, "results": results}, indent=1), encoding="utf-8")
    print(json.dumps(summary, indent=1))
    sys.exit(0 if summary["green"] == summary["runs"] or not a.reference else 1)


if __name__ == "__main__":
    main()
