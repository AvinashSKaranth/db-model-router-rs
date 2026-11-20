# ADR-0013: Library scope is model + router only; CLI, db-manager and migrations extracted

**Status:** Accepted (2026-11-14, user decision)
**Supersedes:** ADR-0002, ADR-0003, ADR-0004. Amends ADR-0007 (CLI exit codes no longer apply).
**Referenced by:** BRD §9, §29, §30, §31; task Phase 7, Phase 8

## Context
The workspace held `dbmr-cli` (init, doctor, diff, types, migrate, encrypt), `dbmr-dbmanager`
(web UI) and a migration runner. The owner is moving that tooling to a separate project.

## Decision
1. This repository ships only: `dbmr-core` (model, rules, hooks, encryption, transactions),
   `dbmr-adapters`, `dbmr-http` (Axum router), `dbmr-schema` (schema parse → model meta),
   `dbmr-kafka` (optional event sink), and the `dbmr` facade.
2. No binary, no migration runner. Applications own DDL. The CLI-based
   `demo/` app was removed with the CLI.
3. `dbmr-schema` stays: it is the input format for `dbmr::load_models_from_schema`.
4. Node tests for CLI/db-manager/codegen/migrations are `out_of_scope` in
   `tests/traceability.json`.

## Consequences
+ Smaller API surface; one job for the crate.
+ No `tokio` main / process exit-code contract to maintain here.
- The external CLI must depend on `dbmr-schema` for shared schema types; keep that crate
  semver-stable.
