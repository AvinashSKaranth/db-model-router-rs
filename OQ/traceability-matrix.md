# Traceability matrix: requirement → OQ check → evidence

Status as of the 2026-11-15 execution (Win11, rustc 1.97.1, all database services live),
with the 2026-11-16 conformance re-run (210 tests) for transactions and the non-BRD features. The
legend is in [README.md](README.md). Node test-case mapping (file level) is in
`tests/traceability.json`:

| Status | Node cases |
|---|---|
| covered | 450 |
| unverified | 425 |
| open | 129 |
| out_of_scope | 367 |
| waived | 3 |

## BRD requirements

| BRD § | Requirement | OQ check | Evidence (test) | Status |
|---|---|---|---|---|
| §5, ADR-0005 | Facade crate, feature-gated adapters | A3.9 | `facade_tests.rs::schema_to_router_end_to_end`; `cargo build --all-features` | Pass |
| §6.2 | Rules compiled once; an unknown rule fails at build | A2.2 | `model_contract.rs::unknown_rule_fails_at_build` | Pass |
| §7, §27 | Adapter contract on 10 engines | OQ-B | `crates/dbmr-adapters/tests/*_tests.rs`, `crates/dbmr/tests/conformance.rs` (22 scenarios x 10 = 220) | **Pass** on all 10 |
| §8, §20.1 | Routes and methods (incl. QUERY, 405) | A1.1–A1.9, A1.13 | `route_tests.rs` | Pass |
| §20.4 | Response shapes (summary, 404 body, list `{data,count}`) | A1.3, A1.5 | `route_tests.rs` | Pass |
| §20.5 | CSV/XML output | A1.10 | `route_tests.rs::csv_and_xml`, `formatter::tests::node_parity` | Pass |
| §20.6 | Row scope on every path (replaces payload overrides, ADR-0017) | A1.14, S4 | `scope_tests.rs::*`, OQ-E E2 | Pass |
| §20.6, D8 | Scope fails closed (401) | A1.17 | `scope_tests.rs::require_scope_fails_closed_and_system_opts_out` | Pass |
| §20.7 | Error mapping, 5xx scrubbing | A1.4, A1.12, S10 | `route_tests.rs::validation_is_422_with_niv_message`, `malformed_json_is_400`, `server_errors_are_scrubbed` | Pass |
| §21 | Model operations | A2.1–A2.7, A2.12 | `model_contract.rs` | Pass |
| §22 | Filters, sort, search, paging | A1.7, A3.1–A3.5 | `filter_tests.rs`, `query.rs`, `compiler.rs` tests | Pass |
| §23 | Hooks with user context | A2.8, S9 | `model_contract.rs::hooks_see_user_and_can_rewrite`, `mass_delete_guard_runs_after_before_delete_hook` | Pass |
| §24 | Field encryption, keyring, tamper detection | A2.9, S3 | `model_contract.rs::encrypted_columns_*`, `facade_tests.rs::schema_encrypted_column_requires_key`, `encryption.rs` tests | Pass |
| §25 | Kafka events; broker failure never aborts writes | A2.11 | `model_contract.rs::events_fire_without_transaction`; `kafka_tests.rs::test_disabled_producer_zero_overhead`, `test_transaction_buffer_mechanics` | Pass, incl. live Kafka |
| §26 | Transactions, savepoints, events after commit | A2.10 | `model_contract.rs::transaction_commit_rollback_and_savepoints`; `sqlite_tests.rs::test_sqlite_transactions`; `conformance.rs` `transactions` | Pass (every SQL adapter live: SQLite, PostgreSQL, CockroachDB, MySQL, MariaDB, MSSQL, Oracle); MongoDB, Redis, DynamoDB return `NO_TX_MESSAGE` (ADR-0016, asserted live) |
| §28 | Schema parse and validation | A3.8 | `schema_tests.rs`, `proptest_schema.rs` | Pass |
| §31 | JSON utilities, big-int fidelity | A3.6, D1.5 | `json.rs` tests, `route_tests.rs::big_integers_are_lossless` | Pass |
| §14, §32 | Injection, TLS, tenancy, leakage | OQ-C S2–S10 | See OQ-C | Pass (S5, S7 live) |
| §34 D15 | Mass-delete guard | A2.5, S9 | `model_contract.rs::soft_delete_hides_rows_and_mass_delete_is_guarded` | Pass |
| §34 D19 | Soft-delete flag server-managed | OQ-C | `model_contract.rs::soft_delete_flag_is_not_client_writable` | Pass |
| §36 | Quality gates (fmt, clippy, deny, audit, MSRV, docs) | OQ-C, CI | `.github/workflows/ci.yml` | fmt/clippy Pass locally; other gates CI-only |
| §37 | Null / undefined contract | A1.11, A2.6 | `route_tests.rs::literal_null_string_is_stored_verbatim`, `model_contract.rs::null_semantics` | Pass |
| §12.1, §33 | > 95% Node test-case traceability | — | `cargo run -p xtask -- traceability-check` | **Not met**: covered/in-scope is 44.8% (file level). The gate now checks honesty (no invented mappings), not the percentage |
| §15, §38 | Performance budgets vs sqlx/Diesel/SeaORM/Node | OQ-D D2 | `crates/dbmr-bench` | Executed 2026-11-15, **inconclusive** (host-bound, see OQ-D); contenders missing |
| §9–§11, §13, §29, §30 | CLI, db-manager, codegen, migrations | — | — | Out of scope (ADR-0013) |
| §11 (Actix) | Optional Actix integration | — | — | Deviation (ADR-0014) |

