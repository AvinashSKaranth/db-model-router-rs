# Business Requirements Document (BRD)

## Full Rust Rewrite of `db-model-router`

**Source library:** `C:\Workspace\eForge\db-model-router` (`https://github.com/AvinashSKaranth/db-model-router`)  
**Target project:** `C:\Workspace\eForge\db-model-router-rs`  
**Document:** `.spec/brd.md`  
**Status:** Draft for implementation planning

---

## 1. Purpose

This document defines the business and functional requirements for a **complete Rust rewrite** of the `db-model-router` library. The rewrite must preserve the library's public behavior, adapter matrix, model CRUD semantics, routing, CLI workflow, schema-driven generation, encryption, transactions, migrations, and optional Kafka integration while achieving **greater than 95% automated test coverage**.

This is **not** a line-by-line translation. The Rust implementation should:

- Re-express the JavaScript API in idiomatic Rust.
- Preserve observable behavior and compatibility contracts.
- Improve type safety, memory safety, concurrency, and performance.
- Be testable in isolation for every adapter and core module.
- Avoid hidden global state where possible, while retaining the documented `init()` + `db` proxy ergonomics.

---

## 2. Source Library Summary

`db-model-router` is a database-agnostic REST API generator for Node.js. It supports 10 database adapters behind one model API and generates Express/ultimate-express routers, migrations, tests, OpenAPI specs, and optional SaaS scaffolding.

Core capabilities:

- Runtime database initialization via `init(adapter, config)`.
- A dynamic `db` proxy forwarding calls to the selected adapter.
- A `model(...)` factory producing a `ModelInstance`.
- A `route(model, override?)` factory producing an HTTP router with 11 endpoints.
- Filter syntax supporting AND/OR, operators, query-parameter operators, and multi-column search.
- Single and bulk insert/update/upsert/delete.
- Soft delete via `safeDelete`.
- Timestamp **field stripping** on write (the model removes `created_at`/`modified_at`
  variants from payloads and relies on database defaults emitted by generated
  migrations; it never sets or refreshes timestamps itself).
- Hooks/interceptors.
- Field-level AES-256-GCM encryption.
- Cross-model ACID transactions with scoped models and Kafka buffering.
- Kafka event production for writes.
- CLI commands for init, inspect, doctor, diff, db-manager, types, migrations, encryption, and SaaS generation.
- Schema-driven project generation from `dbmr.schema.json`.
- Adapter-specific connection configuration and environment variables.

---

## 3. Rewrite Goals

### 3.1 Primary Goals

1. Reproduce all documented public behaviors of `db-model-router`.
2. Support all 10 adapters:
   - MySQL
   - MariaDB
   - PostgreSQL
   - SQLite3
   - MongoDB
   - MSSQL
   - CockroachDB
   - Oracle
   - Redis
   - DynamoDB
3. Provide a Rust-native API with strong typing.
4. Achieve **>95% test coverage** across core modules and adapter logic.
5. Support deterministic, isolated tests, including property-based tests.
6. Maintain CLI parity for generation, inspection, validation, migrations, and encryption tooling.

### 3.2 Non-Goals for Initial Release

- Byte-for-byte compatibility with generated JavaScript project files.
- Supporting every optional Node.js peer dependency exactly as-is.
- Reimplementing the browser-based db-manager UI unless explicitly scoped; if included, it may be a separate web crate.

---

## 4. Success Criteria

The rewrite is complete when:

- All core model operations pass equivalent behavior tests.
- All adapter contracts pass integration tests against real or containerized databases.
- CLI commands generate equivalent project structures and pass golden-file tests.
- Encryption round-trips and rotation tests pass.
- Transaction rollback/commit tests pass.
- Kafka production tests pass when enabled.
- Coverage report shows **>95%** for:
  - Core model logic
  - Validator/filter logic
  - Route handlers
  - Encryption
  - Migration runner
  - CLI generators where feasible
- Quality gates in Â§36 are green (fmt, clippy deny-warnings, `cargo audit`,
  `cargo deny`, secrets scan, feature-matrix build).
- Null/undefined/default test suite (Â§12.5) and security test suite (Â§12.4)
  are green.
- PostgreSQL benchmark vs Diesel/SeaORM/sqlx (Â§38) meets the Â§38.6 budget, or a
  documented deviation is recorded.
- No critical regressions in documented examples.

---

## 5. Proposed Rust Architecture

### 5.1 Workspace Layout

```text
db-model-router-rs/
  Cargo.toml                  # workspace; ONE published crate is `dbmr`
  crates/
    dbmr-core/                # model, filters, validation, encryption, tx  (publish = false)
    dbmr-schema/              # schema parsing/validation/generation        (publish = false)
    dbmr-adapters/            # feature-gated database adapters             (publish = false)
    dbmr-http/                # HTTP router generation (axum default)       (publish = false)
    dbmr-cli/                 # CLI logic; binary comes from facade         (publish = false)
    dbmr-kafka/               # optional Kafka producer                      (publish = false)
    dbmr-dbmanager/           # optional management UI/API                   (publish = false)
    dbmr/                     # **public facade**: re-exports; [[bin]] name = "dbmr"
  .spec/  docs/  tests/
```

**Publishing strategy (ADR-0005):** only `dbmr` is published (MSRV 1.85, edition
2021, MIT). Default features = `sqlite`; other 9 adapters behind feature flags
(`postgres`, `mysql`, `mariadb`, `mssql`, `oracle`, `cockroachdb`, `mongodb`,
`redis`, `dynamodb`); `kafka`/`db-manager-web` optional and empty by default.
Install: `cargo add dbmr` or `cargo add dbmr --features "postgres kafka"`;
`cargo install dbmr` for the CLI.

### 5.2 Design Principles

- Use traits for adapter and model behavior. **All traits intended for
  `Arc<dyn ...>` must be object-safe** â€” no generic methods or generic
  associated types.
- Make optional database drivers feature flags.
- Avoid global mutable state; allow an optional global registry for compatibility,
  implemented with `std::sync::OnceLock` (or `arc_swap` if re-init is a real
  requirement) â€” never a `Mutex` on the read hot path.
- Use object-safe async traits via `async-trait` (async fn in trait is
  object-safe only when non-generic; prefer the explicit macro for dyn
  compatibility across editions).
- Use **typed internal rows** (`RowMap`/`DbValue`, Â§7.1) internally; use
  `serde_json::Value` only at the HTTP/CLI boundary.
- Use `serde_json` with the `arbitrary_precision` feature for big-int fidelity;
  never regex-patch JSON strings.
- Use structured errors with HTTP status mapping.
- Use property-based testing (`proptest`) for filters and validators.
- Parse model rules once at construction (Â§6.2); the hot path iterates over
  compiled structures only.
- Hooks are zero-cost when unused (Â§6.5).

---

## 6. Core Functional Requirements

### 6.1 Initialization

**Requirement ID:** FR-INIT-001

The system shall provide:

```rust
pub fn init(adapter: &str, config: Option<Config>) -> Result<()>
```

Behavior:

- Accept adapter keys: `mysql`, `mariadb`, `postgres`, `postgresql`, `oracle`, `sqlite3`, `mssql`, `cockroachdb`, `mongodb`, `redis`, `dynamodb`.
- Load the corresponding adapter.
- Resolve encryption configuration once at initialization.
- Preserve already-resolved keyring on bare re-init when no config is provided.
- Return a clear error for unsupported adapters or missing drivers.
- Expose a `db` handle/proxy to the active adapter.
- **Global registration:** the global handle is stored in `std::sync::OnceLock`
  (set once; repeat init = explicit error unless `arc_swap`-style re-init is
  selected). Hot-path reads are lock-free. Re-init for tests requires an
  explicit `ArcSwap` ring behind the `testing` feature or dependency-injected
  handles; `init()` must never take a `Mutex` on the read path.

**Tests:**

- Unsupported adapter error.
- Missing driver error.
- Encryption config resolution.
- Re-init behavior (OnceLock: second bare set returns error / or arc-swap per
  decision Â§35.8).
- Adapter key aliases (`postgres` / `postgresql`).
- Concurrent readers under `init` (loom-style test).

---

### 6.2 Model Factory

**Requirement ID:** FR-MODEL-001

The system shall provide:

```rust
pub fn model<T>(
    db: Arc<dyn DatabaseAdapter>,
    table: &str,
    structure: ModelStructure,
    primary_key: &str,
    unique: UniqueSpec,
    options: ModelOptions,
) -> ModelInstance<T>
```

Also support a global-db convenience form:

```rust
pub fn model_from_global<T>(...)
```

Model structure rules:

- Type rules: `string`, `integer`, `numeric`, `boolean`, `object`, `datetime`, `auto_increment`.
- `required|` prefix for NOT NULL.
- `encrypted|` prefix for encrypted fields, and must precede `required|`.
- Validation rules such as `regex:...`, `maxLength:...`.
- The primary key is removed from payloads on **create paths only** (insert,
  and upsert when the record does not already exist).
- Timestamp columns (and their known spelling variants) are removed from all
  write payloads.
- The soft-delete column is **not** stripped and remains writable by clients;
  non-PK `auto_increment` columns are also **not** stripped. Any other unknown
  key is stripped after validation.
- See Â§21 for the exact per-operation stripping and validation matrix.

**Rule compilation (normative):** rule strings such as
`encrypted|required|string|regex:^[a-z]+$` are parsed **once at `model()`
construction** into a compiled `Vec<CompiledColumn>`:

```rust
pub struct CompiledColumn {
    pub name: CompactString,
    pub index: usize,              // stable slot in the compiled column set
    pub rules: CompiledFieldRule,  // enum: required, encrypted, type+arg, regex, ...
    pub encrypted: Option<EncryptedPath>,  // resolved dotted or top-level path
}
```

- All `regex:` arguments become precompiled `regex::Regex` instances.
- The validator-safe rule set (encrypted flag stripped) and the encryptable
  column index set are precomputed.
- Field stripping, validation, and encryption lookup iterate over precomputed
  bitsets/slices â€” rule strings are never re-parsed on insert/update.
- A malformed rule string fails at model construction, not at first request.

**Tests:**

- Signature variants.
- Structure validation.
- Encrypted metadata compilation.
- Unknown column stripping.
- Timestamp stripping.

---

### 6.3 Model Operations

**Requirement ID:** FR-MODEL-002

`ModelInstance` shall implement:

- `insert(data, context) -> Result<InsertResult>`
- `update(data, context) -> Result<UpdateResult>`
- `upsert(data, context) -> Result<UpsertResult>`
- `patch(data, context) -> Result<Option<Record>>`
- `remove(data, context) -> Result<RemoveResult>`
- `by_id(id, options, context) -> Result<Option<Record>>`
- `find(data, context) -> Result<FindResult>`
- `find_one(data, context) -> Result<Option<Record>>`
- `list(data, context) -> Result<FindResult>`
- `with_tx(tx) -> ModelInstance`

Single and bulk behavior:

- Single insert returns full record when adapter supports `RETURNING`/`OUTPUT`; otherwise re-fetch by PK.
- Bulk insert accepts `{ data: [...] }` and returns `{ rows, message, type }`.
- Update requires PK in payload.
- Patch fetches existing record, merges known fields, re-fetches updated record.
- Upsert PK is optional and uses unique columns for conflict.
- Remove supports single PK, filter object, or filter array.
- Soft delete sets `safeDelete` column to truthy value instead of hard deleting.
- All reads auto-filter soft-deleted rows.

**Tests:**

- Single/bulk insert.
- Update single/bulk.
- Patch merge and missing record.
- Upsert insert/update paths.
- Remove single/filter.
- Soft delete filtering.
- `findOne` returns `None` when no record matches (design-unified `Option`).
- `byId` invalid id returns 422.
- Pagination defaults: page 0, size 30, max 200.
- Sort normalization.

---

### 6.4 Filters

**Requirement ID:** FR-FILTER-001

Filter structure:

```text
[OR_groups[AND_conditions[col, op, val]]]
```

Operators:

- `=`, `!=`, `<`, `>`, `<=`, `>=`
- `LIKE`, `NOT LIKE`
- `IN`, `NOT IN`

Requirements:

- Empty object produces match-all filter.
- Query parameters parse into filter conditions using documented prefixes.
- `search` is reserved and expands to OR-groups across configured `search_columns`.
- Search is case sensitivity adapter-native.
- Empty/whitespace search is ignored.
- Search without configured columns is dropped.
- `%` and `_` in SQL adapters act as LIKE wildcards; Mongo/Redis/DynamoDB match literally where applicable.

**Tests:**

- Property-based filter generation.
- Query param operator parsing.
- IN/NOT IN parsing.
- Search expansion cross-product.
- Malformed filter handling.

---

### 6.5 Hooks

**Requirement ID:** FR-HOOKS-001

Model options shall support:

- `beforeSave`
- `afterSave`
- `beforeCreate`
- `afterCreate`
- `beforeUpdate`
- `afterUpdate`
- `beforeUpsert`
- `afterUpsert`
- `beforeDelete`
- `afterDelete`
- `beforeFind`
- `afterFind`

Behavior:

- Hooks may be sync or async. Provide **both** paths without boxing chaos:
  1. Compile-time/static API: a `ModelHooks` trait with default **no-op async
     fn** methods (`before_save`, `after_find`, â€¦). A model built with default
     hooks incurs zero overhead (empty vtables optimized away or never
     registered).
  2. Runtime/JS-style registration: a `BoxedHooks` registry holding
     `Box<dyn Fn(HookArgs) -> BoxFuture<'_, Result<HookArgs>>>` used when
     users supply closures at runtime.
- Hooks may mutate payloads/records.
- `beforeFind` may replace filter.
- `afterFind` may replace records.
- Hook errors propagate and abort operation.
- All hook trait methods take `&self`; the boxed registry is `Send + Sync` and
  shared via `Arc`.
