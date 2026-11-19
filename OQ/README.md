# Operational Qualification: dbmr

This plan proves that `dbmr` (model + router library) does what `.spec/brd.md` requires and
that an LLM can build working applications with it. Every claim points at an executable
check. A requirement without a passing check is not claimed.

| Item | Value |
|---|---|
| System under test | `dbmr` 0.1.0 workspace (`crates/*`), library scope per ADR-0013 |
| Last execution | 2026-11-14, Windows 11 Pro, rustc 1.97.1. No database services (Docker off) |
| Requirements source | `.spec/brd.md` (§ numbers below), `docs/deviations.md`, `docs/adr/` |
| Node reference | `../db-model-router` (`src/commons/*.js`, `test/**`) |

## Protocols

| ID | Protocol | Question it answers | Status 2026-11-14 |
|---|---|---|---|
| [OQ-A](OQ-A-functional-parity.md) | Functional parity | Do the HTTP and model contracts match BRD §20–§26, §31 and §37? | **Pass** (SQLite) |
| [OQ-B](OQ-B-adapter-conformance.md) | Adapter conformance | Does each database adapter behave the same? | **Pass** on all 10 adapters (2026-11-16: `conformance.rs` 22 scenarios x 10 = 220) |
| [OQ-C](OQ-C-security.md) | Security | Are the review findings S1–S10 and BRD §14/§32 closed? | **Pass** for S2, S3, S4, S6, S8, S9, S10. S5 and S7 pass code review; their live test was not executed |
| [OQ-D](OQ-D-robustness-performance.md) | Robustness and performance | Bounded inputs, error mapping, BRD §15/§38 budgets | Robustness **Pass**; performance **Not executed** (needs PostgreSQL) |
| [OQ-E](OQ-E-llm-ease-of-use.md) | LLM ease of use | Can an LLM build correct apps from the docs alone? | Harness validated (reference 5/5, negative control caught); **agent run not executed** |

Requirement → test → result: [traceability-matrix.md](traceability-matrix.md).
Node test-case mapping: `tests/traceability.json` (file level; rebuild with `scripts/build_traceability.py`).

## Status legend

- **Pass**: the check ran and passed in the stated environment.
- **Fail**: the check ran and failed.
- **Not executed**: the check exists but needs infrastructure that was not available. This
  is never counted as a pass.
- **Review only**: the code was inspected and fixed, but no automated check exists yet.

## How to execute

```bash
# OQ-A, OQ-C (offline part), OQ-D robustness: SQLite + in-memory
cargo test --workspace --all-features
cargo run -p dbmr --example quickstart
cargo run -p xtask -- traceability-check

# OQ-B, OQ-C S5/S7: live databases
docker compose up -d --wait
cargo test --workspace --all-features -- --ignored --test-threads=1

# OQ-D performance (PostgreSQL)
DATABASE_URL=postgres://postgres:password@127.0.0.1:5432/test_db cargo run -p dbmr-bench --release

# OQ-E: harness self-test, then an agent run
PYTHONUTF8=1 python OQ/oq-e/run_oq_e.py --reference
PYTHONUTF8=1 python OQ/oq-e/run_oq_e.py --agent-cmd "<your agent>" --runs 3
```

CI (`.github/workflows/ci.yml`) runs each step as a job. Each job name gives the OQ ID it
provides evidence for.

## Recording a run

For each execution, append a row to the protocol's *Execution log* with:
- date;
- commit (or "uncommitted");
- environment;
- result;
- for any failure, the failing test name and its first assertion message.

Do not edit a past row. Add a new one.

## Sign-off

| Role | Name | Date | Signature |
|---|---|---|---|
| Author | | | |
| Reviewer | | | |
| Approver | | | |
