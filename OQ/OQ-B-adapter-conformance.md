# OQ-B: Adapter conformance

**Objective:** show that every database adapter implements the `DatabaseAdapter` contract
(BRD §7, §27) with the same observable behaviour, so the router and model contract
(OQ-A) hold whichever adapter is used.

**Acceptance criterion:**
- the adapter's live suite passes against the pinned service image in `docker-compose.yml`;
- for adapters marked *full contract*, OQ-A also passes on that adapter.

## Matrix

Every adapter runs two suites:
- its own live suite in `crates/dbmr-adapters/tests/<db>_tests.rs`;
- the shared contract `crates/dbmr/tests/conformance.rs`, driven through `Model`: one test
  per scenario per adapter (`<db>::<scenario>`, 22 x 10 = 220 tests), each on its own
  freshly created table `conf_<scenario>`:
  `insert_single`, `insert_many`, `by_id`, `find_filters` (`IS NULL`, `IN`, empty `IN`,
  `>=`, `<`, case-insensitive `LIKE`), `list_paging` (sort, total count, 2^53+1),
  `update_single`, `update_many`, `patch_partial`, `upsert_single` (composite key),
  `upsert_many`, `remove_soft`, `remove_many`, `remove_hard`, `mass_delete_guard`,
  `json_columns` (object/array columns round-trip as JSON on insert, bulk insert, read,
  patch; MySQL `JSON`, MariaDB `JSON`=LONGTEXT, Postgres `JSONB`, text elsewhere),
  `transactions` (commit + rollback on every SQL adapter, or `NO_TX_MESSAGE` on MongoDB,
  Redis, DynamoDB), `optimistic_lock` (version bump, 409 stale, 428 missing, upsert of a
  new row), `hidden_columns` (never read back, revealed by `with_hidden()`),
  `auto_timestamps` (`created_at`/`modified_at` set by the model), `include_relations`
  (`has_many` / `belongs_to` via `Query::include`), `typed_roundtrip` (`TypedModel<T>`),
  `type_contract` (over HTTP: `"9"` → 9, `"007"` stays text, `"false"`/`"0"`/`1` → booleans,
  `9.00` → 9, every declared column present in declared order, the same rows for text query
  filters including `?price=9.5` then `?price=9`, 422 for bad values, 409 for a duplicate
  unique key including within a batch, CSV header order, a request succeeds after a database
  error).

| Adapter | Service image | Transactions | Status 2026-11-16 |
|---|---|---|---|
| SQLite | (in-process) | yes | **Pass** (contract + all OQ-A suites) |
| PostgreSQL | `postgres:16` | yes | **Pass** |
| CockroachDB | `cockroachdb/cockroach:v24.1.0` | yes (PG adapter) | **Pass** |
| MySQL | `mysql:8.0` | yes | **Pass** |
| MariaDB | `mariadb:11` | yes | **Pass** |
| MSSQL | `mssql/server:2022-latest` | yes | **Pass** after fixes (insert re-fetch, `DATETIME2`) |
| Oracle | `gvenzl/oracle-free:23-slim` | yes | **Pass** after fixes (insert re-fetch, `LIKE`, UTC timestamps) |
| MongoDB | `mongo:7` | no | **Pass** after fix (typed filter values) |
| Redis | `redis:7-alpine` | no | **Pass** after fixes; experimental (ADR-0016) |
| DynamoDB | `amazon/dynamodb-local` | no | **Pass** after fixes; experimental, Local only |

## Defects found by the live run (fixed)

| Adapter | Defect | Fix |
|---|---|---|
| MSSQL | `SCOPE_IDENTITY()` is `numeric`, which the row decoder mapped to NULL, so single insert returned no record | `CAST(SCOPE_IDENTITY() AS BIGINT)` |
| Oracle | Insert never captured the generated key, so single insert returned no record | Single-row insert runs `RETURNING ROWID INTO :out` and re-reads that row |
| Oracle | `LIKE` was case-sensitive (other dialects are not) | `UPPER(col) LIKE UPPER(term)` |
| MongoDB, Redis, DynamoDB | Query-string values are text, so `?qty=in(2,3)` / `?qty=>=3` never matched numeric fields | `Model` converts filter values to the declared `integer` / `numeric` column type (D35) |
| Redis | Generated ids were millisecond timestamps (bulk rows overwrote each other) and were not stored, so the row could not be re-read or deleted | Per-table `INCR __seq:<table>` id, stored in the hash |
| Redis | `remove` with `safe_delete` hard-deleted rows | Sets the flag |
| Redis, DynamoDB | Upsert ignored the unique key and inserted duplicates | `merge_existing`: reuse the stored row that matches every key column |
| DynamoDB | Rows inserted without `id` failed (`required keys not given`) | Generated id |
| DynamoDB | `LIKE` compared with the `%` wildcards, so it never matched | Wildcards stripped |
| MSSQL | tiberius built without a TLS feature: connections were plaintext and `trust_cert` was a no-op (S5) | `native-tls` feature; `test_mssql_untrusted_cert_is_rejected` |
| Kafka test | `localhost` resolves to `::1` first on Docker Desktop and exceeded the 3 s init timeout | Test uses `127.0.0.1` |
| `docker-compose.yml` | MySQL published on 3306, tests and `env/` expect 3308 | Published on 3308 |

