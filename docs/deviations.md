# Deviations from Node `db-model-router`

This document records intentional design and behavioral deviations between the Node.js implementation (`db-model-router`) and the Rust rewrite (`dbmr`). Every deviation is cross-referenced against an approved Architecture Decision Record (ADR) and Section 34 of the BRD.

| ID | Topic | Upstream (Node) Behavior | Rust Rewrite (`dbmr`) Behavior | ADR |
|---|---|---|---|---|
| D1 | Route path for single insert | Documented as `POST /add`, actually mounted as `POST /:id` | Preserved: mounted as `POST /:id` (path id stripped on creation) | ADR-0009 |
| D2 | Error status propagation | Inspects `err.cause.status` or `err.status` via stringly comparison | Fixed: Strongly-typed `AppError` enum carrying `axum::http::StatusCode` | ADR-0007 / ADR-0009 |
| D3 | Missing record sentinels | Inconsistent: `findOne` → `false`, `byId` → `null`, `insert` → `null` | Fixed: Unified `Option<Record>` at model layer; HTTP maps `None` to 404 | ADR-0001 / ADR-0009 |
| D4 | Re-fetch miss on single write | Silently returns `null` and swallows Kafka events / hooks | Preserved: Returns `None` with `RefetchFailed` warning log | ADR-0009 |
| D5 | Bulk detection via own `data` property | Any object with a `data` key treated as bulk payload | Preserved: Detection kept; schema validator warns if column named `data` | ADR-0009 |
| D6 | CSV header & XML quotes | CSV header from first row only; XML escapes `& < > "` but not `'` | Preserved: Byte-parity for CSV/XML export formats | ADR-0009 |
| D7 | `sendFormatted` row unwrapping | Wraps `{ data }` objects as rows | Preserved: Handled identically | ADR-0009 |
| D8 | Override deletion of `"null"` strings | Deleted any value matching the literal string `"null"` | Removed with the override DSL (ADR-0017). Row permissions come from a `Scope` set by app middleware and enforced by the model; a missing or `null` required scope fails closed with 401 `Missing scope: <col>` (pinned by `scope_tests.rs::require_scope_fails_closed_and_system_opts_out`) | ADR-0009, ADR-0017 |
| D9 | `dataToFilter` input mutation | Mutated original input object (`delete data.filter`) | Preserved semantics, purified: cloned before merge, input not mutated | ADR-0009 |
| D10 | NULL filter comparison | Emitted `= NULL` in SQL (which matches zero rows) | Fixed: Generates explicit `IS NULL` / `IS NOT NULL` SQL predicates | ADR-0009 |
| D11 | Nested SQLite savepoint events | Events flushed immediately on savepoint release | Fixed: Kafka events buffered until the outermost transaction commits | ADR-0009 |
| D12 | CLI OutputContext crash | Called non-existent `success/info/error` methods on OutputContext | Out of scope: CLI extracted | ADR-0013 |
| D13 | Timestamp auto-management claims | Claimed auto-management but relied completely on DB defaults | Changed: database defaults still work; `.created_at(col)` / `.modified_at(col)` (and schema `timestamps`) are set by the model as RFC 3339 UTC text and are not client-writable (see D42) | ADR-0009 |
| D14 | List/find count total | `count` returns total matching rows across database, not page size | Preserved: Documented and preserved | ADR-0009 |
| D15 | Empty filter mass-delete hazard | `{ filter: [] }` resulted in unbounded mass-delete | Fixed: Model-layer mass-delete guard requires `allow_destructive = true` | ADR-0009 |
| D16 | `inspect` heuristics for softDelete | Inferred softDelete from boolean columns heuristically | Out of scope: `inspect` is CLI | ADR-0013 |
| D17 | Insecure MSSQL TLS defaults | Defaulted to `encrypt:false` and `trustServerCertificate:true` | Fixed: Secure defaults (`encrypt:true`, `trustServerCertificate:false`) | ADR-0009 |
| D18 | Timestamp casing variants | Stripped multiple casing variants (`createdAt`, `creationDate`, etc.) | Preserved: Variant stripping lists kept | ADR-0009 |
| D19 | Client-writable `safeDelete` | Allowed clients to set/clear soft-delete flag in write payloads | Fixed: `safeDelete` stripped from HTTP write payloads; server-managed | ADR-0009 |
| D20 | GET-by-id validation bypass | Did not execute full `byId` validation rules | Preserved: Uses `find`-based single lookup without ID shape rejection | ADR-0009 |

