# OQ-A: Functional parity (HTTP + model contract)

**Objective:** show that the router and model behave as BRD §20–§26, §31 and §37 specify,
matching Node `db-model-router` except for the documented deviations (`docs/deviations.md`).

**Environment:** SQLite in-memory through `SqliteAdapter`. The tests are offline and
deterministic.

**Acceptance criterion:** every check below passes. Exact status codes and exact response
bodies are asserted, not just "2xx".

## A1: HTTP routing and responses (BRD §20)

File: `crates/dbmr-http/tests/route_tests.rs`

| Check | Test | Expected |
|---|---|---|
| A1.1 Single insert ignores path id and PK | `single_insert_ignores_path_id_and_pk` | 200, stored record, id from DB |
| A1.2 Bulk requires `data` | `bulk_requires_data_array` | 400 `Request body must contain a 'data' array` |
| A1.3 Bulk insert summary | `bulk_insert_summary` | `{rows, message, type:"success"}` |
| A1.4 Validation error | `validation_is_422_with_niv_message` | 422, niv message text |
| A1.5 GET by id, 404 body, `select_columns` | `get_by_id_and_404` | `{"message":"Not Found","type":"danger"}` |
| A1.6 QUERY method (list + item, malformed body ignored) | `query_method_by_id_and_list` | Same results as GET |
| A1.7 List filters, paging, search, `in()`, `<`, clamp | `list_filters_paging_search` | Node coercion; size capped at 200 |
| A1.8 PUT/PATCH/DELETE pre-check | `put_patch_delete_with_precheck` | 404 when not visible |
| A1.9 Bulk update and delete | `bulk_update_and_delete` | Summaries |
| A1.10 CSV/XML byte output | `csv_and_xml`, `formatter::tests::node_parity` | Exact bytes (D6, D22) |
| A1.11 Literal `"null"` stored verbatim | `literal_null_string_is_stored_verbatim` | D8 |
| A1.12 Malformed JSON | `malformed_json_is_400` | 400 (D29) |
| A1.13 Unsupported method | `unknown_method_is_405` | 405 (D30) |
| A1.14 Row scope on reads and writes | `crates/dbmr-http/tests/scope_tests.rs`: `reads_stay_inside_the_scope_whatever_the_filter_shape`, `deletes_stay_inside_the_scope`, `updates_cannot_reach_or_move_rows_across_scopes`, `inserts_get_the_scope_value`, `a_list_scope_reads_all_and_writes_one_of_them` | Rows outside the scope look missing; inserts get the scope (ADR-0017, D46) |

File: `crates/dbmr-http/tests/router_features_tests.rs`

| Check | Test | Expected |
|---|---|---|
| A1.15 `RouterOptions::read_only()` | `read_only_router_answers_405_for_writes` | 405 `{"message":"Method Not Allowed","type":"danger"}` (D30, D41) |
| A1.16 Single/bulk flags independent; export off | `single_and_bulk_flags_are_independent_and_export_can_be_off` | 405 per flag; 400 `Export is disabled` |
| A1.17 Scope fails closed | `crates/dbmr-http/tests/scope_tests.rs::require_scope_fails_closed_and_system_opts_out` | 401 `Missing scope: <col>`; `Context::system()` opts out (D8, D46) |
| A1.18 Hidden columns | `hidden_columns_are_not_writable_or_filterable` | Never returned; 422 `Invalid filter Inputs` (D40) |
| A1.19 Optimistic locking over HTTP | `etag_and_if_match_drive_optimistic_locking`, `bulk_put_carries_versions_per_row` | `ETag`; 409 stale, 428 missing (D39) |
| A1.20 `?include=` | `include_works_on_list_query_and_item` | Relations attached on list, QUERY and item (D43) |
| A1.21 OpenAPI | `openapi_document_follows_models_and_options` | Paths follow `RouterOptions`; hidden columns left out |
| A1.22 Health route | `health_reports_database_state_without_leaking_errors` | 200 `{"status":"ok"}` / 503 `{"status":"unavailable"}` |
| A1.23 Tenant-safe relations | `includes_and_foreign_keys_stay_inside_the_scope` | Other-tenant rows absent from `?include=`; cross-tenant foreign key on insert, bulk insert and patch → 422 `Invalid reference: <fk>` |

## A2: Model operations (BRD §21, §23, §26)