- No mixed sync/async enum at the call site â€” the async boundary is uniform;
  sync hooks are expressed as trivially-awaited async closures.

**Tests:**

- Hook invocation order.
- Payload mutation.
- Filter replacement.
- Error propagation.

---

### 6.6 Transactions

**Requirement ID:** FR-TX-001

Provide:

```rust
pub async fn transaction<F, R>(f: F) -> Result<R>
where F: FnOnce(TransactionContext) -> Future<Output = Result<R>>
```

Behavior:

- Atomic multi-model execution.
- `tx.model(model_or_table)` returns model scoped to transaction.
- `tx.begin_tx()` on unsupported adapters returns the error
  `"Active database adapter does not support transactions."` (Â§26) before any
  begin.
- Kafka events buffered during transaction and emitted after commit.
- Rollback on error.
- Adapter support detection.

**Tests:**

- Commit path.
- Rollback path.
- Scoped model usage.
- Kafka buffering.
- Unsupported adapter error.

---

### 6.7 Field-Level Encryption

**Requirement ID:** FR-ENC-001

Support AES-256-GCM transparent encryption:

- Schema rule prefix `encrypted|`.
- Encrypt configured top-level columns and nested object fields.
- Envelope format: `enc:v<N>:<base64>`.
- Decrypt on read.
- Legacy plaintext passes through.
- Keyring with multiple versions.
- Active key version.
- Rotate keys via CLI.
- Scan/backfill via CLI.

Config:

```rust
EncryptionConfig {
    key: KeySource,
    version: u32,
    keys: BTreeMap<u32, KeySource>,
}
```

**Tests:**

- Encrypt/decrypt round trip.
- Nested fields.
- Legacy plaintext.
- Key rotation.
- Missing config error.
- CLI scan/apply/dry-run.

---

### 6.8 Kafka Integration

**Requirement ID:** FR-KAFKA-001

Optional Kafka producer:

- Disabled unless broker configured.
- `kafka.init(opts?)`.
- `kafka.disconnect()`.
- `kafka.produce(table, operation, data)`.
- `kafka.status()`.
- One event per affected row.
- Bulk chunking in groups of 500.
- Events only after successful writes.
- Failed produce logs warning and does not throw.

**Tests:**

- Disabled mode zero overhead.
- Event format.
- Bulk chunking.
- Failure handling.

---

## 7. Adapter Requirements

### 7.1 Common Adapter Trait

**Requirement ID:** FR-ADAPTER-001

All adapters implement the following **object-safe** (dyn-compatible) surface.
No generic methods, no generic associated types, no `&mut self` on the shared
trait, so `Arc<dyn DatabaseAdapter>` covers every caller.

```rust
/// Raw connection lifecycle lives on the CONCRETE adapter type, not the dyn
/// trait: `MysqlAdapter::connect(config) -> Result<Self>` (or `->
/// Result<Arc<Self>>`). The dyn boundary must not carry `&mut` methods.
#[async_trait::async_trait]
pub trait DatabaseAdapter: Send + Sync + 'static {
    async fn disconnect(&self) -> Result<()>;
    async fn is_connected(&self) -> bool;

    async fn get(&self, table: &str, filter: &FilterExpr, sort: &[SortKey],
                 safe_delete: Option<&str>, options: GetOptions) -> Result<FindResult>;
    async fn list(&self, table: &str, filter: &FilterExpr, sort: &[SortKey],
                  safe_delete: Option<&str>, page: u64, size: u64) -> Result<FindResult>;
    async fn count(&self, table: &str, filter: &FilterExpr,
                   safe_delete: Option<&str>) -> Result<u64>;

    async fn insert_rows(&self, table: &str, data: &[RowMap], unique: &[Vec<String>])
        -> Result<WriteSummary>;
    async fn upsert_rows(&self, table: &str, data: &[RowMap], unique: &[Vec<String>])
        -> Result<WriteSummary>;
    async fn remove_rows(&self, table: &str, filter: &FilterExpr, safe_delete: Option<&str>)
        -> Result<WriteSummary>;

    async fn query(&self, sql: &str, params: &[DbValue]) -> Result<QueryResult>;
    fn escape_id(&self, name: &str) -> String;

    /// Starts a transaction. Returns a trait object; commit/rollback consume it.
    async fn begin_tx(&self) -> Result<Box<dyn TransactionAdapter>>;
}

#[async_trait::async_trait]
pub trait TransactionAdapter: Send + Sync + 'static {
    async fn query(&self, sql: &str, params: &[DbValue]) -> Result<QueryResult>;
    async fn commit(self: Box<Self>) -> Result<()>;
    async fn rollback(self: Box<Self>) -> Result<()>;
}
```

Top-level convenience (generic, on concrete `Arc<dyn DatabaseAdapter>` â€” this is
where generics are allowed, off the dyn trait):

```rust
pub async fn transaction<F, R, Fut>(db: Arc<dyn DatabaseAdapter>, f: F) -> Result<R>
where
    F: FnOnce(TransactionContext) -> Fut,
    Fut: Future<Output = Result<R>>;
```

`TransactionContext` wraps the `Arc<dyn TransactionAdapter>` and resolves
`tx.model(model_or_table)` scoped to the transaction (Â§6.6).

**Internal row representation (not `serde_json::Value`):**

```rust
pub type RowMap = IndexMap<CompactString, DbValue>;
pub enum DbValue { Null, Bool(bool), Int(i128), Float(f64), Text(String),
                   Bytes(Vec<u8>), Json(serde_json::Value) }
```

- Driver wire formats decode **directly into `RowMap`/`DbValue`** â€” no
  intermediate JSON tree, no double heap allocation per row.
- `serde_json::Value` is used only at the JSON/HTTP/Kafka/CLI boundary and only
  with `preserve_arbitrary_precision` semantics (Â§15, Â§31).
- `DbValue::Int(i128)` gives exact big-int fidelity for `BIGINT`, Mongo `Long`,
  DynamoDB N-with-int-range; out-of-`i128` numbers become `Text` rather than
  being truncated.
- **Connection construction**: adapters build their pool eagerly in
  `connect(config) -> Result<Self>` (pool stored in `OnceLock`/`Arc<Pool>`
  inside the adapter). All trait methods take `&self`; no `RwLock`, no `&mut`
  anywhere on the shared interface. Lifetime of the adapter is owned by the
  caller.
- Prepared statements are compiled at connect time where the driver supports
  it; dynamic SQL paths (search, sort) still use parameterized values only.

### 7.2 Adapter-Specific Requirements

Each adapter must support:

- Connection config documented in skill reference.
- Correct SQL dialect and placeholder style.
- Upsert conflict handling.
- Soft delete filtering.
- Pagination/sorting.
- Type coercion (numeric, boolean, JSON).
- `RETURNING`/`OUTPUT` fast paths where supported:
  - PostgreSQL/CockroachDB/SQLite3: `RETURNING *`
  - MSSQL: `OUTPUT inserted.*`
  - Others: re-fetch by PK.

### 7.3 Driver Crate Matrix (pinned)

Chosen to avoid conflicting async runtimes and unmaintained crates; all are
tokio-compatible unless noted (SQLite may use `spawn_blocking`):

| Adapter(s) | Driver crate | Notes |
|---|---|---|
| PostgreSQL / CockroachDB | `sqlx` (`postgres` feature, rustls-tls) â€” or `tokio-postgres` + `deadpool-postgres` | `RETURNING *`-capable; cached column types needed for numerics |
| MySQL / MariaDB | `sqlx` (`mysql`, rustls-tls) â€” or `mysql_async` (tokio-native, async transactions native) | chunked inserts >999 rows; timezone forced UTC |
| SQLite3 | `sqlx` (`sqlite`, bundled, WAL) â€” or `rusqlite` + `tokio::task::spawn_blocking` | sync driver; savepoints for nested tx; faster for embedded workloads |
| MSSQL | `tiberius` (pure Rust, TDS) + `bb8-tiberius` | no async runtime conflict; `OUTPUT inserted.*` |
| Oracle | `oracle` (rust-oracle; OCI driver) + `spawn_blocking` | requires Oracle Instant Client; not available in all CI |
| MongoDB | `mongodb` (official) | `$set`/`$unset`, ObjectId, bulk unordered writes |
| Redis | `redis` crate with `aio` (connection manager) â€” **or** `fred` (pin one) | hash keys `{table}:{pk}`; O(N) scans for filters |
| DynamoDB | `aws-sdk-dynamodb` | `BatchWriteItem` 25/batch; `contains` filters |
| Kafka (optional) | `rdkafka` with `cmake-build` for CI reliability; `dynamic-linking` optional for fast local builds | buffered producer, Â§25 |

Guidance:

- Prefer `sqlx` uniformly for SQL where possible to collapse feature-flag
  complexity; drop to driver-specific crates only for `MSSQL`, `Oracle`, and
  (optionally) SQLite synchronous performance.
- TLS: rustls by default; document OpenSSL fallback for enterprise Oracle/MSSQL
  setups.
- Do **not** let driver runtime requirements leak into `dbmr-core` trait
  definitions; the trait layer is runtime-agnostic.
- Minimum maintenance standard: each driver choice must have active releases
  and no known unsound unsafe blocks; verify at pin time.

### 7.4 Adapter Test Matrix

| Adapter | Unit tests | Integration tests | Property tests |
|---|---:|---:|---:|
| MySQL | yes | yes | yes |
| MariaDB | yes | yes | yes |
| PostgreSQL | yes | yes | yes |
| SQLite3 | yes | yes | yes |
| MongoDB | yes | yes | yes |
| MSSQL | yes | yes | yes |
| CockroachDB | yes | yes | yes |
| Oracle | yes | yes | yes |
| Redis | yes | yes | yes |
| DynamoDB | yes | yes | yes |

---

## 8. HTTP Route Requirements

**Requirement ID:** FR-HTTP-001

Route generation shall produce exactly these endpoints. The router must be
mounted with parameter inheritance enabled (Express `mergeParams: true`) so
nested/parent routers expose `:id`/`:pk`.

| Method | Path | Action |
|---|---|---|
| GET | `/:<pk>` | Get by PK |
| QUERY | `/:<pk>` | Query by PK (body/query driven) |
| POST | `/:id` | Insert single |
| PUT | `/:id` | Update single |
| PATCH | `/:id` | Partial update |
| DELETE | `/:id` | Delete single |
| GET | `/` | List |
| QUERY | `/` | List/filter body-driven |
| POST | `/` | Bulk insert |
| PUT | `/` | Bulk update |
| DELETE | `/` | Bulk delete |

**Important:** the single-insert route is `POST /:id`, not `POST /add`. The
literal string `/add` simply matches `:id` with `id = "add"`. Implementations
must reproduce this: a request to `POST /<anything>` is a single insert whose
primary-key value comes from the path segment.

Behavior:

- Payload override for multi-tenancy (`{ tenant_id: "user.tenant_id" }`).
- Formatted output: JSON, CSV, XML via `output_content_type`.
- HTTP `QUERY` method support with raw-body fallback parsing.
- Access validation before update/delete (record must exist).
- Single-record mutations return **HTTP 200**, never 201.
- 404 uses the exact body `{"message":"Not Found","type":"danger"}`.
- Validation failures return 422 with `{"type":"danger","message":"<space-joined errors>"}`.
- See Â§20 for the full status/body/envelope contract.

**Framework support:**

- Primary: Axum (recommended).
- Optional: Actix-web behind feature flag.
- Router trait abstraction to allow both.

**Tests:**

- Route registration.
- Handler behavior with mock adapter.
- Query method fallback.
- Multi-tenancy override.
- Output formatting.

---

## 9. CLI and Schema Requirements

### 9.1 CLI Commands

The Rust CLI shall support:

- `init`
- `inspect`
- `doctor`
- `diff`
- `db-manager`
- `types`
- `migrate:create`
- `migrate:up`
- `migrate:down`
- `migrate:status`
- `encrypt:scan`
- `encrypt:rotate-key`
- `help`

Universal flags:

- `--json`
- `--dry-run`
- `--no-install`
- `--help`

### 9.2 Schema-Driven Generation

`dbmr.schema.json` support:

- adapter
- framework
- options
- tables
- columns
- pk
- unique
- softDelete
- timestamps
- search_columns
- parent
- encrypted rules

Generated artifacts:

- app entrypoint
- db init
- migration runner
- route mounting
- model files
- route files
- tests
- OpenAPI spec
- SaaS structure when enabled

### 9.3 SaaS Structure

When `saasStructure: true`, generate:

- tenants, users, roles, role_permissions, webhooks, webhook_logs
- auth middleware
- tenant isolation
- permission middleware
- password utilities
- webhook utilities
- seeds
- consolidated migrations

**Tests:**

- Golden-file generation tests.
- Schema validation tests.
- Parent nesting tests.
- Doctor sync tests.
- Diff preview tests.

---

## 10. Database Management UI

**Requirement ID:** FR-DBM-001

Optional `db-manager` web dashboard:

- Table browser with filtering, sorting, pagination, inline editing.
- Raw SQL query editor with CSV export.
- Query history.
- Dashboard overview stats.
- Light/dark/system themes.

This may be implemented as a separate crate and feature to keep core lean.

---

## 11. Type Generation

**Requirement ID:** FR-TYPES-001

`types` command shall generate TypeScript interface definitions by default,
matching the source CLI's observable output. The source implementation writes
`types/models.d.ts` (overridable with `--out`) and prints to stdout with
`--stdout`; it does **not** emit Rust.

For the Rust rewrite the baseline requirement is:

1. **Parity mode (required):** emit the same TypeScript definitions the Node
   CLI emits, including interface names, optional markers, and rule-to-type
   mapping, so existing consumers keep working.
2. **Rust mode (optional, opt-in flag such as `--lang rust`):** additionally
   emit structs with serde derives.

The BRD treats TypeScript output as the compatibility contract; Rust output is
an additive feature and must not change default behavior.

---

## 12. Testing and Coverage Requirements

### 12.1 Coverage Target

**Primary target â€” test-case coverage (>95%).** The project's headline metric is
*test-case coverage*, defined as:

```text
test-case coverage =
  (number of source test cases with a mapped, passing Rust test)
  / (total number of source test cases)