## Added 2026-11-14 (review fixes)

Each row is pinned by the listed test. "Verified" means the test passes on SQLite in-memory.

| ID | Topic | Node behavior | dbmr behavior | Pinned by |
|---|---|---|---|---|
| D21 | Rule argument split | niv splits `rule:arg` at every `:`, and `regex:` patterns are split on `,` | Split at the **first** `:` only; the `regex:` pattern is verbatim | `dbmr-core` `rules.rs` unit tests |
| D22 | CSV null cell | `null` rendered as the literal `null` | Empty cell `""`. No trailing newline; header comes from the first row (unchanged) | `route_tests.rs` CSV case, `formatter.rs::node_parity` |
| D23 | Binary columns in JSON | Buffer serialized as `{type:"Buffer",data:[...]}` | Standard base64 string | `DbValue::to_json` |
| D24 | Upsert conflict target | First declared unique key, even when the row does not carry it | First unique key that **every** row fully supplies, else the PK when every row has it, else a plain insert. Mongo, MSSQL and Oracle AND all key columns and never match on a partial key | `model_contract.rs::upsert_uses_unique_key_or_pk` |
| D25 | Insert re-fetch and soft-delete | Insert re-fetch ignores `safeDelete` | **Preserved**. Update and upsert re-fetches honour it | `model.rs` `write_one` |
| D26 | Unbounded delete | `{}` / `{filter:[]}` deletes every row | 400 `Request body must contain filter criteria`, unless the model is built with `allow_destructive(true)` | `model_contract.rs::soft_delete_hides_rows_and_mass_delete_is_guarded`, `route_tests.rs` |
| D27 | Transactions | Every SQL adapter | Every SQL adapter (SQLite, PostgreSQL/CockroachDB, MySQL/MariaDB, MSSQL, Oracle). MongoDB, Redis and DynamoDB return `Active database adapter does not support transactions.` | ADR-0016, `model_contract.rs::transaction_commit_rollback_and_savepoints`, `conformance.rs` `transactions` |
| D28 | Unknown column in filter (SQLite) | SQLite treats an unknown `"col"` as a string literal, giving 0 rows and no error | **Preserved** (database quirk). PostgreSQL and MySQL return a 500 | `route_tests.rs` |
| D29 | Malformed JSON body | Express `body-parser` error (400, Express default error body) | 400 `{"message":"Malformed JSON body","type":"danger"}`. For `QUERY`, a malformed body is ignored and query-string params are used | `route_tests.rs::malformed_json_is_400`, QUERY case |
| D30 | Unsupported method | Express gives 404 | 405 `{"message":"Method Not Allowed","type":"danger"}` (also for routes disabled by `RouterOptions`). `QUERY` is routed on both collection and item paths | `route_tests.rs`, `router_features_tests.rs::read_only_router_answers_405_for_writes` |
| D31 | Router path param | `:id` | Internal name `__dbmr_id`, so nesting under a parent route with its own `{id}` cannot collide | `scope_tests.rs::nested_routes_scope_by_path_parameter` |
| D32 | 5xx body | Error message always returned | Scrubbed to `Internal server error` unless `NODE_ENV` is `development`, `test` or `TEST` | `route_tests.rs` 5xx scrub case |
| D33 | Big integers | Precision lost above 2^53 | Lossless through JSON (`arbitrary_precision`); a value beyond i64 is sent as text, with a typed cast on PostgreSQL | `route_tests.rs` big-int case |
| D34 | Mongo field names | Keys passed through, so `{"$where":...}` reached the query | Keys starting with `$` (or empty) are rejected with 422 `Invalid field name`, and every comparison uses an explicit `$eq` etc. | `mongo_adapter.rs::tests::filter_is_operator_safe` |
| D35 | Filter value types | Query-string values compared as text | `Model` converts filter values to the declared `integer`/`numeric` column type before the adapter sees them (text that does not parse is kept), so typed stores match `?qty=3` | `conformance.rs` (`mongodb`, `redis`, `dynamodb`) |
| D36 | Oracle `LIKE` | Case-sensitive | `UPPER(col) LIKE UPPER(term)`, matching the other dialects | `conformance.rs::oracle` |
| D37 | Key-value ids | Redis: millisecond timestamp, not stored; DynamoDB: none | Redis `INCR __seq:<table>` stored in the row; DynamoDB time + counter string | `conformance.rs` (`redis`, `dynamodb`) |
| D38 | Key-value upsert | Redis/DynamoDB upsert inserted duplicates | Matches the stored row on every column of a unique key and overlays it | `conformance.rs` (`redis`, `dynamodb`) |