## Non-BRD features (deviations D39–D43)

| Feature | OQ check | Evidence (test) | Status |
|---|---|---|---|
| Optimistic locking, `ETag` / `If-Match`, 409 / 428 | A1.19, OQ-B | `conformance.rs` `optimistic_lock`; `router_features_tests.rs::etag_and_if_match_drive_optimistic_locking`, `bulk_put_carries_versions_per_row` | Pass on all 10 |
| Hidden columns | A1.18, OQ-B | `conformance.rs` `hidden_columns`; `router_features_tests.rs::hidden_columns_are_not_writable_or_filterable` | Pass on all 10 |
| App-side timestamps | OQ-B | `conformance.rs` `auto_timestamps` | Pass on all 10 |
| Relations / `include` | A1.20, OQ-B | `conformance.rs` `include_relations`; `router_features_tests.rs::include_works_on_list_query_and_item` | Pass on all 10 |
| Same value types, record shape and unique-key handling on every adapter | OQ-B `type_contract` | `crates/dbmr/tests/conformance.rs::<db>::type_contract` | Pass on all 10 |
| Relations stay inside the scope (reads and writes) | A1.23 | `router_features_tests.rs::includes_and_foreign_keys_stay_inside_the_scope` | Pass |
| Typed models | OQ-B | `conformance.rs` `typed_roundtrip` | Pass on all 10 |
| `RouterOptions` (405 / export off) | A1.15, A1.16 | `router_features_tests.rs::read_only_router_answers_405_for_writes`, `single_and_bulk_flags_are_independent_and_export_can_be_off` | Pass |
| OpenAPI 3.1, health route | A1.21, A1.22 | `router_features_tests.rs::openapi_document_follows_models_and_options`, `health_reports_database_state_without_leaking_errors` | Pass |

## Non-BRD requirement: LLM usability

| Requirement | OQ check | Evidence | Status |
|---|---|---|---|
| Docs let an LLM build correct apps | OQ-E E1–E5 | `OQ/oq-e/run_oq_e.py` | Harness validated (reference 5/5, negative control red); agent run Not executed |
| A compiled minimal example exists | OQ-E context | `cargo run -p dbmr --example quickstart` (CI job `test`) | Pass |
| Machine-readable entry points | OQ-E context | `llms.txt`, `AGENTS.md` | Present |