```

A "source test case" is one `it(...)` block in `db-model-router/test/**/*.test.js`.
A "mapped Rust test" is one or more `#[test]`/`#[tokio::test]` functions
referenced by the traceability matrix in Â§33 that assert the same observable
behavior (not necessarily the same implementation). Fixture-only, helper, and
`before/after` hooks are not counted as test cases.

The baseline inventory is **117 test files**; the exact case count must be
captured by an extraction script (`rg -c "it\(" test/`) and committed as
`tests/traceability.json` so the denominator is auditable rather than guessed.

**Secondary targets â€” code coverage (gates, not the headline metric):**

| Scope | Line coverage | Branch coverage |
|---|---:|---:|
| `dbmr-core` (model, filter, validation, encryption, tx) | â‰¥ 95% | â‰¥ 90% |
| `dbmr-http` | â‰¥ 95% | â‰¥ 90% |
| `dbmr-schema` | â‰¥ 95% | â‰¥ 90% |
| `dbmr-cli` generators | â‰¥ 85% | â‰¥ 80% |
| Each adapter crate | â‰¥ 90% | â‰¥ 85% |
| Whole workspace (excluding UI static assets) | â‰¥ 95% | â‰¥ 90% |

The previous wording that mixed "95% core / 90% adapters / 85% CLI" with an
"overall >95%" gate was ambiguous. The gates above are per-scope and are not
substitutes for the test-case coverage target. A scope may not fail its gate
because another scope is above target.

CI must publish both numbers. Test-case coverage is computed from the
traceability matrix, not from `cargo llvm-cov`.

### 12.2 Test Types

1. **Unit tests**
   - Model operations with mock adapters.
   - Filter parsing and expansion.
   - Validation.
   - Encryption.
   - Route handlers.
2. **Integration tests**
   - Real database containers.
   - Adapter-specific SQL dialects.
   - Transactions.
3. **Property tests**
   - Filter generation.
   - Round-trip serialization.
   - Schema validation.
   - Encryption round trips.
4. **Golden tests**
   - CLI generated files.
   - OpenAPI output.
5. **Compatibility tests**
   - Compare outputs with source JS library where feasible.

### 12.3 Coverage Exclusions

Only the following may be excluded from coverage:

- UI static assets.
- Generated third-party code.
- Platform-specific code paths unavailable in CI.

All exclusions must be documented in `coverage.toml` or CI config.

### 12.4 Security Test Cases (Normative â€” Every Adapter)

For each of the 10 adapters (and the `dbmr-http` surface) the following
malicious/edge inputs must have explicit tests, mirroring the Node
`test/adapters/*.security.test.js` suites (~20 files total):

- SQL injection via **values** (parameterized; must never reach the wire as
  SQL text) â€” include payloads like `' OR '1'='1`, `"; DROP TABLE--`.
- SQL injection via **identifiers** (table/column) â€” `users"); DROP TABLE--`,
  backtick/bracket/quote variants; both must be rejected or escaped per adapter
  (Â§32).
- MongoDB operator injection â€” `{$where: 1}`, `{"$gt": ""}` in filter values,
  nested `$`-prefixed keys; expected to produce no matches, never evaluate
  server-side JS.
- Overflow length inputs (e.g. 1 MB strings, deep JSON nesting â‰¥ 256 levels)
  must not crash or stack-overflow the parser.
- ReDoS-class inputs on `regex:` columns and search strings.
- Prototype-pollution/`__proto__`/`constructor` keys in filter JSON and merge.
- Mass-assignment: unknown columns stripped even when `required` omitted.
- Mass-delete: empty-filter delete must be rejected at model layer (Â§32 / Â§34 D15).
- Encryption tamper: flip one bit of a GCM envelope â†’ tag error, never plaintext;
  wrong key version â†’ clear error; `enc:v999:...` with correct known version set
  â†’ precise failure message.
- Kafka disabled â†’ zero events emitted; enabled â†’ events after commit only.
- Multi-tenancy override: client cannot inject `override.$router`/`$framework`;
  `$`-keys rejected from request payload; literal `"null"` stored verbatim
  (fixed, ADR-0009 D8).
- `db-manager` (if shipped): binds only to configured host; raw SQL endpoints
  gated; history opt-in; no hot-reload path exposes stack traces in production.
- Secrets: encryption keys/passwords never appear in logs (test with tracing
  subscriber capture); `errorResponse` in `NODE_ENV=production` never prints
  `sqlMessage`.
- Boundary values: `size=200/201`, `page=-1`, `limit` aliases, i64 overflow
  fields, decimal out-of-range, date `9999-12-31`, empty string in required
  boolean.

### 12.5 Null / Undefined / Default Behavior Test Cases (Normative)

Every public function/op must have explicit tests for Â§37 semantics, including
but not limited to:

- `null` vs `"missing"` vs `""` vs `0` vs `false` vs `"null"` for every input
  field (filter key, PK argument, hook return, override resolution).
- `NaN`/`Infinity`/negative in `page`, `size`, `limit` (strings and numbers).
- Empty input shapes: `{}`, `[]`, `{ data: [] }`, `{ filter: [] }`, `null`
  whole body.
- Hook return values: `undefined`, non-object (number/string/array), throws.
- `afterFind` empty array return; `beforeFind` non-array return; `beforeDelete`
  `undefined` return.
- Fields with defaults: every **Default** row in Â§37 must have at least one
  positive and one negative test.
- Timestamps: strip removes every known variant (Â§6.2); `option.created_at`
  override narrows the set; DB default-supplied value present on read-back.
- JSON round-trips: big-int `2^53`, `9223372036854775807`, >`u64`; decimal
  precision; embedded `"null"` inside JSON strings.
- `insert` returning `id` but refetch missing â†’ `null` (and no Kafka event) â€”
  Â§21.2; `update` fallback paths (`record` â†’ id refetch â†’ payload-PK refetch â†’
  raw adapter result).
- Default-value equality tests for `expect_*` of parser/CLI defaults (golden).

---

## 13. Migration Requirements

- Timestamped migration files.
- `up`, `down`, `status`.
- SQL migrations for SQL adapters.
- Scripted migrations for NoSQL adapters.
- Transactional migration application where supported.
- Rollback by batch.

**Tests:**

- Create/apply/rollback/status.
- Pending migration detection.
- Failure rollback.

---

## 14. Security Requirements

- Parameterized queries only; no SQL string concatenation for values.
- Encryption keys never logged, and key material zeroized after use where the
  crypto backend allows it.
- Field-level encryption uses AES-256-GCM with a random 12-byte IV and a
  16-byte authentication tag. **No additional authenticated data (AAD) is used
  in v1**; the version tag is carried in the envelope prefix, not as AAD. Any
  AAD support is a future, explicitly versioned envelope change.
- Tamper detection: an authentication failure on decrypt is a hard error, never
  a silent plaintext fallback.
- Safe handling of invalid input (422 errors).
- Multi-tenancy override cannot be bypassed by client payload.
- Secrets loaded from environment or key files.
- Identifier quoting and null-byte stripping per adapter (Â§32).
- Prototype-pollution resistance for `__proto__`/`constructor` keys (Â§32).
- See Â§32 for the expanded security contract, including the db-manager
  exposure and mass-delete risks that must be decided before implementation.

---

## 15. Performance Requirements

- Eliminate redundant round trips using `RETURNING`/`OUTPUT`.
- Async I/O throughout; SQLite synchronous driver wrapped in `spawn_blocking`
  only where a full async stack is not desired.
- Connection pooling per adapter; pools built eagerly at `connect()` (Â§7.1).
- **No intermediate JSON trees on the hot path:** driver rows decode directly
  into `RowMap`/`DbValue` (Â§7.1); `serde_json::Value` appears only at the
  HTTP/Kafka/CLI boundary, with `arbitrary_precision` for wire fidelity (Â§31).
- **No `Mutex` on read paths:** global handle in `OnceLock`/`arc_swap`;
  per-request state passed by `Arc`/reference.
- Model rules compiled once at construction (Â§6.2); the hot path iterates
  slices/bitsets, never strings.
- **Zero-copy bodies:** QUERY/JSON fallback uses `bytes::Bytes`; no clone of
  the request buffer in the malformed-JSON fallback.
- Batched Kafka production; no per-row allocations for event envelopes
  (serialize straight into the producer buffer).
- Avoid unnecessary cloning in hot paths (move `RowMap` rows, borrow in
  validators).
- Prepared statements compiled at connect time where the driver supports it.
- Benchmarks comparable to or better than Node.js version (keep `benchmark/`
  scenarios: Postgres reports exist; add Rust-vs-Node comparison targets).
- **Hard requirement â€” differential benchmark vs Rust ORMs on PostgreSQL**,
  specified in Â§38; must run in CI at least nightly and gate regressions.

---

## 16. Risks and Mitigations

| Risk | Mitigation |
|---|---|
| Adapter behavior differences | Integration tests per adapter |
| Large CLI scope | Phase delivery and golden tests |
| Coverage gaps | Coverage gates in CI |
| Global state complexity | Introduce explicit context with optional global shim |
| Encryption compatibility | Shared test vectors |
| Kafka optional dependency | Feature flags |

---

## 17. Phased Delivery Plan

### Phase 1: Core

- Core traits
- Model operations
- Filters
- Validation
- Encryption
- Transactions
- SQLite + PostgreSQL adapters

### Phase 2: Adapters

- MySQL, MariaDB, CockroachDB, MSSQL, Oracle, MongoDB, Redis, DynamoDB

### Phase 3: HTTP and CLI

- Route generation
- Schema parser
- Generators
- Migrations
- Types

### Phase 4: Advanced

- db-manager
- SaaS scaffolding
- Kafka
- Coverage hardening

---

## 18. Acceptance Criteria

The rewrite is accepted when:

1. **Model parity.** Every method in Â§21 returns the documented shape,
   including the unified `Option<Record>` missing semantics, `count` semantics,
   and the `"N row(s) removed"` string (deviation ADR-0009, D3).
2. **All 10 adapters pass integration tests** against real or containerized
   databases, plus the per-adapter contract matrix in Â§27.
3. **HTTP contract parity.** Every endpoint in Â§20 returns the documented
   status code and body for success, not-found, validation failure, and
   internal failure; CSV and XML output match Â§20.5 byte-for-byte for fixture
   data.
4. **CLI parity.** Each command in Â§29 accepts the documented flags, produces
   the documented default paths, and passes golden-file tests. `init` produces
   a runnable project; `--dry-run` writes nothing.
5. **Schema round-trip.** Parse â†’ normalize â†’ print â†’ parse is idempotent and
   byte-stable per Â§28.7, and every documented validation rule in Â§28.2 has a
   negative test.
6. **Encryption and rotation** match the envelope, key-resolution, and
   serialization rules in Â§24, including cross-implementation test vectors so a
   Rust-written value decrypts under the Node library and vice versa.
7. **Transactions** commit/rollback correctly, buffer Kafka events until commit,
   and reject unsupported adapters with the exact source message.
8. **Section 12.1 numbers:** test-case coverage > 95% and all per-scope code
   coverage gates met, both published by CI.
9. **No reproduced upstream defects.** Every item in Â§34 has either a fix or an
   explicit, documented decision to preserve it.
10. README/examples are updated for Rust usage, and the migration/compatibility
    story for existing Node consumers matches the decision in Â§35.
11. **Documentation of deviations.** Any behavior that intentionally differs is
    listed in `docs/deviations.md` with rationale and a migration note.

---

## 19. Traceability

This BRD is derived from:

- `README.md`
- `skill/SKILL.md`
- `types/index.d.ts`
- `src/commons/model.js`
- `src/commons/route.js`
- `src/index.js`
- adapter and CLI source modules
- test suites under `test/`

Every requirement above should map to at least one automated test in the Rust workspace.

**Source-of-truth rule:** whenever behavior is ambiguous, the **Node
implementation at `C:\Workspace\eForge\db-model-router` is authoritative**
(file/line references: `src/commons/model.js`, `src/commons/route.js`,
`src/commons/validator.js`, `src/commons/function.js`,
`src/commons/encryption.js`, `src/commons/kafka.js`,
`src/commons/migration-runner.js`, `src/index.js`, per-adapter `src/*\db.js`,
`src/cli/`, `db-manager/routes/api.js`, `test/**/*.test.js`,
`benchmark/*`). Reviewers must verify against these files rather than
against this document alone; behavior that genuinely cannot be ported (e.g.
dynamic `require()` of JS migrations) is recorded in `docs/deviations.md`.

---

## 20. Normative Contract: HTTP Routing and Responses

This section is **normative** for any HTTP surface the Rust rewrite exposes.

### 20.1 Mounted router rules

- The router must parameter-inherit (Express `mergeParams: true`
  equivalent), so a router mounted at `/api/users` still matches `:id`.