File: `crates/dbmr/tests/model_contract.rs`

| Check | Test |
|---|---|
| A2.1 Insert strips PK, timestamps and unknown keys, then re-fetches | `insert_strips_pk_timestamps_unknown_and_refetches` |
| A2.2 Unknown rule fails at build with the Node message | `unknown_rule_fails_at_build` |
| A2.3 Upsert target is the unique key or PK (D24) | `upsert_uses_unique_key_or_pk` |
| A2.4 Bulk insert is atomic | `bulk_insert_is_atomic` |
| A2.5 Soft delete and mass-delete guard | `soft_delete_hides_rows_and_mass_delete_is_guarded` |
| A2.6 Null semantics, `IS NULL` (§37, D10) | `null_semantics` |
| A2.7 Patch | `patch_requires_pk_and_changes_only_supplied_columns` |
| A2.8 Hooks receive the user and can rewrite (§23) | `hooks_see_user_and_can_rewrite` |
| A2.9 Encryption at rest (§24) | `encrypted_columns_round_trip_and_are_ciphertext_at_rest` |
| A2.10 Transactions, savepoints, events buffered until commit (§26, D11) | `transaction_commit_rollback_and_savepoints` |
| A2.11 Events without a transaction (§25) | `events_fire_without_transaction` |
| A2.12 Paging defaults (30) and clamp (200) | `list_pagination_defaults_and_clamp` |

## A3: Filters, JSON and schema (BRD §22, §28, §31)

| Check | Test |
|---|---|
| A3.1 Filter value precedence (`!in(`, `in(`, `%`, `>=`, ...) | `crates/dbmr-core/tests/filter_tests.rs::test_parse_filter_value_precedence` |
| A3.2 `dataToFilter` decision table | `filter_tests.rs::test_data_to_filter_decision_table` |
| A3.3 Sort normalisation; search expansion | `filter_tests.rs::test_normalize_sort`, `test_apply_search` |
| A3.4 Reserved query parameters (Node coercion) | `dbmr-core` `query.rs::reserved_params_follow_node_coercion` |
| A3.5 WHERE semantics (NULL, IN, LIKE per dialect) | `dbmr-adapters` `compiler.rs::where_semantics` |
| A3.6 JSON utilities, big-int fidelity (§31) | `dbmr-core` `json.rs` tests; `route_tests.rs::big_integers_are_lossless` |
| A3.7 niv rule semantics | `dbmr-core` `rules.rs::niv_semantics` |
| A3.8 Schema parse, validate, meta | `crates/dbmr-schema/tests/schema_tests.rs` (7), `proptest_schema.rs` |
| A3.9 Schema → router end to end | `crates/dbmr/tests/facade_tests.rs::schema_to_router_end_to_end` |
| A3.10 Multi-table app with nested router | `crates/dbmr-http/tests/scope_tests.rs::nested_routes_scope_by_path_parameter`; `crates/dbmr/examples/tour.rs` (`cargo run -p dbmr --example tour`) |

## Out of scope

The following belong to the separate CLI project (ADR-0013) and are not part of this OQ:
- CLI, db-manager, codegen and migrations (BRD §9–§11, §13, §29, §30);
- the matching 367 Node cases in `tests/traceability.json`.

## Known gaps

- 129 Node cases are `open` in `tests/traceability.json`, mainly
  `unit/commons-deep-coverage`, `features.test.js` and the `*-coverage` suites. Mapping is
  file-level, not 1:1 per case.

## Execution log

| Date | Commit | Environment | Result |
|---|---|---|---|
| 2026-11-14 | uncommitted | Win11, rustc 1.97.1, SQLite in-memory | **Pass**. `cargo test --workspace --all-features` passes 73 tests (incl. 5 doctests), 0 fail, 10 ignored (live services). Counts: route_tests 18, model_contract 14, facade 2, filter 4, schema 7+1 proptest, core lib 11, adapters lib 3, http lib 1 + doc, sqlite 4, kafka 2, demo 1 |
| 2026-11-15 | uncommitted | Win11, rustc 1.97.1, Docker Desktop 29.3.1 (images per `docker-compose.yml`) | **Pass**. Offline: 74 passed, 0 failed, 19 ignored (live). With `--include-ignored`: 94 passed, 0 failed. New: `conformance.rs` (1 offline + 9 live) |
