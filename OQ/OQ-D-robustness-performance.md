# OQ-D: Robustness and performance

**Objective:**
- show that hostile or malformed input produces a bounded, typed error and never a panic,
  a hang or unbounded work (BRD §14, §36, §37);
- show that throughput meets the BRD §15 / §38.6 budgets.

## D1: Robustness (offline, SQLite)

| Check | Input | Expected | Test | Status |
|---|---|---|---|---|
| D1.1 Malformed JSON body | `{not json` | 400 typed body | `route_tests.rs::malformed_json_is_400` | **Pass** |
| D1.2 Body size bound | > 16 MiB | 400, body not buffered past the limit | `BODY_LIMIT` (`router.rs`) | Review only. **Gap:** add a test with a 17 MiB body |
| D1.3 Page size bound | `size=1000` | Clamped to 200 | `route_tests.rs::list_filters_paging_search`, `model_contract.rs::list_pagination_defaults_and_clamp` | **Pass** |
| D1.4 Hostile identifiers | `name" OR 1=1 --` filter key; `sort=name"; DROP TABLE users; --` | Treated as one quoted identifier; no rows widened, table intact | `route_tests.rs::hostile_identifiers_cannot_inject` | **Pass** |
| D1.5 Integers beyond 2^53 / i64 | `9007199254740993` (2^53+1) | Lossless round trip | `route_tests.rs::big_integers_are_lossless`, `json.rs::test_big_int_fidelity` | **Pass** |
| D1.6 Invalid filter input | `null` / `""` criteria | 422 `Invalid filter Inputs` | `filter_tests.rs::test_data_to_filter_decision_table` | **Pass**. **Gap:** wrong nesting and unknown operators are not tested |
| D1.7 Empty/degenerate `IN` | `in()` | `1=0` (no rows), never a dropped clause | `compiler.rs::where_semantics` | **Pass** |
| D1.8 Database error | UNIQUE violation | 500, message scrubbed | `route_tests.rs::server_errors_are_scrubbed` | **Pass** |
| D1.9 Bulk failure mid-batch | Duplicate in batch | No partial write (SQLite/PG) | `model_contract.rs::bulk_insert_is_atomic` | **Pass** (SQLite) |
| D1.10 Unknown method | `OPTIONS` | 405 | `route_tests.rs::unknown_method_is_405` | **Pass** |
| D1.11 No panics in library code | — | `unwrap`/`expect` denied by clippy outside tests | `cargo clippy -- -D warnings` | **Pass** |
| D1.12 In-memory SQLite lifetime | Idle pool reaping | Database not dropped | `SqliteAdapter::connect` pins one never-expiring connection for `:memory:` | Review only |

**Not covered (recommended):**
- property/fuzz tests of `parse_filter_value` and `Query::from_json` with arbitrary JSON
  (`proptest` is already a dependency of `dbmr-schema`);
- concurrent-writer tests on PostgreSQL.

## D2: Performance (PostgreSQL)

**Harness:** `crates/dbmr-bench` (`cargo run -p dbmr-bench --release`, `DATABASE_URL` →
`postgres:16`). It has 15 scenarios:
- insert_single, 3 bulk scenarios, read_pk, find_filter, find_sort_limit, list_page;
- update_pk, bulk_update, upsert, delete_pk, tx_multi, tx_rollback, jsonb_roundtrip.

It reports mean, p50, p95, p99 and ops/s.

**Acceptance (BRD §38.6):**

| Budget | Status |
|---|---|
| ≥ 85% of raw sqlx throughput on single-row CRUD and jsonb | **Not executed**. The harness has no raw-sqlx contender yet |
| Within 10% of Diesel / SeaORM medians | **Not executed**. The contenders are not implemented (gap vs ADR-0011) |
| ≥ 2× Node `db-model-router` on read_pk / insert_single | **Not executed** on the current code |
| > 20% regression between releases fails | No baseline for the current code |

`crates/dbmr-bench/reports/results-dbmr-rs.json` (2026-11-08) predates the 2026-11-14
model and adapter rewrite. It is a **historical reference only**, not evidence for this
OQ.

**Run 2026-11-15 (current code): not valid for the budgets.** All 15 scenarios ran
(`reports/results-dbmr-rs-2026-11-15-host-bound.json`), but every scenario was about 3×
slower than on 2026-11-08, including `read_pk`. A control of raw sqlx `SELECT 1` on the
same pool measured **p50 1.72 ms**, higher than dbmr `read_pk` p50 1.48 ms. The host's
Docker network round trip (12 other database containers running) dominates, so the run
measures the host, not the library. It does show dbmr adds no measurable overhead over a
raw round trip. Re-run on a quiet host with only `postgres` up, and compare all
contenders in the same session.

**Procedure:**
1. Run `docker compose up -d --wait postgres`.
2. Run `cargo run -p dbmr-bench --release`.
3. Archive the JSON report with the commit SHA.
4. Compare the numbers with the budgets above.

CI job `bench` only builds the harness.

### Scoped Axum endpoints on SQLite

Run `cargo run --release -p dbmr-bench --bin scoped_router`. The two routers share one
in-memory SQLite connection and the same indexed data. Both receive a trusted tenant
scope. The benchmark checks their success responses for equality before timing 2,000
in-process requests per scenario at concurrency 1 and 8. Output includes throughput
and p50, p95, and p99.

One Windows run at concurrency 8 measured the following p95 latencies:

| Request | Raw Axum + SQLx | dbmr |
|---|---:|---:|
| Item GET | 1.15 ms | 1.61 ms |
| List with total count | 2.61 ms | 4.86 ms |
| PATCH | 1.16 ms | 3.20 ms |

Both read paths issue one query for an item and two for a counted list. The raw
PATCH uses `UPDATE … RETURNING` in one round trip. The generated PATCH does a
visibility precheck, a guarded update, and a scoped re-read. These measurements
characterize the SQLite router on one host. The PostgreSQL budgets above still need
a matched live comparison.

## Execution log

| Date | Commit | Environment | Result |
|---|---|---|---|
| 2026-11-14 | uncommitted | Win11, rustc 1.97.1, no PostgreSQL | D1 **Pass** (D1.2 and D1.12 review only); D2 **Not executed** |
| 2026-11-15 | uncommitted | Win11, rustc 1.97.1, Docker Desktop 29.3.1 (images per `docker-compose.yml`) | D1 unchanged (**Pass**). D2 executed but **inconclusive**: host-bound (raw `SELECT 1` p50 1.72 ms ≥ dbmr `read_pk` p50 1.48 ms) |