- `QUERY` (RFC 4791) is a first-class method.

  **Axum-specific registration (normative).** Axum's `MethodFilter` covers only
  the standard methods (GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS, TRACEâ€¦)
  â€” there is no `MethodFilter::QUERY`, and `MethodFilter::try_from(
  Method::from_bytes(b"QUERY").unwrap())` returns `Err(NoMatchingMethodFilter)`.
  `Method::from_bytes(b"QUERY")` itself is valid and hyper routes it fine.
  Register a custom QUERY route by one of:
  1. `.route(path, method_router.fallback(query_handler))` where
     `query_handler` checks `req.method() == Method::from_bytes(b"QUERY")`
     and otherwise returns 405; or
  2. a Tower middleware on the route that forwards only `QUERY` into a
     mounted sub-router; or
  3. `Router::method_not_allowed_fallback` + manual method gate.

  For actix-web: register `.service(web::resource().custom("QUERY", ...))` /
  `guard::Method(Method::from_bytes(b"QUERY").unwrap())`. Do **not** ship the
  Node-style `all()` catch-all blindly â€” it must be method-gated.
- `QUERY` body fallback: if `req.method === "QUERY"` and the body is
  missing/empty/not an object/no keys, and `req` is a readable stream, the raw
  body is collected and `JSON.parse`d. Malformed JSON is **ignored**
  (the handler runs with the prior body); it is not a 400.
  In Axum, collect via `bytes::Bytes` (or an axum body stream) â€” the fallback
  must not clone the request buffer.
- Route handler context passed to model hooks/operations is
  `{ req, user: req.user }`.

### 20.2 Endpoints, inputs, and outputs

For single-record mutations, `id` is the last path segment; it is **always**
the target PK value for `PUT`/`PATCH`/`DELETE`, and is **discarded** for
`POST /:id` (the path id does not become the PK; the PK is stripped from the
create payload). For `GET`/`QUERY` by id, the path segment is the PK.

For bulk operations the body must be `{ "data": [...] }`.

| Endpoint | Calls | Success status | Success body |
|---|---|---|---|
| `GET /:<pk>` | `model.find(payloadWithPk)` then index 0 | 200 | record; 404 if `count == 0` |
| `QUERY /:<pk>` | `model.find(payloadWithPk)` | 200 | record; 404 if `count == 0` |
| `POST /:id` | `model.insert(payload)` | 200 | record (or bulk summary if body has `data`) |
| `PUT /:id` | existence via `model.findOne` then `model.update` | 200 | record/summary; 404 if pre-check misses |
| `PATCH /:id` | existence via `model.findOne` then `model.patch` | 200 | record or `null`; 404 if pre-check misses |
| `DELETE /:id` | existence via `model.findOne` then `model.remove` | 200 | adapter remove summary; 404 if pre-check misses |
| `GET /` | `model.list(query+params)` | 200 | `{ data, count }` |
| `QUERY /` | `model.list(query+body)` | 200 | `{ data, count }` |
| `POST /` | `model.insert({ data })` | 200 | `{ rows, message, type, id? }` |
| `PUT /` | `model.update({ data })` | 200 | `{ rows, message, type, id? }` |
| `DELETE /` | `model.remove(payload)` | 200 | adapter remove summary |

Notes:

- **`PUT`/`PATCH`/`DELETE /:id` pre-check** uses
  `model.findOne(payloadOverride({}, req, override) with pk = param)`, so the
  override applies to the existence check exactly as to the mutation. A miss
  is `404 {"message":"Not Found","type":"danger"}` and the model is **not**
  called for the mutation.
- **Payload assembly.** `GET` by id: `{ ...query, ...params }`; `QUERY` by id:
  `{ ...query, ...body, ...params }`; list `GET`: `{ ...query, ...params }`;
  list `QUERY`: `{ ...query, ...body }`. For by-id, `params` wins; for list,
  `body` wins over `query`. After assembly, `payloadOverride` runs, then
  reserved params are extracted (Â§20.4), then `search` is re-attached.
- **`GET`/`QUERY` by id use `model.find`, not `byId`.** Consequently the
  `byId` 422 path is reachable only via the model API, and single-GET by id is
  not id-shape-validated.
- `PUT /:id` sets `payload[model.pk] = req.params.id` **after** the override,
  so the path segment always wins for the PK on single update/patch/delete.
- `POST /:id` does `delete payload[model.pk]`, so the PK from the path is
  never inserted.

### 20.3 Error envelope

`errorResponse(res, err)` â€” **as fixed in ADR-0009 (D2)**: the rewrite reads
HTTP status only from the typed `AppError` enum (each variant carries
`StatusCode`); the Node `err.cause.status` stringly-typed path is not
implemented. Observable body contract is unchanged:

- If `400 <= status < 500`: body `{ type: "danger", message: err.message || "Bad request" }`.
- Else (5xx / unknown): in `NODE_ENV` of `development`, `test`, or `TEST`,
  `message = err.sqlMessage || err.message || "Internal server error"`;
  otherwise `message = "Internal server error"`.
- Always `{ type: "danger", message }`. Never a bare string.

Other exact statuses returned by handlers:

- `422` `{ type: "danger", message }` where message is the space-joined
  validator error messages (e.g. `"The name field is required."`), or
  `"Invalid filter Inputs"` from the filter parser, or
  `"Primary key is required for patch"`.
- `400` `{ type: "danger", message: "Request body must contain a 'data' array" }`
  for bulk `POST`/`PUT /`.
- `400` `{ type: "danger", message: "Request body must contain filter criteria" }`
  for `DELETE /` with an empty object body.
- `404` `{ message: "Not Found", type: "danger" }` (note: this one omits
  `type` ordering is irrelevant; it has both keys).
- Adapter-derived invalid filter/operator errors surface as `500`.

Rust mapping: structured `AppError { status: u16, message: String }` with a
single `IntoResponse`; do not clone the `err.cause.status` footgunâ€”read status
from the typed error.

### 20.4 Reserved request parameters

`select_columns`, `output_content_type`, `search`, `page`, `size`, `limit`,
`sort` are reserved and extracted from the payload before it is used as a
filter:

- `select_columns`: comma-separated string **or** array of column names.
- `output_content_type`: `csv` | `xml` | other (sends JSON 200).
- `search`: string; whitespace-only is ignored.
- `page`: integer default `0`; NaN/negative â†’ `0`.
- `size`/`limit`: integer default `30`, minimum `1`, maximum `200`; `limit`
  is an alias for `size`.
- `sort`: see Â§22.6.
- `select_columns` is applied **after decryption and before hooks** on the
  model. For `GET` by id the route also applies it a second time only if it
  had not been extracted.

For `find`/`findOne`/`list`, after extraction the remaining keys must form a
valid filter (Â§22). `page`, `size`, `limit`, `sort` are removed before
filtering.

### 20.5 CSV and XML output

Only list/by-id **GET-family** endpoints may emit CSV/XML (`output_content_type`
= `csv` or `xml`). All mutation responses are JSON.

CSV (`text/csv`, HTTP 200, no `Content-Disposition`):

- Rows = array data, else `data.data`, else `[data]`.
- Header = keys of the **first row only**.
- Lines separated by `\n`. Values quoted only if they contain `,`, `"`, or
  newline; embedded quotes doubled. Object values are JSON-stringified.
- Values are stringified with `String(v)` (i.e. JS semantics, e.g. `null`/
  `undefined` â†’ the **empty string**? no â€” `String(null)` = `"null"`,
  `String(undefined)` = `"undefined"`; implement exactly this).

XML (`application/xml`, HTTP 200, no `Content-Disposition`):

- Root `<records>`, child `<record>`.
- Escapes only `&`, `<`, `>`, `\"` (not `'`).
- Object values JSON-stringified.

These behaviors are bug-compatible and must be documented as such (see Â§34).

### 20.6 Payload override (`payloadOverride`)

For every routed endpoint, before the model is invoked, the payload is passed
through the model's route override:

- Applied to a top-level object **or element-wise to an array**.
- For each key in the override object: if the key starts with `$`, skip it.
  Otherwise set `payload[key] = getByPath(req, override[key])`, or `""` if
  the path does not resolve.
- `getByPath` supports dotted paths from `req` (e.g. `user.tenant_id`,
  `headers.x-tenant`).
- Reserved override keys `$express`, `$router`, `$framework` are consumed for
  framework resolution and also skipped here because of the `$` prefix.
- **Deviation (ADR-0009, D8):** literal `"null"` strings are **stored verbatim**
  and are *not* deleted. The Node rule ("delete any string-valued `'null'`") is
  removed. Missing override paths still resolve to `""`.
- Overrides apply to **all** payloads including reads and bulk row batches.

### 20.7 Framework and router resolution order

1. Explicit router object: `options.router` â†’ used as-is (must have
   `get/post/put/patch/delete`/`query`).
2. Explicit express module: `override.$express`.
3. Framework selection: `override.$framework ?? process.env.DBMR_FRAMEWORK ?? "express"`;
   if `"ultimate-express"`, try it first, else fall back to `express`.
4. If neither `express` nor `ultimate-express` is installed, throw
   `"Neither 'express' nor 'ultimate-express' is installed."` with the
   underlying load error as `cause`.

The Rust implementation should prefer Axum, but to satisfy parity option
`$framework`/`DBMR_FRAMEWORK`/`ultimate-express` must be mapped:
`ultimate-express` selects the Actix/high-perf feature; `express` selects the
default (Axum) integration. Both must support QUERY.

---

## 21. Normative Contract: Model Operations

`model(db, table, structure, primary_key, unique, option)` (plus the
legacy-string first-argument form using the global `db`). Rust API names may
differ (snake_case), but the shapes and rules below are binding.

### 21.1 Stripping and ordering rules (all write paths)

For every insert/update/upsert (single and bulk), **in this order**:

1. `beforeSave` hook (whole payload for bulk: per-row via `data` array).
2. `beforeCreate` / `beforeUpdate` / `beforeUpsert` hook (applies to the same
   payload object).
3. Remove `primary_key` from each row **only on create paths**:
   - `insert`: always (single and bulk).
   - `upsert`: **not** removed â€” upsert uses the PK to decide insert vs update.
   - `update`: PK is **kept** (validated).
   - `patch`: PK is required and kept.
4. Strip timestamp fields (`created_at`, `createdAt`, `created`,
   `create_date`, `createDate`, `creation_date`, `creationDate`; same for
   `modified_*` plus `updated_at`, `updatedAt`, `updated`, `update_date`,
   `updateDate`, `modification_date`, `modificationDate`) from each row.
   Overrides: `option.created_at` / `option.modified_at` (string or string[])
   **replace** the default variant lists.
4a. **(ADR-0009, D19) Strip the soft-delete column** from client-write HTTP
   payloads (and exclude it from bulk UPDATE validators). The column remains
   server-managed. In the pure model API it may only be set via
   `option.safeDelete`/explicit repository calls, never from route payloads.
5. Validate (see Â§21.3).
6. Remove unknown columns (`RemoveUnknownData`): keys not present in
   `modelStructure` are silently dropped (single: via array wrap; bulk: direct).
7. Encrypt encrypted fields (Â§24).
8. `jsonStringify` the payload (object columns serialized, big-int safe).
9. Call adapter `insert`/`upsert`.

Bulk detection: any payload object with an own property `data` is treated as
bulk and `data` must be an array. A model column literally named `data`
therefore makes single records indistinguishable from bulk (documented
ambiguity â€” preserve and flag).

**On single insert/update/upsert only**, unknown-column stripping is applied
to an **array wrapping** the single object (`RemoveUnknownData(modelStructure,
[data])`) â€” irrelevant semantics, note for golden tests.

### 21.2 Return shapes

Single operations:

- `insert(x)`:
  1. `beforeSave` / `beforeCreate`.
  2. Validation (Â§21.3, `CREATE` validator, PK excluded).
  3. `db.insert`. Result `{ rows, message, type, id? }`.
  4. If result has `record`, use it; else if `id` truthy, re-fetch by PK with
     `{ count: false, limit: 1 }`; **if the re-fetch returns nothing, the
     function returns `null` and emits no Kafka event and skips After hooks.**
  5. On success: decrypt, `afterSave`, `afterCreate`, emit Kafka `insert`,
     return the **full decrypted record** (object), not the adapter summary.
