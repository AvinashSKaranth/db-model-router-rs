# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.0.1] - 2026-11-20

### Changed (breaking): row-level scope replaces override rules (ADR-0017)
- `create_model_router(model, overrides)` is now `create_model_router(model)`, and
  `create_model_router_with(model, overrides, opts)` is `create_model_router_with(model, opts)`.
  The override DSL (`user.*`, `params.*`, `query.*`, `headers.*`, `body.*`, literals),
  `apply_payload_override`, `try_apply_payload_override` and `dbmr_core::path` are removed.
- New `Scope` request extension (`dbmr::Scope`), set by the app's auth middleware. The router
  copies it to `Context.scope`. The **model** enforces it on every operation: reads AND it
  in, deletes AND it in (the mass-delete guard still applies), inserts get scalar values
  written in, update/upsert/patch targets outside it are 404. A JSON array means IN; an
  insert under a list scope must name one value (422 `The <col> is outside the scope.`).
- `ModelBuilder::require_scope(cols)`: a call without those scope columns, or with `null`, is
  401 `Missing scope: <col>`. `Context::system()` skips it and sees every row (jobs).
- `Context::with_scope` takes `(column, value)` pairs; `Context` gained `system: bool`.
- Migration: replace `json!({"tenant_id": "user.tenant_id"})` with a middleware that inserts
  `Scope::new([("tenant_id", json!(tenant))])`; replace `params.*` overrides on nested routes
  with a `route_layer` middleware that adds the path parameter to the scope.
- `POST /` with one JSON object now creates one record and returns it (before: 400). Bulk
  stays `POST /` with `{"data":[...]}`. Item routes (`/{id}`) refuse `{"data":[...]}` on
  every method with 400 `Bulk writes ({"data": [...]}) use the collection route`.

### Security
- A `{"data": {...}, "other": ...}` filter no longer drops sibling keys (they are ANDed).
- Bulk `PUT` can no longer update or take over rows outside the caller's scope, including
  through the upsert conflict key.
- Item routes can no longer be used for bulk writes that bypass item-level checks.
- CSV export prefixes cells starting with `=`, `+`, `-`, `@`, tab or CR with `'` (formula
  injection) and quotes cells containing CR.
- `serde_json` `preserve_order` is on, so record keys keep declared order everywhere.

### Added
- `crates/dbmr/examples/tour.rs`: every feature in one runnable file with asserts.
- `skills/dbmr/references/examples.md`: worked examples for every feature.

### Removed (breaking)
- `dbmr-cli`, `dbmr-dbmanager`, the migration runner and `xtask migrate`. The library is now model + router only (ADR-0013).
- `ModelInstance::new(...)` positional constructor. Use `Model::builder(adapter, table)...build()?`.
- `begin_tx` on MySQL, MariaDB, MSSQL, Oracle, CockroachDB, Mongo, Redis and DynamoDB (it was not atomic). These now return `NO_TX_MESSAGE` (ADR-0016).

### Changed (breaking)
- Same API contract on every adapter. Values follow the declared rule on every adapter: writes convert text forms (`"9"` → `9` for `integer`/`numeric`, `"true"`/`"false"`/`"1"`/`"0"` → boolean) after validation, reads convert driver forms (`0`/`1` → `false`/`true`, `9.00` → `9`, numbers in a `string` column → text), and filter values get the same conversion. Every declared column is returned (null when unset) in declared order after the primary key, then undeclared columns. Columns without a type rule pass through as the database returns them. The soft-delete, version and auto
  timestamp columns are declared automatically. Non-integral `numeric` text is stored as f64.
- Declared unique keys are checked on insert (single and bulk, including duplicates within a
  batch) on every adapter: 409 `Duplicate value for unique key (a, b).` (bulk: `data.<i>: `
  prefix). Before: a database 500 on SQL, a silent duplicate on MongoDB/Redis/DynamoDB.
- Redis stores hash fields type-tagged (`s:007`, `i:7`, `f:1.5`, `d:9.00`, `b:true`, `j:{..}`),
  so text such as `"007"` or `"true"` reads back as text. Data written by earlier builds is
  not read back with its types.
