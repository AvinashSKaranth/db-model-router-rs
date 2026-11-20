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
DEFAULT_CONTEXT = ["skills/dbmr/SKILL.md", "crates/dbmr/examples/quickstart.rs",
                   "skills/dbmr/references/http-api.md"]
POINTS = {1: 10, 2: 7, 3: 4}

CARGO_TOML = """[package]
name = "app"
version = "0.0.0"
edition = "2021"

[workspace]

[dependencies]
{database_dependency}
axum = "0.8"
tokio = {{ version = "1", features = ["full"] }}
serde_json = "1"

[dev-dependencies]
tower = {{ version = "0.5", features = ["util"] }}
"""


def prompt_for(task: Path, context: list[str], feedback: str | None, backend: str) -> str:
    database = "dbmr (with SQLite)" if backend == "dbmr" else "SQLx (with SQLite)"
    parts = ["You are writing `src/lib.rs` for a Rust crate named `app`. Dependencies available: "
             f"{database}, axum 0.8, tokio, serde_json. Output ONLY the contents of src/lib.rs, "
             "no markdown fences, no explanation.\n"]
    for c in context:
        parts.append(f"\n===== {c} =====\n{(REPO / c).read_text(encoding='utf-8')}")
    specification = task.read_text(encoding="utf-8")
    if backend == "sqlx":
        specification = (specification
                         .replace("`dbmr` (SQLite)", "Axum and SQLx (SQLite)")
                         .replace("std::sync::Arc<dbmr::adapters::SqliteAdapter>", "sqlx::SqlitePool")
                         .replace("dbmr::Result<axum::Router>", "Result<axum::Router, sqlx::Error>")
                         .replace("dbmr::Result<()>", "Result<(), axum::http::StatusCode>")
                         .replace("an `AppError` with HTTP status 422", "HTTP status 422")
                         .replace("The standard dbmr HTTP contract applies:", "Implement this HTTP contract:"))
    parts.append(f"\n===== TASK =====\n{specification}")
    if feedback:
        parts.append(f"\n===== YOUR PREVIOUS lib.rs FAILED. cargo output (tail) =====\n{feedback}\nFix it.")
    return "\n".join(parts)


def strip_fences(src: str) -> str:
    m = re.search(r"```(?:rust)?\n(.*?)```", src, re.S)
    return m.group(1) if m else src


def evaluate(task_id: str, lib_rs: str, work: Path, target: Path, backend: str) -> tuple[bool, str]:
    crate = work / task_id
    if crate.resolve().parent != work:
        raise ValueError(f"Test crate is outside the work directory: {crate}")
    shutil.rmtree(crate, ignore_errors=True)
    (crate / "src").mkdir(parents=True)
    (crate / "tests" / "common").mkdir(parents=True)
    dependency = (f'dbmr = {{ path = "{(REPO / "crates/dbmr").as_posix()}" }}'
                  if backend == "dbmr" else
                  'sqlx = { version = "0.8", default-features = false, features = ["sqlite", "runtime-tokio"] }')
    (crate / "Cargo.toml").write_text(CARGO_TOML.format(database_dependency=dependency), encoding="utf-8")
    (crate / "src/lib.rs").write_text(lib_rs, encoding="utf-8")
    shutil.copy(HERE / "acceptance/common.rs", crate / "tests/common/mod.rs")
    acceptance = (HERE / f"acceptance/{task_id}.rs").read_text(encoding="utf-8")
    if backend == "sqlx":
        acceptance = (acceptance
                      .replace("use dbmr::adapters::SqliteAdapter;", "use sqlx::SqlitePool;")
                      .replace("SqliteAdapter::connect(", "SqlitePool::connect(")
                      .replace("err.status_code()", "err.as_u16()")
                      .replace("use dbmr::DatabaseAdapter;", "use sqlx::Row;")
                      .replace('db.query("SELECT is_deleted FROM tasks WHERE title = \'lunch\'", &[])',
                               'sqlx::query("SELECT is_deleted FROM tasks WHERE title = \'lunch\'").fetch_all(&db)')
                      .replace('raw[0]["is_deleted"], dbmr::DbValue::Int(1)', 'raw[0].get::<i64, _>("is_deleted"), 1'))
    (crate / "tests/acceptance.rs").write_text(acceptance, encoding="utf-8")
    env = {**os.environ, "CARGO_TARGET_DIR": str(target), "RUSTFLAGS": ""}
    p = subprocess.run(["cargo", "test", "--quiet", "--test", "acceptance"], cwd=crate, env=env,
                       capture_output=True, text=True, encoding="utf-8", errors="replace")
    out = (p.stdout + p.stderr)[-6000:]
    return p.returncode == 0, out


def agent_response(raw: str, output_format: str) -> tuple[str, dict | None]:
    if output_format == "text":
        return strip_fences(raw), None
    payload = json.loads(raw)
    if payload.get("is_error"):
        raise ValueError(payload.get("result", "The agent returned an error"))
    usage = payload["usage"]
    tokens = {
        "input_tokens": sum(usage.get(key, 0) for key in
                            ("input_tokens", "cache_creation_input_tokens", "cache_read_input_tokens")),
        "output_tokens": usage["output_tokens"],
    }
    return strip_fences(payload["result"]), tokens