- `update(x)`: like `insert` but `UPDATE` validator, PK **included** in
  validation, no create-path PK strip; calls `db.upsert`; on success returns
  the record (from `result.record`, or re-fetch by `id`, **or re-fetch by the
  payload's own PK**). Else falls back to returning the raw adapter result.
- `upsert(x)`: as `update`, but uses the `CREATE` validator (PK excluded) and
  runs `beforeUpsert`/`afterUpsert` in addition to update hooks.
- `patch(x)`: `beforeSave`/`beforeUpdate` (as single object, `runItemHook`),
  strip timestamps, require PK (`422 "Primary key is required for patch"`),
  validate **only the supplied keys** with `required` removed (a column with
  no remaining rules is not validated), fetch existing record (decrypt),
  merge known model keys over existing (unknown keys silently dropped),
  encrypt, `db.upsert`, re-fetch and return the decrypted record, else
  `null`. If existing count == 0 â†’ return `null` (404 at route layer).
- `remove(x)`: `beforeDelete` may replace the argument; builds a filter via
  Â§22 from `jsonSafeParse(data)`; calls `db.remove(table, filter,
  option.safeDelete)`; `afterDelete` return value ignored; returns the
  adapter's `{ message }` object; emits Kafka `delete` with the original
  (string-parsed) data.
- `byId(id, options?, context?)`: `null`/`undefined`/`""` â†’ 422; non
  string/number â†’ 422. Runs `beforeFind`; get with `{ count: false, limit:
  1 }`; missing â†’ `null`; else decrypt, apply `select_columns`, run
  `afterFind` (takes `h[0]` if non-empty). `options` with `select_columns` is
  the options object; otherwise it is the context.
- `find(x)`: extract reserved (Â§20.4), sort normalize, `dataToFilter`, apply
  search, `beforeFind` may replace filter, get (with count), decrypt, apply
  `select_columns`, `afterFind` may replace data array. Returns `{ data, count }`
  where **`count` is the total matching rows**, not `data.length`.
- `findOne(x)`: same, limit 1, `count: false`; missing â‡’ **`None`**
  (deviation ADR-0009, D3: Node returns `false`; unified to `Option<Record>`).
- `list(x)`: same plus pagination (defaults page 0 / size 30 / max 200;
  NaN/negative â†’ defaults; `limit` alias). Returns `{ data, count }`.
  **`find` has no pagination; `list` does.**
- `withTx(tx)`: clones the model bound to a transaction-scoped adapter and a
  Kafka buffer; exposes `pk`, `table`, `modelStructure`, `hooks`.

Bulk operations:

- Detection: own property `data` whose value may or may not be an array;
  validated as `required|array`.
- `insert({ data })`: PK stripped on all rows, timestamps stripped,
  `CREATE` bulk validator (`data.*.<col>` per model column except PK),
  unknown keys dropped, encrypted, `db.insert`. Returns the adapter bulk
  result `{ rows, message, type, id? }` (`id` only when total == 1).
- `update({ data })`: no PK strip, timestamps stripped, `UPDATE` bulk
  validator (all rows must carry every required column **and** the PK â€” note
  `UPDATE` validator includes the PK in `data.*`), unknown dropped,
  `db.upsert`.
- `upsert({ data })`: as insert but keeps PK, CREATE bulk validator.
- `remove(x)`: as single.
- Bulk after-hooks are called **once** with the adapter result and their
  return values are ignored; bulk `beforeSave`/`beforeCreate`/etc. are
  applied **per row**.

### 21.3 Validators

For non-bulk: `CREATE` validates every structure rule except PK; `UPDATE`
validates every rule including PK; `PATCH` validates only present keys minus
`required`. Bulk: `data` = `required|array` plus `data.*.<col>` rules.

The validation pre-pass strips the internal `encrypted` token
(`buildValidatorStructure`) because the validator cannot parse compound
flags.

`validateInput` merges all validator errors into one space-separated
message and throws `422` (`"The <field> field is required. ..."` style,
node-input-validator messages).

### 21.4 `select_columns`

Comma-separated string or array, applied to each decrypted row. Missing keys
are simply absent. For single responses `applySelect` is also applied by the
route when extraction happened there.

### 21.5 `search`

- Reserved; extracted and removed before filtering; empty/whitespace â†’ ignored.
- Requires `option.search_columns` non-empty, else silently dropped.
- Cross-product expansion: `search` term is combined with **every** configured
  search column as `[col, "%...%"?]` â€” note: the model does **not** wrap the
  term; adapters decide wildcard semantics (Â§22.5).
  (Exact semantics: the term is passed verbatim; see adapters.)

### 21.6 `withTx`

Clones the instance with the transaction adapter and a private Kafka buffer
(`option._kafkaBuffer`); same table/structure/hooks. Used by
`db.transaction(fn)` (Â§26) â€” `tx.model(modelOrTable)` accepts a registered
model instance or a table name (resolved against the global model registry).

---

## 22. Normative Contract: Filters

### 22.1 Filter shape

`[ OR_group [ AND_condition [col, op, value], ... ], ... ]` â€” an array of
OR groups; each group is an array of AND conditions. Single-element arrays
are normalized to one group.

- `isEmptyFilter`: `null`, `[]`, `[[]]`, `[[[]]]` â†’ match-all. Mutated/shared
  arrays are the caller's concern.
- Empty safeDelete: adapters add `[safeDelete, "=", 0]` to **each OR group**,
  producing `WHERE (... AND safeDelete = 0) OR (...)`. For boolean Postgres
  columns the literal is `FALSE`.

### 22.2 `objectToFilter`

One AND condition per key, all in one OR group. Non-string values (numbers,
booleans, objects, null) are emitted as `[col, "=", value]` with the original
value type preserved (Â§22.5/Â§22.7 cover NULL semantics).

### 22.3 `dataToFilter(data, pk)` â€” exact decision table

| Input | Output |
|---|---|
| `null`, `undefined`, `""` | **422** `"Invalid filter Inputs"` |
| `number` or `string` | `[[[pk, "=", input]]]` |
| array, length 0 | `[[[]]]` |
| array, all primitives (string/number) | `[[[pk, "in", input]]]` |
| array of objects, every object has exactly the PK and nothing else | `[[[pk, "in", ids]]]` |
| array of objects (otherwise) | `[ objectToFilter(item)[0], ... ]` (OR of groups) |
| `{ data: [...] }` | recurse on `data` |
| `{ filter: [...] }` plus other keys | `filter` JSON-cloned; each original OR group extended with the `objectToFilter(rest)` AND-conditions; **input object is mutated** (`delete data.filter`) |
| non-empty object | `objectToFilter(obj)` |
| `{}` | `[[[]]]` (match-all â€” subject to mass-delete guard, Â§32) |
| anything else | **422** |

Before filter mapping, `jsonSafeParse` is applied â€” strings that look like
JSON objects/arrays are parsed; scalars pass through (Â§31).

### 22.4 `parseFilterValue` â€” exact operator precedence

1. `!in(a,b,...)` (case-insensitive) â†’ `["not in", [trimmed...]]`
2. `in(a,b,...)` (case-insensitive) â†’ `["in", [trimmed...]]`
3. starts with `!` and remainder contains `%` â†’ `["not like", remainder]`
4. contains `%` â†’ `["like", value]`
5. starts with `>=` â†’ `[">=", rest]`
6. starts with `<=` â†’ `["<=", rest]`
7. starts with `>` â†’ `[">", rest]`
8. starts with `<` â†’ `["<", rest]`
9. starts with `!` â†’ `["!=", rest]`
10. otherwise `["=", value]`

Non-string input is `["=", value]` without parsing.

### 22.5 LIKE semantics per adapter

- **SQL adapters** (MySQL/MariaDB/Postgres/CockroachDB/SQLite3/MSSQL/Oracle):
  if no `%` present, adapters auto-wrap `%term%` (substring match). `%` and
  `_` are wildcards. Postgres uses `ILIKE` for both `like` and `ilike`;
  `not like` â†’ `NOT ILIKE`.
- **MongoDB**: regex-escaped, case-insensitive regex (`\Q...\E`-style escape).
- **Redis**: case-insensitive substring match over JSON-stringified hash values.
- **DynamoDB**: `contains` filter expression.
- `in`/ `not in` with an empty array is invalid for Postgres.

### 22.6 `sort`

`-col` â†’ DESC, `+col`/`col` â†’ ASC. Input forms: array, JSON-string array, or
comma-separated string. Known variants ignored; non-string elements ignored by
adapters. `sort` is removed from the filter payload. Multi-key sort ordering
is preserved.

### 22.7 NULL semantics (fixed â€” ADR-0009, D10)

Non-string `null` in a filter becomes an explicit NULL predicate per adapter:

- **SQL adapters:** `col IS NULL` (and `col IS NOT NULL` for `!=` negation) â€”
  `IS NULL` semantics as expected.
- **MongoDB:** `{ field: null }` (matches null and missing) â€” kept deliberately.
- **Redis/DynamoDB:** `null` payload matches fields whose stored JSON value is
  `null`.

This replaces the Node `col = NULL` (never matches) behavior; it is a deviation
from Node for SQL backends, recorded in `docs/deviations.md`.

---

## 23. Normative Contract: Hooks

Hook set (all optional, all may be async):

| Hook | When called | Payload carried | Effect of return value |
|---|---|---|---|
| `beforeSave` | first, insert/update/upsert (incl. patch) | whole payload (`{data}` for bulk) | replaces payload (item-level) |
| `beforeCreate` / `beforeUpdate` / `beforeUpsert` | after `beforeSave` | whole payload | replaces payload (item-level) |
| `afterSave` | after success | record (single) / adapter result (bulk) | **single:** replaces record; **bulk:** ignored |
| `afterCreate` / `afterUpdate` / `afterUpsert` | after success | same | same |
| `beforeFind` | in `find`/`findOne`/`list`/`byId` | current filter array | replaces filter **only if** the return is an array |
| `afterFind` | after read | array of rows | replaces rows array (or `h[0]` for one-item) |
| `beforeDelete` | in `remove` | original argument | replaces argument |
| `afterDelete` | after remove | adapter result | ignored |

Rules:

- `runItemHook` result replaces the item **only** if it is an object; a
  `undefined`/non-object return keeps the item.
- Bulk path: `runHook` iterates `payload.data` per row; top-level
  non-`{data}` arrays are iterated per element; single objects go through
  `runItemHook` directly.
- Existence checks (`findOne` in PUT/PATCH/DELETE route, existence fetch in
  `patch`) **bypass** `beforeFind` hooks but **do** decrypt; `afterFind` is
  not applied to the pre-check either (route uses raw response).
- Hook errors propagate and abort the operation; they are not caught.
- `afterFind` for `findOne`/`byId` takes `h[0]` when the returned array is
  non-empty.
- Kafka events are emitted **after** after-hooks and decryption (Â§25).

---

## 24. Normative Contract: Encryption

### 24.1 Envelope

- Algorithm: AES-256-GCM; random 12-byte IV; 16-byte tag; **no AAD**.
- Envelope: `enc:v<N>:<base64(ivâ€–ciphertextâ€–tag)>`; regex
  `^enc:v(\d+):([A-Za-z0-9+/]+={0,2})$`. Envelopes with an unknown version
  still decrypt if the keyring has that version.
- Nullable: `null`, `undefined`, and already-enveloped values are skipped on
  encrypt. `isEncrypted` only checks the prefix, not the version.

### 24.2 Serialization (typed round-trip)

| Type | Serialize | Deserialize |
|---|---|---|
| `boolean` | `"true"` / `"false"` (accepts `true`, `1`, `"true"`, `"1"` on encrypt) | `"true"` / `"1"` â†’ `true`; else `false` |
| `integer` | numeric string (`String(n)` if integer-like) | `parseInt(s, 10)`; non-numeric â†’ `NaN` (kept as-is; no throw) |
| `numeric` | numeric string | `parseFloat(s)` |
| `datetime` | ISO 8601 string | **stays a string** |
| `string` | verbatim | verbatim |

Unsupported encrypted types: warn once and store plaintext (schema validation
rejects most of these).

### 24.3 Nested dotted fields

- Only **one** level of nesting (`parent.key`) is supported.
- Dotted fields cannot be `pk`/`unique`/`softDelete`/`search_columns`;
  `object` parents must be pre-existing columns.
- Whole-object encryption (an `object` column marked `encrypted`) is **not**
  supported; use dotted keys.

### 24.4 Key resolution

`EncryptionConfig`:

```text
{ key: KeySource, version: u32 â‰¥ 1, keys?: Map<u32, KeySource> }
```

`KeySource` (`env:VAR` | base64 string | raw string):

1. `env:VAR` â†’ read env; missing â†’ **error**; value is sha256-hashed to 32 bytes.
2. base64 that decodes to exactly 32 bytes â†’ used directly.
3. anything else â†’ sha256-hashed.

- `version` must be a positive integer â‰¥ 1. `keys` map keys are parsed to
  integers â‰¥ 1; the active `version` entry must exist.
- Config is resolved **once at `db.init()`**; bare re-init preserves the prior
  keyring.
- Model with encrypted fields and no resolved config â†’ **throw** at model
  construction.
- `keys` provides the full version â†’ source map so old envelopes stay
  decryptable; writes always use the active version.
- Decrypting a tampered envelope is an error (tag mismatch), never plaintext.
- Reads are side-effect-free (decrypt does not re-encrypt).
- Legacy plaintext passes through on read.

### 24.5 CLI scan/rotate

- `encrypt:scan --tables a,b [--apply]`: report-only unless `--apply`; pages
  by keyset on PK (fallback offset pagination for composite/missing PK), page
  size 1000, writes in batches of 1000. Skips rows already encrypted or
  already at target version.
- `encrypt:rotate-key --to <v> [--new-key <src>]`: re-encrypts at the target
  version; legacy plaintext and same-version envelopes are left untouched.
  Uses the same pagination.

---

## 25. Normative Contract: Kafka

- Enabled only if `KAFKA_BROKER` (or `kafka.init({ broker })`) is set; env
  defaults: `KAFKA_CLIENT_ID=db-model-router`, `KAFKA_TOPIC_PREFIX=dbmr`;
  comma-separated brokers trimmed; disabled â†’ produced calls are no-ops with
  zero payload cost.
- Topic: `{prefix}.{table}`. Message key: `table`.
- One event **per row**; value JSON:
  `{ table_name, operation_type: "insert"|"update"|"upsert"|"delete", data, timestamp }`
  with an ISO-8601 `timestamp` shared per produce call.
- Batch size 500; produce failures are logged and **never** thrown.
- Events emitted only after successful writes. Single-record events carry the
  decrypted record; bulk events carry the decrypted payload rows. Bulk rows
  whose object columns were JSON strings get them parsed back to objects
  before production (`payloadForProduce`); non-JSON strings pass through.
- **Transactions**: events produced inside `db.transaction` are buffered;
  flushed after the **outermost** commit (fire-and-forget, not awaited);
  rollback discards. Nested SQLite savepoints (`sp_<depth>`) do **not** flush
  early â€” events stay buffered until the outermost transaction settles
  (ADR-0009, D11).
- `kafka.status()` / `kafka.disconnect()` semantics follow the Node module.

---

## 26. Normative Contract: Transactions

- Only adapters exporting `transaction()` participate: **SQLite and
  PostgreSQL**. All others â†’ `db.transaction()` throws
  `"Active database adapter does not support transactions."`.
- `db.transaction(fn)`:
  - `fn` receives a `tx` object with `tx.model(modelOrTable)`, and `tx.query`.
  - `tx.model(registeredModel)` returns the same model bound to the
    transaction adapter; `tx.model("table")` resolves the table through the
    global registry.
  - Commit on success, rollback on error, rethrow original error.
- **SQLite**: module/instance-level depth counter; nested invocations use
  SAVEPOINTs `sp_<depth>`; events buffered until the outermost transaction
  commits (ADR-0009, D11).
- **Postgres**: dedicated client; `BEGIN`/`COMMIT`/`ROLLBACK`; depth via
  closure; `RETURNING *` supported inside the transaction.
- MSSQL performs internal transactions for bulk insert only and does **not**
  export a transaction API.
- Kafka buffering inside transactions flushes after the **top-level** commit;
  nested savepoint events are **not** buffered (Â§25).
- Rollback must restore all writes in the transaction, including events.

---

## 27. Adapter Contract Matrix

All adapters implement the shape of Â§7.1. Per-adapter exact semantics that
must be reproduced or consciously deviated from (each row is a required test):

| Adapter | Escape & null-byte strip | Upsert | Bulk limit/behavior | get/list `RETURNING` | Soft-delete value | Notable coercions/retries | Transactions |
|---|---|---|---|---|---|---|---|
| MySQL | backtick-quote, strip `\0` | `ON DUPLICATE KEY UPDATE` | chunk >999 rows; `INSERT IGNORE` fallback when nothing to update | none â†’ re-fetch | `1` | `timezone=+00:00` forced; `multipleStatements` default true | no |
| MariaDB | as MySQL | as MySQL | as MySQL | none â†’ re-fetch | `1` | as MySQL | no |
| PostgreSQL | double-quote ident parts | `ON CONFLICT ... RETURNING` | 1000/chunk driver-side | `RETURNING *` | `FALSE` boolean literal | `ILIKE`; cached col types for numeric/bool; error map `23505`â†’`ER_DUP_ENTRY`, `23503`â†’`ER_NO_REFERENCED_ROW`; retry `ECONNREFUSED/ECONNRESET/EPIPE/57P01/57P03`; UTC session, optional `dateStrings` | yes |
| CockroachDB | as Postgres | as Postgres (no RETURNING rows on upsert? verify) | as Postgres | `RETURNING *`? | `FALSE` | Postgres-like dialect | **no export** |
| SQLite3 | `""`-double-quote ident | `ON CONFLICT` + `RETURNING *` fast path | sync, multi-statement detect â†’ `INSERT OR IGNORE` | `RETURNING *` | `1` (or param) | WAL mode; sync driver; nested SAVEPOINTs | yes (savepoints) |
| MSSQL | `[brackets]` (verify) | `MERGE ... OUTPUT inserted.*` | transaction per 1000-batch; `OUTPUT inserted.*` | `OUTPUT inserted.*` | `1` | `?`â†’`@paramN`; retry `ECONNREFUSED/ECONNRESET/EPIPE/ETIMEOUT`; dup code map `2627`/`2601`; `formatDate` UTC; `dateStringsMode`; **secure defaults: `encrypt:true`, `trustServerCertificate:false`**, opt-out requires explicit connection option + startup warning (ADR-0009, D17) | no (bulk-internal only) |
| Oracle | UPPERCASE quoted identifiers | `MERGE ... RETURNING ... INTO` | batch array binds | `RETURNING`/fetch | `1` | `OUT_FORMAT_OBJECT`; CLOB as string; session date format; retry ORA codes | no |
| MongoDB | n/a (typed) | `_id` 24-hex â†’ ObjectId; `$set` excludes `_id` | unordered bulk writes | n/a â†’ `find` | `1` | regex-escape, case-insensitive; dup key mapped to `ER_DUP_ENTRY` | no |
| Redis | n/a (key-prefix) | overwrite hash by key | n/a | n/a â†’ re-fetch hash | `1` | records = hashes `{table}:{pk}`; objects JSON-stringified in hash; scans **all** keys O(N); substring match case-insensitive | no |
| DynamoDB | n/a | `PutItem`/`UpdateItem` | `BatchWriteItem` 25/batch | n/a â†’ `GetItem`/`Query` | `1`/bool | `primaryKey`, `tablePrefix`/`prefix`, region/endpoint/credentials; filter expressions; `contains` | no |

Additional rules:

- `remove` returns `{ message: "<N> <table>[s] removed" }` with raw table
  name, singular/plural on N>1.
- Bulk return `{ rows, message, type: "success", id? }` where `message` is
  `"<N> <Table Name>s are saved"` (`namify`: `_`â†’space, `\b\w`â†’uppercase);
  `id` only when N==1.
- `count` in `get` is computed by an additional `COUNT(*)` query unless
  `count:false` or `rows.length` suffices (see source `get(... count ...)`).
- Adapters throw `Invalid filter object` (500-visible) for shapes they cannot
  compile; this is not converted to 422 (Â§20.3).
- `qcount` (COUNT query) filters by the same filter + safeDelete.

---

## 28. Normative Contract: Schema

`dbmr.schema.json` â†’ validated â†’ normalized â†’ printed.

### 28.1 Top-level shape

`{ adapter, framework, tables, relationships?, options? }` â€” plus optional
`unique`/per-table extras. Valid adapters: `mysql`, `mariadb`, `postgres`,
`sqlite3`, `mongodb`, `mssql`, `cockroachdb`, `oracle`, `redis`, `dynamodb`.
Valid frameworks: `express`, `ultimate-express`.

### 28.2 Column rules

- Column key regex: `^(encrypted\|)?(required\|)?(string|integer|numeric|boolean|object|datetime|auto_increment)(:[^|]+)?(\|[^|]+)*$`
  (compound pipe-separated tokens, optional rule argument after `:`).
- `encrypted` may only prefix `string|integer|numeric|boolean|datetime`;
  not allowed on `object` or `auto_increment`.
- `encrypted|` must precede `required|` if both.
- Dotted fields (`parent.child`): parent must exist and be `object`, max one
  nesting level; dotted fields cannot be `pk`, `unique`, `softDelete`, or in
  `search_columns`.
- `pk`, `softDelete`, `search_columns`, `unique` entries cannot reference
  encrypted columns.
- `unique`: flat array (one composite group) or array of arrays; each entry
  must be an existing column or equal to the PK.
- `softDelete`, `search_columns` must reference existing columns; `parent`
  must reference an existing table.
- Relationships require `parent`, `child`, `foreignKey` and must reference
  existing tables (see Â§9).
- `options`: `output` non-empty string or `null`; `saasStructure`,
  `rateLimiting`, `helmet`, `logger`, `loki` booleans; `apiBasePath` starts
  with `/`; `port` positive integer; `session` âˆˆ `memory|redis|database`;
  `encryption` shape validated as in Â§24.

### 28.3 Parser normalization (defaults)

- Table: `pk = "id"`, `unique = [pk]`, `timestamps = { created_at: null,
  modified_at: null }` (null = timestamp support not declared; generation
  emits defaults), `softDelete = null`, `search_columns = []`, `parent = null`.
- Relationships derived from `parent`: each child table with `parent` implies
  a relationship `{ parent, child, foreignKey: <parentPk> }` (parent PK name).
- `options`: `session = "memory"`, `rateLimiting = helmet = logger = true`,
  `loki = false`, `output = null`, `saasStructure = true`, `apiBasePath =
  "/api"`, `port = 3000`.
- Strings are not trimmed implicitly; `adapter`/`framework` are
  case-normalized (check source for exact rule â€” verify before golden tests).

### 28.4 Encryption metadata compile

From the table rules, per encrypted column builds `{ col, path }` defs:
top-level â†’ `{ path: col }`; dotted â†’ `{ path: col, nested: ["parent",
"child"] }`. `hasEncrypted` toggles model-time requirements.

### 28.5 Schema â†’ migration / SQL generation

- Timestamp columns get `DEFAULT CURRENT_TIMESTAMP` (adapter-specific form).
- `softDelete` boolean gets adapter boolean default.
- PK `auto_increment` â†’ SQLite `INTEGER PRIMARY KEY AUTOINCREMENT` etc.
- `unique` groups are emitted as `UNIQUE (...)` constraints excluding the PK.
- Encrypted columns emit as encrypted-compatible types (typically `TEXT`);
  exact mapping per type per adapter must be a test matrix row.

### 28.6 `inspect` (database â†’ schema)

- `--type` required; SQL adapters only; `--env` (default `.env`), `--out`
  (default `dbmr.schema.json`), `--tables` (comma filter), `--json` (print,
  no file), `--dry-run` (report only).
- Introspects columns, PK, uniques, defaults, auto-increment; detects
  `created_at`/`modified_at` variants and maps them to `timestamps`; detects
  boolean `softDelete`-like columns (documented heuristic â€” see Â§34).
- Exit code 1 on errors.

### 28.7 Schema round-trip

Parse â†’ normalize â†’ print â†’ parse must be idempotent. Print rules (from the
Node reference `printSchema`): sorted table/column keys; objects 2-space
indent; trailing newline; omitted defaults (schema-compact form). The exact
compact-key set is part of the golden-file contract and must be locked down
in the Rust golden tests, including whether `framework`/`relationships` are
emitted when absent.

### 28.8 init-time generation

`init` is **first-buildout only**: refuses if `app.js` exists (unless
`--dry-run`); all configuration from `schema.options`; no config flags.
Generated project is ESM Node/Express today â€” see Â§35 open decisions.

---

## 29. Normative Contract: CLI

### 29.1 Commands and flags

All commands accept universal flags: `--yes`, `--json`, `--dry-run`,
`--no-install`, `--help`. `--json` suppresses logs; command results are
accumulated and printed as a single object (or array). Arg parser:
`--new-key` â†’ `args.newKey` (camel-cased); `--from schema.json` â†’
`{ from: "schema.json" }`; hyphenated â†’ camelCase; flags never consume the
next token starting with `-`.

| Command | Required flags | Defaults / notes |
|---|---|---|
| `init` | `[schemaPath]` | default `dbmr.schema.json`; refuses if project exists; `--dry-run` preview only; `--no-install` |
| `inspect` | `--type` | `--env .env`, `--out dbmr.schema.json`, `--tables`, `--json`, `--dry-run`; adapter must be SQL |
| `doctor` | â€” | `--from`, `--yes`, `--json`; checks schema, adapter driver dep, file sync |
| `diff` | â€” | `--from`, `--yes`, `--json`; read-only; prints Added/Modified/Deleted |
| `db-manager` | `DB_TYPE` in env | `--env .env`, `--port 4000`; binds all interfaces; no auth |
| `types` | â€” | `--from`, `--out types/models.d.ts`, `--stdout`; generates TS |
| `migrate:create <name>` | name | `--type`, `--env`, `--dir`/`--migrations`; name â†’ `YYYYMMDDHHmmss_<sanitized>.js|.sql` |
| `migrate:up` | â€” | applies pending; records batch |
| `migrate:down` | â€” | rolls back last batch in reverse |
| `migrate:status` | â€” | Applied/Pending list |
| `encrypt:scan` | `--type` | `--from`, `--env`, `--tables`, `--key`, `--version`, `--keys`, `--apply` (default report) |
| `encrypt:rotate-key` | `--to` | `--new-key`, `--key`, `--keys`, `--from`, `--env`, `--tables` |
| `help` | optional command | per-command help |

### 29.2 Exit codes

0 success, 1 user/usage/schema errors, non-zero on unexpected failure.
`--json` errors are reported in the JSON output, not stderr.

### 29.3 Known upstream defect â€” do NOT reproduce

`OutputContext` exposes only `log`, `result`, `flush`, yet `migrate:create`,
`migrate:up`, `migrate:down`, and `types` call `outputContext.success()`,
which throws when a context is injected. The Rust CLI must use one coherent
output interface.

### 29.4 `init` generated project (current Node implementation â€” to be decided)

`package.json` (`type: "module"`), `app.js`, `commons/`, `routes/`,
`middleware/`, `migrations/`, `models/`, tests, OpenAPI spec, optional SaaS
structure, Dockerfile/.dockerignore/docker-compose(when needed), `.env`,
random secrets shared between `.env` and compose, `scripts` + `dependencies`
installed unless `--no-install`. See Â§35 for the open question of whether the
Rust CLI keeps generating Node or generates Rust.

---

## 30. Normative Contract: Migrations

- Tracking table: **`_dbmr_migrations`** â€” `id VARCHAR(255) PK`, `name`,
  `batch INT`, `applied_at` default current timestamp; adapter-specific DDL
  for Postgres/Cockroach, MySQL/MariaDB, MSSQL, Oracle, SQLite default.
- File set: `^\d+_.+\.(js|sql)$`, `.down.sql` excluded; alphanumeric order.
- `id` = basename without extension; `name` = full filename.
- `migrate:create` writes `YYYYMMDDHHmmss_<sanitized_name>.js` with
  `exports.up(db)`/`exports.down(db)` template. SQL migrations require a
  sibling `.down.sql` for rollback.
- `up`: applies pending in order; `batch` = max(existing) + 1. **No
  wrapping transaction.** JS migrations are `require`d by absolute path
  (dynamic require) â€” arbitrary code execution by design.
- `down`: rolls back **last batch only**, in reverse ID order.
- `status`: Applied/Pending with batch numbers.
- Missing-table detection string set: `no such table`, `does not exist`,
  `doesn't exist`, `invalid object name`, `ORA-00942`.
- **`.js` migrations cannot run in Rust without Node**; see Â§35 decision.

---

## 31. Normative Contract: JSON Utilities (`function.js`)

- `jsonSafeParse(str)`: if `str` is a string whose trimmed form starts `{`
  & ends `}` or `[` & ends `]` it is JSON-parsed; **plain scalars (`"12345"`,
  `"true"`, `"null"`, `"hello"`) are returned as strings, never coerced.**
- `jsonStringify(obj)`: internal JSON-typed strings are decoded back before
  re-serialization; non-JSON strings pass through verbatim.
- **Big-int fidelity â€” Rust implementation.** Do **not** replicate the JS
  string-regex pre-wrapping step. Enable `serde_json`'s
  `arbitrary_precision` feature workspace-wide: `serde_json::Number` then
  retains the exact source lexeme, and `i64`, `u64`, and beyond are preserved
  losslessly (e.g. `9007199254740993` round-trips exactly). `u64`/`i128` map
  natively for database co-types (BigInt, Mongo Long, DynamoDB N). There is
  no regex pass over payloads at any layer.
- `getByPath(obj, "a.b.c")` / `setByPath(obj, "a.b.c", v)`: dotted-path
  access; `getByPath` returns `undefined` for missing segments; used by
  overrides (`req` paths) and encryption (nested fields).
- Big-int fidelity is a first-class requirement: `9007199254740993` must
  round-trip unchanged through parseâ†’stringify; property tests must cover
  `2â¶Â³-ish` and >`u64` cases.

---

## 32. Expanded Security Requirements

- Identifiers (table/column) are quoted per adapter and null bytes
  (`\0`) stripped; **never** interpolated for values.
- **Mass-delete guard.** Empty-filter deletes are rejected at the model layer
  with a dedicated error before any adapter is invoked (ADR-0009, D15). The
  HTTP layer separately blocks empty bodies for `DELETE /` (400). A model may
  opt into match-all deletes via an explicit `allow_destructive: true` option
  (documented, off by default, logged at startup when used). This is a security
  fix, not optional parity.
- **Prototype pollution.** Keys `__proto__`/`constructor` must not be used to
  set prototype state when JSON-cloning filter objects or merging payloads;
  use `Object.create(null)`-style maps or explicit allow-lists.
- **ReDoS.** The validator column-rule regex and `parseFilterValue` regexes
  are linear-safe today; the Rust regexes must retain linear behavior
  (no nested quantifiers); search strings passed into Mongo regex must be
  escaped (Â§22.5).
- **Mongo injection.** `$`-prefixed keys in user JSON could become query
  operators; strip/escape `$`-prefixed top-level keys in filters before
  building Mongo queries.
- **db-manager exposure.** It binds all interfaces, has **no authentication**,
  and exposes raw SQL. Any Rust db-manager must default to `127.0.0.1`,
  require an explicit `--port`/`--bind` and opt-in `--host 0.0.0.0`, and gate
  raw SQL behind a config flag. Query history stores raw SQL â€” redact or
  disable in production.
- **KMS/secrets.** Encryption keys must be zeroized after use; never log
  key material; `errorResponse` must never print raw `err` in production.
- **`"null"` string deletion** â€” footgun removed per ADR-0009 (D8): literal
  `"null"` strings are stored verbatim.
- Row-level authorization is the application's responsibility; the override
  mechanism is the only multi-tenancy primitive and must be documented as
  such.

---

## 33. Test-Case Traceability

Maintain `tests/traceability.json`:

```json
{ "source": "db-model-router/test", "cases": [
  { "file": "test/model.test.js", "name": "inserts single record", "rust": "dbmr-core::model::insert_single_returns_record", "status": "mapped" }
] }
```

Rules:

- Every source case must appear exactly once.
- `status` âˆˆ `mapped` | `waived` | `n/a`; `waived` requires a rationale.
- CI recomputes coverage from this file and fails below 95%.
- Source inventory today: **117 files** â€” adapters (bulk/individual/route/
  security Ã—10, ~20), CLI/command tests (~8 groups), core model/route/filter/
  function/encryption/kafka/migration, integration (db-manager, schema
  workflow), **20 property tests**, **15 test/unit/\* files**. Count cases with
  `rg -c "\bit\(" test/`.

Recommended Rust test layout mirroring intent: `dbmr-core/tests/{model,
filters,encryption,kafka,transaction}.rs`, per-adapter
`dbmr-adapters/tests/*.rs` (feature-gated), `dbmr-cli/tests/golden.rs`,
`dbmr-schema/tests/roundtrip.rs`, property tests with `proptest`.

---

## 34. Known Upstream Defects â€” Decide, Don't Blindly Preserve

Decisions record **ADR-0009** (fixed 8, preserve 12). Column "Status" reflects
the approved disposition. See `docs/adr/0009-defects-fix-vs-preserve.md` for the
authoritative resolution text.

| # | Defect | Suggested resolution | Status (ADR-0009) |
|---|---|---|---|
| D1 | Single insert route documented as `POST /add`, actually `POST /:id` | Keep `/:id`; document | **PRESERVE** |
| D2 | `errorResponse` reads `err.cause.status`, never `err.status` | Typed `AppError`; status enum, never stringly-typed | **FIX** |
| D3 | `findOne` â†’ `false`; `byId` â†’ `null`; `insert` â†’ `null`; inconsistent | Rust: `Option<Record>` everywhere; 404 mapping at HTTP | **FIX** |
| D4 | Single insert/update/upsert silently returns `null` when re-fetch misses (skips hooks/Kafka) | Preserve `None` + add re-fetch-miss metric/warning log (no silent behavior change beyond logging) | **PRESERVE** (warn note) |
| D5 | Bulk `POST /`/`PUT /` require `data` array; a real column named `data` makes single records bulk | Keep detection; warn on columns named `data` in schema validator | **PRESERVE** |
| D6 | CSV header from first row only; XML doesn't escape `'` | Byte-parity CSV/XML output | **PRESERVE** |
| D7 | `sendFormatted` treats `{ data }` as rows | Keep documented behavior | **PRESERVE** |
| D8 | Payload override deletes any value exactly `"null"` | `"null"` string stored verbatim | **FIX** |
| D9 | `dataToFilter` mutates input (`delete data.filter`) | Pure function; merged-group semantics kept | **PRESERVE** (purified) |
| D10 | Non-string `null` â†’ `= NULL` matches nothing in SQL | Explicit `IS NULL` / NULL predicates per adapter | **FIX** |
| D11 | Nested SQLite savepoint events emit before outer commit | Buffer until outermost tx commit | **FIX** |
| D12 | CLI `OutputContext` lacks `success/info/error`; 4 commands call them â†’ throw | Single coherent CLI output API | **PRESERVE** (fixed in passing) |
| D13 | Model never sets/refreshes timestamps despite "auto-management" claims | Document DB-defaults contract | **PRESERVE** |
| D14 | `find` returns `{ data, count }` where `count` = total matches | Keep; document | **PRESERVE** |
| D15 | `{ filter: [] }` â†’ match-all (delete-all risk) | Model-layer mass-delete guard; opt-in `allow_destructive` | **FIX** |
| D16 | `inspect` heuristic for softDelete/timestamps may misfire | Report-only confidence output | **PRESERVE** |
| D17 | MSSQL `encrypt:false` + `trustServerCertificate` default | Secure defaults + explicit opt-in | **FIX** |
| D18 | Timestamps stripping covers many casing variants; `timestamps:{x:null}` means "not declared" yet variants stripped | Keep strip; document | **PRESERVE** |
| D19 | `safeDelete` writable by clients | Server-managed: stripped from HTTP write payloads & bulk UPDATE validators | **FIX** |
| D20 | Route GET-by-id does not run `byId` validation | Keep `find`-based path; document | **PRESERVE** |

---

## 35. Open Decisions â€” Status (ADR-indexed)

**Answered (accepted):** see `docs/adr/`.

1. **API stance** → **ADR-0001 (Accepted)** — Rust-native API, behavior parity;
   no drop-in proxy; `Option<Record>` unified (D3 fix); Node shim deferred (ADR-0008).
2. **Generated-project target** → **ADR-0002 (Accepted)** — Rust/Axum default;
   no Node-target flag in v1.
3. **db-manager scope** → **ADR-0003 (Accepted)** — Webserver with embedded UI
   (`dbmr-dbmanager`), default bind `127.0.0.1`, raw SQL gated by `--enable-raw-sql`,
   query history redacted, default port 4000.
4. **`.js` migration compatibility** → **ADR-0004 (Accepted)** — never eval JS;
   `.sql` pairs (`.up.sql`/`.down.sql`) only; `.js` files rejected with guidance.
5. **Crate publishing / MSRV / feature matrix** → **ADR-0005 (Accepted)** —
   single published `dbmr` facade (MSRV 1.85, edition 2021, MIT); internal
   crates `publish = false`; default features = `sqlite`; other 9 adapters +
   `kafka` + `db-manager-web` behind features.
6. **JSON/decimal fidelity** → **ADR-0006 (Accepted)** — `rust_decimal::Decimal`
   first-class in `DbValue::Decimal`, `i128` integers, `arbitrary_precision` globally,
   UTC ISO 8601 datetimes.
7. **Error/exit-code contract** → **ADR-0007 (Accepted)** — Typed `AppError` enum
   with HTTP status mapping; CLI exit codes: 0 success / 1 user/usage / 2 internal.
8. **The `db` global** → **ADR-0008 (Accepted)** — Explicit context primary;
   `OnceLock<Arc<dyn DatabaseAdapter>>` application global with `testing` reset ring.
9. **Defect fixes approved** → **ADR-0009 (Accepted)** — D2, D3, D8, D10, D11,
   D15, D17, D19 fixed; remaining defects preserved (documented).
10. **CI database matrix** → **ADR-0010 (Accepted)** — all 10 adapters as live
    service containers (fast-lane / heavy-lane); no silent skips.
11. **Benchmark budgets** → **ADR-0011 (Accepted)** — Diesel, SeaORM, SQLx raw,
    tokio-postgres contenders, 15 scenarios, strict regression budgets.
12. **Node interop** → **ADR-0012 (Accepted)** — Pure Rust v1; migration guide provided;
    Node bindings deferred to post-1.0.

All Phase 0 decisions resolved and accepted.

---

## 36. Engineering Quality Gates (Baseline Features)

These are **baseline requirements** â€” not optional hardening â€” and must be
green on every merge, not only at release.

### 36.1 Formatting

- `cargo fmt --check` passes with no diffs; CI fails otherwise.
- Import/module ordering via `rustfmt.toml` (no unstable options in stable CI).
- Line width 100; trailing newline; consistent `use` grouping (`std`,
  external, `crate`).

### 36.2 Linting (`clippy`)

- `cargo clippy --all-targets --all-features -- -D warnings` is a CI gate.
- `clippy.toml`: `msrv = "1.85"` (or the pinned MSRV); `avoid-breaking-exported-api = true`.
- Denied on **library** crates (allowed only in `#[cfg(test)]`/benches):
  `unwrap_used`, `expect_used`, `panic`, `indexing_slicing` where feasible,
  `dbg_macro`, `todo`, `unimplemented`.
- On by default: `clippy::pedantic` groups selectively enabled;
  `clippy::cognitive_complexity` threshold ~30; `clippy::too_many_lines` ~120;
  `clippy::too_many_arguments` ~7; `clippy::module_complexity`.
- `#![forbid(unsafe_code)]` at crate roots by default; any opt-out requires a
  `// SAFETY:` comment and review sign-off (driver interop only).
- `clippy::arithmetic_side_effects` where bounds are non-obvious; every `as`
  cast on user/DB data must be `try_into().map_err(...)`.
- Deny `clippy::float_cmp` in non-test code (page/size are integers).

### 36.3 Code smell / structure rules

- Functions â‰¤ 120 lines; modules without god-structs; public API surface
  documented with `# Examples` where it has behavior.
- No clones in hot paths without justification (`RowMap` moves are moves);
  ASCII/Unicode limits in formatters declared intentionally.
- `#[must_use]` on pure builders (filter builders, sort keys).
- Errors: one `thiserror` enum per crate; no bare `Box<dyn Error>` internally;
  context via `.with_context(...)` at boundaries only.
- No `Arc<RwLock<_>>` in new code â€” `OnceLock`/`arc_swap`/message-passing only.
- Feature-flag combinations: CI builds `default`, `sqlite+postgres`, and
  `--all-features`; feature-gated code paths are exercised in tests.

### 36.4 Static scans

- **`cargo audit`**: CI fails on any `high`/`critical` RustSec advisory;
  exceptions require an ADR with expiry date.
- **`cargo deny`**: license allowlist (MIT, Apache-2.0, BSD-2/3, ISC, Zlib,
  MPL-2.0); `deny` on unlisted licenses; ban duplicate major versions of core
  crates (`tokio`, `serde`, `rustls`); source must be crates.io by default.
- **`cargo semver-checks`** in release pipeline: detect accidental breaking
  API changes against the previous tag; breaks require semver bump.
- **`cargo udeps`** (needs nightly or stable fallback) and/or
  `cargo machete` to detect unused dependencies â€” reported as warnings, fixed
  per release.
- **Spell/basic docs check** on public API.
- **Secrets scan**: `gitleaks` (or `trufflehog`) on CI and pre-commit;
  patterns for `enc:v` envelope payloads, AWS/GCP tokens, `.env` contents.
- **Log-injection scan**: `tracing` output must not include raw user payloads
  or keys; enforced via lint that greps for `payload`/`password`/`secret`
  in `.instrument`/`.event` calls.
- **Dependency pinning**: `Cargo.lock` committed for binaries and workspace;
  MSRV CI (`1.85`) tested on stable + MSRV.

### 36.5 Security cases (static)

- `cargo geiger` reported (informational) for unsafe usage in tree.
- Assert no `unsafe` in first-party code (see 36.2); third-party unsafe
  reviewed per RUSTSEC channel.
- SQL-injection lint: CI rule banning `format!` into query strings in
  `src/` outside `escape_id` helpers; raw SQL literals allowlisted only in
  migration runner (migration bodies are user-authored by design) and
  db-manager (explicit opt-in).
- Search for `String::from("SELECT")` concatenation patterns must return zero
  hits.

### 36.6 Test-case discipline

- Every public fn: â‰¥ 1 positive test, â‰¥ 1 negative/error test, and â‰¥ 1
  default/boundary test where inputs permit (null/missing/empty/0/false/max).
- Every new feature lands with its test file in the same PR; no "add tests
  later" exceptions for model/filter/validator/HTTP/encryption paths.
- Bug fixes require a regression test first with a link to the issue/deviation.
- Test names: behavior-based snake case (`insert_single_returns_record`, not
  `test_1`); each test self-contained; shared fixtures via `rstest`/`proptest`
  generators.
- Determinism: tests must not depend on wall clock, env order, or network
  beyond testcontainers/docker-compose.
- Flaky tests: quarantine lane with expiry â‰¤ 14 days; flake rate CI alarm.

### 36.7 Documentation gates

- `README.md`, `CHANGELOG.md` (Keep a Changelog), `docs/deviations.md`,
  `docs/adr/*`, per-adapter connection docs checked in and tested in doc-tests.
- Public API has rustdoc; `cargo doc --no-deps` builds without warnings.

### 36.8 Benchmarks & regressions (see Â§38)

- Nightly `cargo bench` on dedicated runner; > 20% latency regression on any
  Â§38 scenario fails the run and opens a tracking issue automatically.

---

## 37. Null, Undefined & Default Behavior Contract (Normative)

JavaScript distinguishes `undefined` (absent/not passed) from `null`
(explicit empty). Rust collapses both into `Option<T>`. This section pins
the mapping and the non-obvious defaults so tests cannot be written from
memory.

### 37.1 Representation rules

| JS input | Wire/JSON | `serde_json`/Rust | Rule |
|---|---|---|---|
| `undefined` | key absent | key absent / `None` | Never written; validators treat as missing |
| `null` | `"key": null` | `Value::Null` | Written as SQL/NoSQL NULL unless column is NOT NULL (DB error) |
| `"null"` (string) | string | `String` | **Stays a string**; passes `parseFilterValue` as `=` "null" (NOT SQL NULL); footgun in override Â§20.6 |
| `""` | string | `String` | Passed verbatim to filters (`=`, `like`â€¦) â€” not treated as empty |
| `0`, `false` | number/bool | `Int(0)`, `Bool(false)` | Never falsy-collapsed in the rewrite (JS `==` semantics are not replicated blindly) |

### 37.2 Every public input â€” decision table

| Input | Rule |
|---|---|
| `byId` id = `null`/`undefined`/`""` | throw 422 (`Invalid id value`) |
| `byId` id = non-string/non-number (object, bool, NaN) | throw 422 |
| `byId` id = `0` | **valid**; treated as PK = 0; missing â†’ `null` (200 flow at HTTP layer â†’ 404) |
| filter value `null` (non-string) | Explicit NULL predicate: `IS NULL` (SQL) / `{ field: null }` (Mongo); see Â§22.7 (ADR-0009, D10) |
| filter value `""` | allowed; `=`, `%`, etc. applied to empty string |
| `{ data: null }` in remove | **422** â€” `dataToFilter(null, pk)` |
| `{}` whole body in remove | model: match-all (delete-all risk) â†’ **rewrite rejects at model layer** (Â§32); route: 400 empty-criteria guard first |
| `{ filter: [] }` | match-all (same reject rule) |
| `{ data: [] }` bulk insert | `required\|array` **passes**; zero rows inserted; response `{ rows: 0, message: "0 <Table> is saved"? }` â€” exact message per Â§27 wording; no rows to events; Kafka emits nothing |
| `page`/`size`/`limit` = `"abc"`, `NaN`, `-5`, `0` | `page â†’ 0`, `size â†’ 30` |
| `size` = `201` | clamped to `200` |
| `limit` given with no `size` | `limit` wins (alias) |
| `search` = `""` or all-whitespace | ignored; no filter added |
| `sort` = `""` / `null` | `sort = []` (no ORDER BY) |
| `select_columns` = `""` / unknown names | `""` â†’ ignored; unknown names silently omitted from each row |
| `output_content_type` = unknown value | falls back to JSON 200 |
| `options.created_at` = `[]` | no columns stripped for `created` group |
| `unique` = `[]` | insert batch: no ON CONFLICT clause; bare insert |
| `true`/`false`/`1`/`0` as boolean-encrypted | `"true"`/`"false"` serialize; deserialize `"true"\|"1"` â†’ `true`, else `false` |
| `NaN` from driver numerics | `DbValue::Float(NaN)` preserved; doc says detection needed; no coercion to 0 (JS emits `NaN` too â€” keep, but `jsonStringify` must not panic) |
| hook returns `null` | runItemHook keeps the **original item** (only objects replace) |
| hook returns `0/false/""` | keeps original item |
| `afterFind` returns empty array | rows = empty array (data "erased") |

### 37.3 Default-value table (parser/model/CLI/schema)

| Module | Parameter | Default |
|---|---|---|
| model option | `safeDelete` | `null` (hard delete; `READS filter not applied`) |
| model option | `search_columns` | `[]` |
| model option | `hooks` | `{}` (no-ops) |
| model option | strip sets | `created_at` & `updated_at` variant lists (Â§6.2) |
| model option | `created_at` | `null` â†’ default variant set |
| pagination | `page` / `size` / `limit` / max | `0` / `30` / `limit` alias / `200` |
| list/find | `sort` | `[]` |
| list/find | `count` | `true` for `get` (COUNT query issued); `false` disables |
| insert | `unique` | `[]` |
| index | `model(name, ...)` legacy 1st-arg string form | uses global `db` |
| init | adapter keys | case as listed; `postgres`/`postgresql` alias both valid |
| init | `options` | `{}` |
| Kafka | `KAFKA_TOPIC_PREFIX` / `KAFKA_CLIENT_ID` | `dbmr` / `db-model-router` |
| Kafka | `broker` | unset â‡’ producer disabled |
| Kafka | batch | `500` |
| CLI | `--env` / `--port` (db-manager) / `--out` (types) / `--from` | `.env` / `4000` / `types/models.d.ts` / `dbmr.schema.json` |
| CLI | migrate `--dir` | `migrations` |
| CLI | encrypt scan `--apply` | absent â‡’ **report-only** |
| CLI | output context | `--json` off by default; human logs on |
| schema options | `session` | `memory` |
| schema options | `rateLimiting` `helmet` `logger` `saasStructure` | `true` |
| schema options | `loki` / `output` | `false` / `null` |
| schema options | `apiBasePath` / `port` | `/api` / `3000` |
| schema timestamps | declared `timestamps.created_at` null | timestamps declared but name not overridden; variant defaults used |
| error prefix | `BASE_SECURE_PATH`-style path helper | OS-specific infer; see `src/commons/function.js` â€” replicate behavior |
| `.env` | `NODE_ENV` `development`/`test`/`TEST` | 5xx detail passed through; else `"Internal server error"` |

### 37.4 Default behavior guarantees (language-agnostic)

- Invalid input that JS throws on (`required` missing) â‡’ 422 with the same
  validator message space-joined format.
- Unknown columns â‡’ **silently dropped** (never an error) â€” after validation.
- `encrypted` model without resolved config â‡’ **throw** at model construction.
- Hook exceptions â‡’ propagated, operation aborted â€” no swallow.
- Kafka producer exceptions â‡’ **swallowed + logged**.
- Produce call on disabled Kafka â‡’ no-op returning `false`.
- Rollback â‡’ all-or-nothing within SQLite/Postgres tx; Kafka events discarded
  (with nested-savepoint caveat, Â§25).
- `.down.sql` absent â‡’ `migrate:down` fails with clear error.
- Missing migrations table â‡’ created on first `up`/`status` call.
- `_dbmr_migrations` batch number = `MAX(batch) + 1` on `up`; `down` rolls
  back exactly the last batch.

### 37.5 Null/undefined test requirement

Every row in Â§37.2/37.3 that has an observable difference between
missing/`null`/`""`/`0`/`false` requires a regression test in
`dbmr-core/tests/null_semantics.rs` (and per HTTP layer in
`dbmr-http/tests/empty_payloads.rs`). These are counting toward the >95%
test-case target in Â§12.1.

---

## 38. Differential Benchmark vs Rust ORMs (PostgreSQL) â€” Required

### 38.1 Goal

Produce and maintain a **peer comparison between `dbmr` (PostgreSQL adapter)
and established Rust ORMs/query libraries**, on identical hardware, data, and
schema â€” not just vs the Node version. Results are published per release and
tracked over time.

### 38.2 Contenders (pinned; bump only via ADR)

| # | Contender | Crate | Role |
|---|---|---|---|
| 1 | `dbmr` | workspace | subject under test |
| 2 | **Diesel** | `diesel` 2.x (sync, `postgres` feature, pool via `diesel::r2d2`) | canonical Rust ORM, compile-time schema |
| 3 | **SeaORM** | `sea-orm` (latest stable) | async, most-complete ORM |
| 4 | **SQLx** (raw query harness) | `sqlx` postgres | SQL-level ceiling for the same protocol |
| 5 | **tokio-postgres** (raw) | `tokio-postgres` + `deadpool-postgres` | protocol ceiling |
| 6 | `db-model-router` (Node, for reference) | pin to current `package.json` via the Node `benchmark/` suite | baseline parity point |

All Rust contenders use the **same tokio runtime** (`current_thread` not
permitted; `multi_thread` default with same worker count), same pool size
(10), same TLS mode (none for bench local), same PostgreSQL version (16 via
testcontainers/`docker-compose` pinned image).

### 38.3 Schema & data

Copy `benchmark/schema.sql` from `db-model-router` unchanged: single
`bench_items` table + `bench_audit` child; seed **2000** deterministic rows
via raw SQL before each contender (TRUNCATE ... RESTART IDENTITY CASCADE);
`sku` unique btree index (`active` index requested where present in source).

### 38.4 Scenarios (must mirror the Node suite 1:1)

| Key | Label | Iterations | Runs | Warmup |
|---|---|---:|---:|---:|
| `insert_single` | Insert single row (returns record) | 1000 | 5 | 100 |
| `bulk_100` | Bulk insert 100 rows | 100 | 5 | 10 |
| `bulk_1000` | Bulk insert 1,000 rows | 30 | 5 | 5 |
| `bulk_10000` | Bulk insert 10,000 rows | 10 | 3 | 2 |
| `read_pk` | Read by PK | 1000 | 5 | 100 |
| `find_filter` | Find filter indexed | 200 | 5 | 20 |
| `find_sort_limit` | Filter + sort + limit 20 | 200 | 5 | 20 |
| `list_page` | Paginated list (page 1 size 20) | 200 | 5 | 20 |
| `update_pk` | Update by PK | 1000 | 5 | 100 |
| `bulk_update_100` | Bulk update 100 by PK list | 100 | 5 | 10 |
| `upsert` | Upsert unique key (half ins / half upd) | 1000 | 5 | 100 |
| `delete_pk` | Delete by PK | 1000 | 5 | 50 |
| `tx_multi` | Transaction insert+update+audit | 200 | 5 | 20 |
| `tx_rollback` | Forced-error rollback | 200 | 5 | 10 |
| `jsonb_roundtrip` | JSONB insert + read back | 1000 | 5 | 100 |

### 38.5 Method & fairness rules

- One contender per process; 5 s cooldown between; fixed order, optional
  `--reverse` pass to detect order bias.
- Fresh pool per process; identical `max = 10`, `min = 0`.
- **dbmr is measured with default settings (validation on)** â€” same as the
  Node benchmark comparison.
- All contenders serialize/deserialize identically typed rows (roughly the
  `bench_items` Rust struct via `RowMap`/`DbValue` for dbmr).
- Prepared statements used where natural; Diesel using bind params (its
  normal path); SeaORM using SeaQuery builders (its normal path); sqlx raw
  with hand-written SQL.
- Numbers reported: ops/sec, latency mean, p50, p95, p99, min, max; row
  counts for size scenarios; allocations via `#[global_allocator]` counting in
  a `bench` profile (optional, best-effort).
- Report: JSON + HTML (mirror `benchmark/report.js` layout: contender Ã—
  scenario matrix with winner badges); committed to
  `benchmarks/reports/<yyyy-mm-dd>-<sha>.html` per nightly run.
- Documented, deterministic jitter handling: discard outliers via
  nearest-rank p-quantiles, report both raw and trimmed medians.

### 38.6 Acceptance budget (PR gate for releases)

- **vs Diesel median**: dbmr within **10%** throughput of Diesel on
  `insert_single`, `read_pk`, `update_pk`, `delete_pk`, and `jsonb_roundtrip`.
- **vs SeaORM median**: within **10%** on bulk and transaction scenarios.
- **vs SQLx raw**: dbmr â‰¥ **85% of sqlx raw** throughput on single-row CRUD
  and `jsonb_roundtrip`.
- **vs Node db-model-router** (reference): â‰¥ **2Ã—** throughput or â‰¤ 50% median
  latency on `read_pk` and `insert_single` is expected (document actual).
- Any scenario > **20% regression** between releases fails CI and requires an
  ADR documenting cause + fix, or re-baselining via benchmark improvement.
- Benchmarks never silenced test failures; results are data, not "pass/fail",
  except the Â§38.6 regression gates.

### 38.7 Implementation checklist

- `benchmarks/` workspace crate: `Cargo.toml` with feature `bench-...` per
  contender (or one binary per contender, zero shared magic).
- `benchmarks/run.sh` orchestrating docker-compose â†’ migrate â†’ seed â†’ bench â†’
  report; `PG_*` env vars mirroring `benchmark/lib.js` (`PG_HOST/PORT/USER/
  PASSWORD/PG_DB`, default `localhost/5432/postgres/password/test_db`).
- CI job `bench` (nightly, not PR-blocking): artifact = HTML report + JSON;
  comparison table vs previous run and vs Node reference.
- Documentation: `benchmarks/README.md` explains methodology, caveats, and
  how to reproduce locally, plus pinned contender versions (bump only via ADR).