- Adapter `connect*` constructors (and `DynamoDbAdapter::new` / `with_credentials` / `from_env`) return `Arc<Adapter>`; drop the `Arc::new(db)` wrapper.
- Encryption key references are `env:VAR` (SHA-256 of the variable's value) or `raw:<base64 of exactly 32 bytes>`. Any other string is an error.
- Overrides fail closed: a missing `user.*` source is 401 (`AppError::Unauthorized`); any other missing source is 400 `Missing override value: <path>`. Before, the value became `""` (D8).
- Every 405 carries the JSON body `{"message":"Method Not Allowed","type":"danger"}`.
- New `AppError` variants: `Unauthorized` (401), `Conflict` (409), `PreconditionRequired` (428). Exhaustive matches need the new arms.
- Transactions are back on every SQL adapter (MySQL/MariaDB, MSSQL, Oracle, CockroachDB), reversing the `begin_tx` removal above. MongoDB, Redis and DynamoDB still return `NO_TX_MESSAGE`.
- MySQL/MariaDB bulk writes run in a transaction (all or nothing).
- Schema soft-delete key is `softDelete` (camelCase).
- `create_model_router(model, override_rules)` takes `impl Into<Arc<Model>>`. Users are passed through the `CurrentUser` request extension.
- Every model operation takes `&Context`.
- `model_from_meta` / `load_models_from_schema` take an `Option` encryption config and fail on `encrypted` columns without one.
- `MssqlAdapter::connect_host` takes `trust_cert: bool`. `connect(url)` honours `TrustServerCertificate`.

### Fixed (security, see OQ/OQ-C-security.md)
- S2: identifier quoting is escape-proof in every SQL dialect.
- S3: schema-built models no longer store `encrypted` columns as plaintext.
- S4: the override applies on every route and to the PUT/PATCH/DELETE pre-check, so tenants cannot cross.
- S5: MSSQL no longer forces `trust_cert`, and connections are now TLS-encrypted (tiberius `native-tls`; before, they were plaintext).
- S6: Mongo rejects `$` field names and uses explicit `$eq`.
- S7: MSSQL/Oracle composite-key upsert ANDs all columns.
- S8: a scalar or empty `IN` can no longer widen a filter.
- S9: the mass-delete guard runs after `before_delete` and returns 400.
- S10: 5xx bodies are scrubbed outside development/test.
- D19: the soft-delete flag cannot be set or cleared by clients.

### Fixed (correctness)
- PostgreSQL: a filter or write that bound a float and later an integer to the same SQL text
  (e.g. `?price=9.5` then `?price=9`) silently matched nothing, because sqlx reuses the
  prepared statement with the first call's parameter types. Numbers for typed columns are now
  sent as text with a cast to the column type.
- Oracle: a pooled connection is discarded after a statement error, so a session the server
  dropped is not handed to the next request (`early eof`).
- Redis: `"007"` read back as `7`, `"true"` as `true`, and `""` as null.
- MySQL/MariaDB/Oracle: `"false"` for a boolean column was a 500; now it is stored as false.
- MySQL/MariaDB: MEDIUMINT, UNSIGNED ints, BLOB/BINARY variants and DATE/TIME/DATETIME/TIMESTAMP
  read as `null`; now decoded (dates as ISO text, UTC for DATETIME/TIMESTAMP).
- MSSQL: TINYINT, SMALLINT, REAL, DECIMAL, UNIQUEIDENTIFIER, DATETIME and XML read as `null`.
- Oracle: NUMBER read as text (ids were `"1"`); DATE/TIMESTAMP and CLOB/BLOB returned debug
  text; boolean binds hung the session; `WITH`/mixed-case `SELECT` returned no rows; no
  reconnect after a dropped session.
- Redis/DynamoDB: `>`/`<` filters failed across Int/Float (e.g. `score>2` missed 2.5); DynamoDB
  NULL matched ordering filters; client-side sort was inconsistent for mixed-type columns
  (could panic). Both now share one total sort order.
- Kafka: configured `client_id` was not passed to the client.
- Schema: `options.encryption` was validated, then dropped by `parse_schema`.
- JSON `object` / `array` columns read back as JSON on every adapter. The model parses them
  when the database returns text (MariaDB `JSON` is `LONGTEXT`; SQLite, MSSQL, Oracle,
  MongoDB, Redis, DynamoDB). Before, MariaDB and the text stores returned a JSON string.
- MySQL `JSON` columns read as `null` (sqlx refused `String` for the JSON type).
- MSSQL `update` / `upsert` on a row carrying the IDENTITY key failed with error 8102; identity
  columns are now left out of `UPDATE SET`.
- Found by the first live run (2026-11-15, `crates/dbmr/tests/conformance.rs` on all 10 adapters):
  - MSSQL and Oracle single insert now return the stored record;
  - Oracle `LIKE` is case-insensitive;
  - filter values are converted to the declared column type, so numeric filters work on MongoDB, Redis and DynamoDB (D35);
  - Redis ids no longer collide on bulk insert and are stored; Redis soft delete sets the flag instead of deleting;
  - Redis and DynamoDB upsert honour the unique key; DynamoDB generates missing ids and its `LIKE` matches;
  - `docker-compose.yml` publishes MySQL on 3308 (as the tests expect).
- Rewrote the SQLite/PostgreSQL adapters:
  - real transactions with savepoints;
  - atomic bulk writes;
  - typed PostgreSQL parameters;
  - storage-typed decoding.
- Mongo, Redis and DynamoDB: `get` honours limit/offset; Mongo soft delete updates instead of deleting; DynamoDB follows `Scan` pagination.
- Oracle commit errors are surfaced.
- In-memory SQLite pools keep one never-expiring connection.
- Redis `=`, `!=` and `IN` compare numbers and text by string form (Redis stores text).
- MSSQL `DATETIME2` decoding (tiberius `tds73`), MySQL datetime write conversion, Oracle timestamp binding (session in UTC).

### Added
- Optimistic locking: `.version_column("version")` (schema `"version"`). Insert sets 1; update/patch/upsert must send the current version and bump it atomically on every adapter. Stale is 409 `Record has been modified by another request.`; missing is 428 `The version field is required for this update.` HTTP: `ETag` on single-record responses, `If-Match` supplies the version.
- Hidden columns: `.hidden([...])` (schema `hidden`). Never returned by reads, write results or events; hooks can still write them. `model.with_hidden()` reveals them server-side. HTTP strips them from write bodies, and filter/sort/select on them is 422.
- App-side timestamps: `.created_at(col)` / `.modified_at(col)` set RFC 3339 UTC text (`2026-01-01T00:00:00.000Z`); clients cannot set them. Schema `timestamps` now use this.
- Relations: `model.has_many(name, &child, fk)`, `model.belongs_to(name, &parent, fk)`, `Query::include([...])`, HTTP `?include=a,b` (one query per relation; unknown name is 422). Schema `relationships` wire them automatically. Relations are tenant-safe: `Context.scope` (filled by the router from the resolved override rules; `Context::with_scope` elsewhere) filters included rows on the scope columns the related model declares, and a written `belongs_to` foreign key must name a parent inside the scope (422 `Invalid reference: <fk>`, bulk rows prefixed `data.<i>: `).
- Typed models: `model.typed::<T>()` returns `TypedModel<T>`; reads return `Page<T> { data, count }`.
- `dbmr::http::create_model_router_with(model, overrides, RouterOptions)`; `RouterOptions::read_only()`. Disabled routes answer 405.
- `dbmr::http::openapi(title, version, &[(path, &model, &opts)])` (OpenAPI 3.1) and `openapi_route(spec)`.
- `dbmr::http::health_router(adapter)` backed by the new `DatabaseAdapter::ping()`; 200 `{"status":"ok"}` or 503 `{"status":"unavailable"}`.
- `PoolConfig` (default 10 max, 0 min, 30 s acquire) and `connect_with(..., PoolConfig)` on SQLite, PostgreSQL, MySQL, MSSQL and Oracle (Oracle takes host/port/service/user/password before it).
- DynamoDB SigV4 signing: `DynamoDbAdapter::with_credentials(endpoint, region, key, secret, session_token)` and `from_env()` (`AWS_*` variables). `new(endpoint)` is for DynamoDB Local (region `us-east-1`, dummy credentials).
- `dbmr::kafka` re-export (feature `kafka`).
- Debug tracing span on every public model operation (`table` field).
- `type_contract` conformance scenario: the HTTP contract (types, record shape, filters, 422s,
  409 duplicates, CSV header, recovery after a database error) must be identical on all 10
  adapters, so a database or driver upgrade that changes returned values fails CI.
- Conformance suite grows to 22 scenarios x 10 adapters (`optimistic_lock`, `hidden_columns`, `auto_timestamps`, `include_relations`, `typed_roundtrip`; `transactions` now commits on every SQL adapter).
- `AGENTS.md`, `llms.txt` and `crates/dbmr/examples/quickstart.rs`.
- OQ protocols in `OQ/` with an LLM ease-of-use harness (`OQ/oq-e`).
- An honest traceability builder (`scripts/build_traceability.py`) and gate.
- CI jobs: deny/audit/gitleaks, coverage, live adapters, bench build, example run.

## [0.1.0] - 2026-11-08

### Added
- Complete Rust 2024 rewrite of `db-model-router` from Node.js reference.
- Core crate `dbmr-core`:
  - `DbValue` enum and `RowMap` supporting high-precision numerics (`rust_decimal::Decimal`), arbitrary JSON, and 64-bit integer fidelity.
  - AES-256-GCM field-level encryption with versioned envelope format (`enc:vN:<base64>`) and tamper detection.
  - Full filter AST (`FilterExpr`, `Condition`, `Operator`), `data_to_filter` parser, search compiler, and sort normalizer.
  - Model CRUD operations (`insert`, `by_id`, `update`, `patch`, `remove`, `upsert`, `list`) with pre-hooks and post-hooks.
  - Model-layer mass-delete protection (Defect D15 fix).
- Schema crate `dbmr-schema`:
  - Strong schema parser and validator for `dbmr.schema.json`.
  - Proptest property-based schema roundtrip validation.
- Adapters crate `dbmr-adapters`:
  - Multi-database engine adapters: SQLite, PostgreSQL, MySQL, MariaDB, CockroachDB, Redis, MongoDB.
  - Live container integration test suite for all database engines.
  - SQL compiler for dialect-specific identifier quoting, WHERE clause generation, and pagination.
- HTTP routing crate `dbmr-http`:
  - Axum 0.8 model router supporting standard CRUD verbs.
  - Fallback handler for RFC 4791 `QUERY` method on collection and item routes.
  - Element-wise payload override engine with header/context resolution.
  - CSV and XML output format negotiation.
- Kafka producer crate `dbmr-kafka`:
  - Non-blocking Kafka producer with message batching (500 per chunk).
  - Transaction-level event buffering with automatic rollback discard and commit flush.
  - Produce failure swallowing ensuring message broker errors never abort database operations.
- CLI toolkit crate `dbmr-cli`:
  - Commands: `init`, `doctor`, `diff`, `types`, `migrate:create`, `migrate:up`, `migrate:down`, `migrate:status`, `encrypt:generate`.
  - Multi-target type generator for TypeScript `.d.ts` and Rust structs.
  - SQL migration runner for `.sql` up/down migration pairs with `_dbmr_migrations` table tracking.
- Database manager crate `dbmr-dbmanager`:
  - Web service on port 4000 serving `/api/*` management endpoints.
  - Self-contained embedded HTML/CSS/JS dashboard UI.
  - Raw SQL console with `--enable-raw-sql` safety gate.
  - CSV export for all database tables.
- Facade crate `dbmr`:
  - Re-exports of all underlying sub-crates with unified feature flags.
  - Global `OnceLock` database singleton shim (`init_global` and `global_db`).
- Benchmark crate `dbmr-bench`:
  - Performance suite comparing dbmr throughput and latency against raw SQLx queries.