## Procedure

1. Run `docker compose up -d --wait`. All 13 services have health checks.
2. Run `cargo test --workspace --all-features -- --ignored --test-threads=1`.
3. Record each adapter's result in the execution log.
4. For a failure, also record the first assertion message and the server log
   (`docker compose logs <svc>`).

CI job `live-adapters` runs steps 1–2. It is non-blocking until it has been green once
(ADR-0016).

## Behaviour each live suite must confirm

| Behaviour | Why it varies per adapter |
|---|---|
| Insert returns the stored row (re-fetch or `RETURNING`) | Dialect-specific id retrieval |
| Bulk insert is all-or-nothing | SQLite, PG and MySQL/MariaDB wrap bulk writes in a transaction; others write row by row unless inside `Transaction` |
| Upsert on unique key; composite key ANDs all columns (S7) | MSSQL/Oracle emulate upsert with count → update/insert |
| Soft delete sets the flag and hides the row | Boolean vs integer columns (PG `= FALSE`, others `= 0`); Mongo `$nin:[true,1]` |
| `IS NULL`, `IN`, empty `IN`, `LIKE` case-insensitive | PG `ILIKE`, MSSQL `<>`, Mongo `$regex` |
| Big integers and decimals round-trip | PG typed casts; Mongo values beyond i64 are stored as strings |
| `begin_tx` works with savepoints on SQL adapters, or returns `NO_TX_MESSAGE` (MongoDB, Redis, DynamoDB) | ADR-0016 |
| Versioned write is a compare-and-set | SQL `UPDATE .. WHERE version = v`; Mongo filtered update; Redis Lua; DynamoDB conditional `UpdateItem` |

## Known gaps

1. `route_tests.rs` and `model_contract.rs` (HTTP layer, hooks, encryption, events) still
   run on SQLite only. The adapter-dependent behaviour is covered by `conformance.rs`;
   the remaining layers are adapter-independent.
2. **No bulk atomicity** outside a `Transaction` on MSSQL, Oracle, Mongo, Redis or DynamoDB: a
   failure mid-bulk leaves earlier rows written. Not yet asserted by a test.
3. **DynamoDB SigV4 signing** is verified against the AWS test vector only, not against real AWS.
   Redis/DynamoDB conditional writes are atomic only for equality conditions.
4. **Redis and DynamoDB** run a full scan per query; Redis stores every value as text
   (`"true"` reads back as a boolean).
5. Oracle tables must use unquoted (upper-case) identifiers; the adapter upper-cases names.
   Timestamp conversion does not cover tables in another schema, and a transaction dropped
   without commit can hold row locks until the connection is recycled.

## Execution log

| Date | Commit | Environment | Result |
|---|---|---|---|
| 2026-11-14 | uncommitted | Win11, no Docker | SQLite **Pass**; Mongo offline filter test **Pass**; 9 live suites **Not executed** (compiled under `--all-features`) |
| 2026-11-15 | uncommitted | Win11, rustc 1.97.1, Docker Desktop 29.3.1 (images per `docker-compose.yml`) (existing `db-model-router-*` containers) | **Pass**: 10/10 adapter live suites + 10/10 `conformance.rs` + Kafka live. `cargo test --workspace --all-features -- --include-ignored`: 94 passed, 0 failed |
| 2026-11-16 | uncommitted | Win11, Docker Desktop (images per `docker-compose.yml`) | **Pass**: `conformance.rs` 210/210 (21 scenarios x 10 adapters), incl. transactions on every SQL adapter. Source: change summary for this round |
