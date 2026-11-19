# OQ-E: LLM ease of use

**Objective:** measure whether an LLM, given only this library's documentation, can write
correct applications on `dbmr` without reading the source. "Correct" means a hidden
acceptance test passes. Compiling is not enough.

**Why:** the library is meant to be used by LLM-driven app generation. The documentation
surface (`AGENTS.md`, `README.md`, `crates/dbmr/examples/quickstart.rs`, `llms.txt`)
is part of the product, and this protocol qualifies it.

## Protocol

| Element | Definition |
|---|---|
| Subject | One LLM agent configuration (model + tool settings). Record the exact model ID |
| Context given (arm **D**, docs) | `AGENTS.md`, `README.md`, `crates/dbmr/examples/quickstart.rs` (harness default) |
| Control arm **R** (README only) | `--context README.md`. Measures what `AGENTS.md` and the example add |
| Withheld | The source tree, the tests and the reference solutions. The agent must not have repository access |
| Output | The complete `src/lib.rs` of a crate `app` with fixed dependencies (dbmr/SQLite, axum 0.8, tokio, serde_json) |
| Repair loop | Up to 3 attempts. After a failure, the agent receives the tail of the `cargo test` output |
| Runs | 3 independent runs per task per arm (LLM output is nondeterministic) |
| Oracle | The hidden acceptance test `oq-e/acceptance/E*.rs`. The harness copies it in; the agent never sees it |

## Tasks

| ID | Task | What it probes (pitfall from `AGENTS.md`) |
|---|---|---|
| [E1](oq-e/tasks/E1.md) | Book catalog CRUD | Single insert at `POST /x/0` or `POST /x` (#3); filter prefixes (#12); 422 validation; own DDL (#1) |
| [E2](oq-e/tasks/E2.md) | Multi-tenant notes | Auth middleware inserting `Scope` (#5, #15). A tenant leak fails the test |
| [E3](oq-e/tasks/E3.md) | Authors with nested books | Nested router with a path-parameter scope `route_layer` (#6); cross-parent 404 |
| [E4](oq-e/tasks/E4.md) | Atomic transfer | `Transaction` + `with_tx` (#8); typed `AppError` status; rollback |
| [E5](oq-e/tasks/E5.md) | Soft delete, search, CSV | `safe_delete` (#9), `search_columns`, `output_content_type=csv`, `in:` rule |

## Scoring

Per task run:
- **10** points if green on attempt 1, **7** on attempt 2, **4** on attempt 3, **0** otherwise.

Reported per arm:
- `score_pct` (mean of points / 10);
- green runs / total runs;
- first-attempt compile rate;
- the median number of attempts to green.

**Acceptance criteria (arm D):**
- `score_pct` ≥ **80**;
- every task green in at least **2 of 3** runs;
- first-attempt compile rate ≥ **0.6**;
- arm D beats arm R by ≥ 10 points. If it does not, the extra docs add nothing and need
  rework.

**Failure triage:** classify each red attempt from its `tail`:

| Class | Meaning |
|---|---|
| (a) API hallucination | A method or type that does not exist |
| (b) Contract misunderstanding | Compiles, but a test assertion fails |
| (c) Rust/axum error unrelated to dbmr | — |

A recurring class (a) or (b) error is a documentation defect: fix `AGENTS.md` or
`README.md`, then re-run.

## Harness

`oq-e/run_oq_e.py`:
- builds the prompt (context docs + task, plus cargo feedback on retries);
- calls the agent through `--agent-cmd` (`{prompt}` / `{out}` placeholders);
- compiles in a fresh crate with a shared target directory;
- runs the hidden test;
- writes `oq-e/results.json`.

```bash
# 1. Harness self-test: must be 5/5 green
PYTHONUTF8=1 python OQ/oq-e/run_oq_e.py --reference
# 2. Arm D
PYTHONUTF8=1 python OQ/oq-e/run_oq_e.py --runs 3 --out OQ/oq-e/results-D.json \
  --agent-cmd 'claude -p "$(cat {prompt})" --model <model-id> > {out}'
# 3. Arm R
PYTHONUTF8=1 python OQ/oq-e/run_oq_e.py --runs 3 --context README.md --out OQ/oq-e/results-R.json \
  --agent-cmd 'claude -p "$(cat {prompt})" --model <model-id> > {out}'
```

Run the agent with no tools or file access (plain completion), so the docs are its only
source.

**Harness validation, executed 2026-11-14:**
- `--reference` scored 5/5 green, 100%. This shows the tasks are solvable with the
  documented API and that the oracle accepts a correct solution.
- Negative control: E2's reference with the tenant override removed went **red**, with
  `client tenant_id must be overridden`. This shows the oracle catches the security-relevant
  mistake it targets.

## Execution log

| Date | Arm | Model | score_pct | green/runs | 1st-compile | Result |
|---|---|---|---|---|---|---|
| 2026-11-14 | reference | — | 100.0 | 5/5 | 1.0 | Harness valid |
| — | D | — | — | — | — | **Not executed** |
| — | R | — | — | — | — | **Not executed** |