## Added 2026-11-16 (features with no Node counterpart)

| ID | Topic | Node behavior | dbmr behavior | Pinned by |
|---|---|---|---|---|
| D39 | Optimistic locking | None; last write wins | Opt-in `.version_column(col)`. Writes must carry the current version (body or `If-Match`); stale → 409 `Record has been modified by another request.`, missing → 428 `The version field is required for this update.` GET by id returns `ETag` | `conformance.rs` `optimistic_lock`, `router_features_tests.rs::etag_and_if_match_drive_optimistic_locking` |
| D40 | Hidden columns | None; every column is returned | Opt-in `.hidden([...])`: never returned or emitted, stripped from client writes, 422 `Invalid filter Inputs` when filtered/sorted/selected | `conformance.rs` `hidden_columns`, `router_features_tests.rs::hidden_columns_are_not_writable_or_filterable` |
| D41 | Disabled routes | Every route mounted | `create_model_router_with(.., RouterOptions)` can disable operations; they answer 405 (D30) | `router_features_tests.rs::read_only_router_answers_405_for_writes` |
| D42 | Timestamps | DB defaults only (D13) | `.created_at` / `.modified_at` set `YYYY-MM-DDTHH:MM:SS.mmmZ` in the model; client values are dropped | `conformance.rs` `auto_timestamps` |
| D44 | Value types | Whatever the driver returned (`0`/`1` vs `true`/`false`, `"9"` stored as text on MongoDB/DynamoDB, `"007"` → `7` on Redis); missing keys on NoSQL | Values normalized to the declared rule on write, read and filter; declared columns always present (null) in declared order | `conformance.rs` `type_contract` (all 10) |
| D45 | Unique keys | Database error (500) on SQL; duplicates accepted on MongoDB/Redis/DynamoDB | Declared unique keys checked on insert: 409 `Duplicate value for unique key (..).` | `conformance.rs` `type_contract`, `model_contract.rs::bulk_insert_is_atomic`, `route_tests.rs::server_errors_are_scrubbed` |
| D43 | Relations | None | `has_many` / `belongs_to` + `?include=`; unknown name → 422. Included rows and written `belongs_to` keys are confined to the request scope (`ctx.scope`): foreign rows are dropped from includes, a foreign key outside the scope → 422 `Invalid reference: <fk>` | `conformance.rs` `include_relations`, `router_features_tests.rs::include_works_on_list_query_and_item`, `::includes_and_foreign_keys_stay_inside_the_scope` |

## Added 2026-11-19 (row scope, ADR-0017)

| ID | Topic | Node behavior | dbmr behavior | Pinned by |
|---|---|---|---|---|
| D46 | Permissions | Payload override rules (`user.*`, `params.*`, `headers.*` ...) forced into filters and payloads by the router | No override DSL. App middleware inserts a `Scope` extension; the model ANDs it into reads, updates and deletes, writes it into inserts, and refuses targets outside it (404). Arrays mean IN. `.require_scope` fails closed (401 `Missing scope: <col>`); `Context::system()` opts out | `scope_tests.rs` (all) |
| D47 | `POST /` with one object | 400 (bulk only) | Single create, returns the record. Item routes refuse `{"data":[...]}` (400) | `route_tests.rs::bulk_put_requires_data_array_and_post_takes_one_object`, `route_edge_tests.rs::item_routes_refuse_bulk_bodies` |
| D48 | CSV export | Cells written as-is | Cells starting with `= + - @`, tab or CR are prefixed with `'` (formula injection) | `route_edge_tests.rs::csv_neutralizes_formulas` |