def main() -> None:
    ap = argparse.ArgumentParser()
    mode = ap.add_mutually_exclusive_group(required=True)
    mode.add_argument("--reference", action="store_true")
    mode.add_argument("--candidate-dir")
    mode.add_argument("--agent-cmd")
    ap.add_argument("--agent-stdin", action="store_true",
                    help="send the prompt to stdin and collect stdout, without redirection in the command")
    ap.add_argument("--agent-output", choices=["text", "claude-json"], default="text")
    ap.add_argument("--backend", choices=["dbmr", "sqlx"], default="dbmr")
    ap.add_argument("--tasks", help="comma list, default all")
    ap.add_argument("--runs", type=int, default=1, help="repeat each task (agent mode)")
    ap.add_argument("--attempts", type=int, default=3)
    ap.add_argument("--context", action="append", help="doc files given to the agent (repeatable)")
    ap.add_argument("--work", default=os.path.join(tempfile.gettempdir(), "dbmr-oq-e"))
    ap.add_argument("--out", default=str(HERE / "results.json"))
    a = ap.parse_args()
    if a.reference and a.backend != "dbmr":
        ap.error("reference solutions use dbmr")

    tasks = sorted((HERE / "tasks").glob("E*.md"))
    if a.tasks:
        tasks = [t for t in tasks if t.stem in a.tasks.split(",")]
    context = a.context or (DEFAULT_CONTEXT if a.backend == "dbmr" else [])
    work = Path(a.work).resolve()
    target = work / "target"
    work.mkdir(parents=True, exist_ok=True)
    attempts = 1 if not a.agent_cmd else a.attempts
    runs = a.runs if a.agent_cmd else 1

    results = []
    for task in tasks:
        for run in range(1, runs + 1):
            feedback, rec = None, {"task": task.stem, "run": run, "attempts": [], "green": False}
            for attempt in range(1, attempts + 1):
                start = time.monotonic()
                tokens, agent_error = None, None
                if a.reference:
                    lib = (HERE / f"reference/{task.stem}.rs").read_text(encoding="utf-8")
                elif a.candidate_dir:
                    lib = Path(a.candidate_dir, f"{task.stem}.rs").read_text(encoding="utf-8")
                else:
                    pf, of = work / f"{task.stem}.prompt.txt", work / f"{task.stem}.out.rs"
                    prompt = prompt_for(task, context, feedback, a.backend)
                    pf.write_text(prompt, encoding="utf-8")
                    of.unlink(missing_ok=True)
                    command = a.agent_cmd.replace("{prompt}", str(pf)).replace("{out}", str(of))
                    if a.agent_stdin:
                        with of.open("w", encoding="utf-8") as output:
                            process = subprocess.run(command, cwd=work, shell=True, check=False,
                                                     input=prompt, encoding="utf-8", stdout=output)
                    else:
                        process = subprocess.run(command, cwd=work, shell=True, check=False)
                    try:
                        lib, tokens = agent_response(of.read_text(encoding="utf-8"), a.agent_output)
                        if process.returncode:
                            agent_error = f"Agent command exited {process.returncode}"
                    except json.JSONDecodeError:
                        lib = ""
                        agent_error = "Agent returned no JSON. Check the agent's authentication and configuration."
                    except (OSError, ValueError, KeyError, TypeError) as error:
                        lib, agent_error = "", f"Invalid agent response: {error}"
                ok, out = ((False, agent_error) if agent_error else
                           evaluate(task.stem, lib, work, target, a.backend))
                compiled = not agent_error and "error[" not in out and "could not compile" not in out
                record = {"attempt": attempt, "compiled": compiled, "green": ok,
                          "seconds": round(time.monotonic() - start, 2),
                          "tail": "" if ok else out[-1500:]}
                if tokens is not None:
                    record.update(tokens)
                rec["attempts"].append(record)
                if agent_error:
                    break
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
        "backend": a.backend,
        "score_pct": round(100 * total / (10 * len(results)), 1) if results else 0,
        "green": sum(r["green"] for r in results), "runs": len(results),
        "first_attempt_compile_rate": round(first_compile / len(results), 2) if results else 0,
    }
    attempts_recorded = [attempt for run in results for attempt in run["attempts"]]
    summary["elapsed_seconds"] = round(sum(attempt["seconds"] for attempt in attempts_recorded), 2)
    if attempts_recorded and all("input_tokens" in attempt for attempt in attempts_recorded):
        for key in ("input_tokens", "output_tokens"):
            summary[key] = sum(attempt[key] for attempt in attempts_recorded)
        if summary["green"]:
            summary["tokens_per_passing_app"] = round(
                (summary["input_tokens"] + summary["output_tokens"]) / summary["green"])
    Path(a.out).write_text(json.dumps({"summary": summary, "results": results}, indent=1), encoding="utf-8")
    print(json.dumps(summary, indent=1))
    sys.exit(0 if summary["green"] == summary["runs"] or not a.reference else 1)


if __name__ == "__main__":
    main()
