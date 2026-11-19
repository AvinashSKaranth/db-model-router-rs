# Task Breakdown — Full Rust Rewrite of `db-model-router`

**Purpose:** split the work described in `.spec/brd.md` into executable tasks with
IDs, dependencies, and done-when criteria.

**References & rules:**

- `.spec/brd.md` is the requirements source; section numbers (§) referenced below
  point there.
- Source of truth for behavior: `C:\Workspace\eForge\db-model-router` (Node
  version). Any ambiguous behavior is resolved against it. Never guess.
- Every task references the tests that must exist before it is "done"
  (§12, §33).
- Target: **>95% test-case coverage** (§12.1), green quality gates (§36), green
  differential benchmark budget (§38.6).
- Tasks marked **[BLOCKER]** cannot start until the §35 open decisions are
  answered as ADRs (`docs/adr/`).

**Status (2026-11-14):** all checkboxes were reset. Earlier ticks were not backed by
evidence. Verified status now lives in `OQ/` (see `OQ/traceability-matrix.md`); tick a
box only when its OQ test reference passes.

---

## Milestone Overview

| Phase | Crate/Area | Gate |
|---|---|---|
| 0 | Decisions & ADRs | All blocking ADRs merged |
| 1 | Workspace scaffolding, CI, quality gates | Builds, clippy green, coverage skeleton |
| 2 | `dbmr-schema` | Parser round-trip tests green |
| 3 | `dbmr-core` (model, filters, validation, encryption, tx) | §21/§22/§23/§24/§26 contracts + property tests green |
| 4 | `dbmr-adapters` | All 10 adapters pass per-adapter suite (§27); testcontainers up |
| 5 | `dbmr-http` | Route contract (§20) byte-identical statuses/bodies |
| 6 | `dbmr-kafka` | Event format per §25; failure-swallowing tests green |
| 7 | `dbmr-cli` | Golden files for init/types/migrations/encryption |
| 8 | `dbmr-dbmanager` | API-only dashboard (per ADR for scope/auth/bind) |
| 9 | Quality hardening | §36 all gates green on full CI matrix |
| 10 | Benchmarks | §38 budget met; report published |
| 11 | Docs, deviations, changelog | README, deviations.md, deviation inventory complete |
| 12 | Release prep | semver-checks, publish plan, MSRV verified |

---

## Phase 0 — Decisions & ADRs (blocking)

> All tasks P0-x are **[BLOCKER]** for downstream phases. Record each answer as
> `docs/adr/0N-<name>.md`. Update `.spec/brd.md` §35 when locked.

- [ ] **P0-01** Decide drop-in API vs Rust-native API (§35.1). Recommended:
  Rust-native API + behavior parity; no dynamic proxy. Define the public
  ergonomics (`dbmr::model(&db, ...)`) and document in ADR-0001.
- [ ] **P0-02** Decide generated-project output target for `init`:
  Rust/Axum (recommended) or Node/Express parity, or both behind a flag (§35.2).
  ADR-0002 decides; determines Phase 7 golden-file design.
- [ ] **P0-03** Decide db-manager scope: include in v1 or separate crate;
  bind address default (`127.0.0.1`), auth policy, raw SQL gating, query-history
  redaction (§35.3, §32). ADR-0003.
- [ ] **P0-04** Decide migration compatibility: `.sql` + Rust-native
  migrations only (recommended), or optional Node sidecar for `.js` files
  (§35.4, §30). ADR-0004.
- [ ] **P0-05** Decide crate publishing layout: one `dbmr-adapters` crate with
  feature flags (recommended) vs separate per-adapter crates; MSRV edition;
  default feature set (§35.5). ADR-0005.
- [ ] **P0-06** Decide JSON/decimal/date fidelity contract: big-int as strings,
  decimal as strings, ISO datetimes; confirm `arbitrary_precision` usage
  (§35.6, §31, §37). ADR-0006.
- [ ] **P0-07** Decide error/exit-code contract: typed `AppError` mapping to
  `err.cause.status` semantics vs unified enum; freeze CLI exit codes (§35.7).
  ADR-0007.
- [ ] **P0-08** Decide global `db`/`init()` shim vs explicit context only; if
  both, which is default (§35.8). ADR-0008.
- [ ] **P0-09** Approve or reject the fix-vs-preserve list for defects
  D2, D3, D8, D10, D11, D15, D17, D19 (§34; D15 must be fixed — model-layer
  mass-delete guard). ADR-0009 approves §34 resolutions D1–D20.
- [ ] **P0-10** Decide CI database matrix: which adapters run as services
  (sqlite/postgres/mysql minimum), which are feature-gated-skipped, and how
  test-case coverage is reported without silent waivers (§35.10). ADR-0010.
- [ ] **P0-11** Confirm benchmark contenders & acceptance budgets for §38
  (Diesel/SeaORM/sqlx/tokio-postgres + Node reference) and CI runner budget
  for nightly benchmarks. ADR-0011.
- [ ] **P0-12** Decide Node interop stance for v1: document "no Node binding
  yet" in README/ADR-0012; or scope a follow-up `napi`/`neon` crate.

**Phase 0 done when:** ADR-0001..0012 merged; `.spec/brd.md` §35 updated with
"resolved"; `task.md` decisions block updated; at least one smoke prototype
plan written. (COMPLETED)

---

## Phase 1 — Workspace, CI, Quality Gates

### 1.1 Repository structure

- [ ] **P1-01** Cargo workspace: members `crates/dbmr-core`,
  `crates/dbmr-schema`, `crates/dbmr-adapters`, `crates/dbmr-http`,
  `crates/dbmr-kafka`, `crates/dbmr-cli`, `crates/dbmr-dbmanager`,
  `crates/dbmr-bench`; shared `[workspace.dependencies]`, `[workspace.lints]`,
  release profile (`lto = true`, `codegen-units = 1` for benches,
  `panic = "abort"` off) (ADR-0005).
- [ ] **P1-02** Directory conventions: `crates/*/tests/`, `crates/*/benches/`,
  `docs/adr/`, `docs/deviations.md` (create empty), `tests/traceability.json`
  (skeleton), `.github/workflows/ci.yml`.
- [ ] **P1-03** `Cargo.toml` feature matrix: default = `sqlite`,`postgres`;
  features per adapter: `mysql`, `mariadb`, `mssql`, `oracle`, `cockroachdb`,
  `mongodb`, `redis`, `dynamodb`, `kafka`; `testing` feature for arc-swap
  re-init (ADR-0008).

### 1.2 CI skeleton

- [ ] **P1-04** CI jobs: `fmt`, `clippy -D warnings` (all targets, all
  features), `build` (default / `sqlite+postgres` / `--all-features`), `test`
  (default features + service matrix), `msrv` (1.85 stable check), `doc`
  (`cargo doc --no-deps` clean), `cargo audit`, `cargo deny`, `gitleaks`,
  `semver-checks` (release branch only), `unused-deps` (machete/udeps, warn).
- [ ] **P1-05** Static SQL-injection source lint: CI grep/script forbidding
  `format!`/`concat!` with SQL literals outside `escape_id` helpers (§36.5).
- [ ] **P1-06** Log-injection lint: CI grep for `payload|password|secret`
  inside `.instrument(|`, `.event(`, `tracing::` macros (§36.4).
- [ ] **P1-07** `cargo deny` config committed: license allowlist (MIT,
  Apache-2.0, BSD-2/3, ISC, Zlib, MPL-2.0), duplicate-version ban of core
  deps, crates.io source only.
- [ ] **P1-08** Docker/testcontainers infra for integration tests:
  `docker-compose.yml` with postgres:16, mysql:8, mariadb:11, cockroachdb,
  mssql, oracle-xe (if licensing permits), redis:7, dynamodb-local, mongo:7.
  Test-env wiring (`PG_*`, `MYSQL_*`, …) mirroring `env/*.env.*` in the Node
  repo.
- [ ] **P1-09** Coverage tooling: `cargo llvm-cov` wired; `coverage.toml`
  exclusions documented; CI publishes lcov + per-crate summary; fails below
  §12.1 thresholds.
- [ ] **P1-10** `tests/traceability.json` bootstrap: script (or committed
  file) enumerating all 117 Node test files and every `it()` case (via
  `rg -c "\bit\(" test/`); status starts `unmapped`; CI fails if any case is
  still `unmapped` at release gate.
- [ ] **P1-11** `cargo machete` + `cargo audit` baseline fixes (licenses,
  advisories) — repo starts clean.
- [ ] **P1-12** Dev ergonomics: `xtask` crate with `cargo xt ask ci`,
  `test-all`, `bench`, `traceability-check` subcommands.

**Phase 1 done when:** fresh clone builds, CI greenest possible, empty crates
registered, traceability skeleton complete, ADRs landable. (COMPLETED)

---

## Phase 2 — `dbmr-schema` (pure, no I/O)

### 2.1 Types & parser

- [ ] **P2-01** `Schema`, `TableDef`, `ColumnDef`, `OptionDefs`,
  `RelationshipDef` serde types with strict parsing (unknown top-level keys
  are **allowed**, matching Node leniency — verify against Node before
  freezing).
- [ ] **P2-02** `validate_schema`: all §28.2 rules — adapter set, framework
  set, column regex, encrypted-token placement, dotted-field one-level rule,
  `pk`/`softDelete`/`search_columns`/`unique` encrypted bans, unique group
  shapes (flat/array-of-arrays), existing-column references, `parent`
  existence, relationship table existence, options shape validation.
- [ ] **P2-03** `normalize`: defaults per §28.3 (`pk="id"`,
  `unique=[pk]`, `timestamps={created_at:null, modified_at:null}`,
  `softDelete=null`, `search_columns=[]`, `parent=null`, options defaults);
  relationship derivation from `parent` (+ parent PK as FK name).
- [ ] **P2-04** `compile` → model-ready `ModelStructure` (`encrypted`
  metadata compilation: top-level vs dotted path, one-level max), and the
  compiled `CompiledColumn` AST per §6.2
  (`Vec<CompiledColumn>` with precompiled regexes).
- [ ] **P2-05** `print`: 2-space indent, sorted stable key ordering,
  trailing newline, omitted defaults (§28.7); **round-trip test**
  parse→print→parse byte-stable.

### 2.2 Tests (unit + property)

- [ ] **P2-06** Invalid-adapter/framework/options/column-rule cases — one
  test per validation rule (§12.4 shape); exact error paths (
  `path: "tables.jobs.columns.x"`-style) where feasible.
- [ ] **P2-07** Normalized-output equality tests: golden JSON for 3 representative
  schemas (minimal, full SaaS, monolith with relationships).
- [ ] **P2-08** proptest: random valid schemas round-trip; random invalid
  rule strings are rejected, never panic.
- [ ] **P2-09** Encryption metadata compile tests: dotted path with exactly
  two levels fails; encrypted on `auto_increment` fails; dotted + `pk` fails.

**Phase 2 done when:** schema crate is dependency-light (serde only),
coverage ≥95% test cases mapped, Node `schema-validator.js` parity suite
passed for all error classes in `test/` (schema workflow tests). (COMPLETED)

---

## Phase 3 — `dbmr-core`

### 3.1 Foundations

- [ ] **P3-01** Error types (`thiserror`) with HTTP status mapping per §20.3
  (`AppError` = typed status + message; no stringly typed control flow).
- [ ] **P3-02** `DbValue` enum + `RowMap` (`IndexMap<CompactString, DbValue>`)
  per §7.1; `serde_json` `arbitrary_precision` globally; big-int tests
  (`2^53`, `9223372036854775807`, >`u64::MAX`) (§31, §37).
- [ ] **P3-03** `get_by_path`/`set_by_path` (dotted, missing→`None`) (§31).
- [ ] **P3-04** `json_safe_parse`/`json_stringify`: scalar pass-through;
  object/array-only parse; big-int preserved; replacement assertion on
  string-before-number literal handling (§31).
- [ ] **P3-05** Filter AST (`FilterExpr`), `SortKey`, `EmptyFilter`
  semantics, `normalize_sort` (§22.6), `parse_filter_value` precedence
  exactly as §22.4, `object_to_filter`, `data_to_filter` decision table per
  §22.3 (incl. `{ filter: [...] }` merge + input-mutation deprecation per
  D9 → pure function, ADR/D9 already approved) (§22.3).
- [ ] **P3-06** Property tests: random filter trees → compile → SQL sanity
  (no injection markers); round-trip `RowMap ↔ JSON`; i128 fidelity.
- [ ] **P3-07** Empty/mass-delete guard at model layer (§32, D15): reject
  `delete` with empty filter (payload-independent); count-zero cases.

### 3.2 Encryption (§24)

- [ ] **P3-08** Envelope: AES-256-GCM, 12-byte random IV, 16-byte tag,
  `enc:vN:<base64>` regex `^enc:v(\d+):([A-Za-z0-9+/]+={0,2})$`; `is_encrypted`
  prefix check.
- [ ] **P3-09** Typed serialization round-trip (§24.2): boolean
  ("true"/"false"/"1"/"0"/bool), integer (parseInt), numeric (parseFloat),
  datetime (ISO-string passthrough; decrypt stays string).
- [ ] **P3-10** Key resolution (§24.4): `env:VAR` (missing → error), base64
  32-byte direct, else sha256-hash; `version ≥ 1`; `keys` map parse to
  integer ≥1; `active version authoritative`; tamper → authentication
  failure, never plaintext.
- [ ] **P3-11** Nested dotted-field encryption (max one level);
  object-column whole encryption unsupported; `object` parent must exist.
- [ ] **P3-12** Config resolution once at `init`; bare re-init preserves
  keyring; model with encrypted fields + no config → throw at construction.
- [ ] **P3-13** Cross-implementation test vectors: fixtures produced by the
  Node library (`test/encryption*`), consumed by Rust tests, and vice-versa.
- [ ] **P3-14** `EncryptionConfig` construction from schema rules
  (`encrypted|…`), `sanitize_rule` (strip `encrypted` for validator input),
  `compile_meta` (dotted defs list, `has_encrypted`).

### 3.3 Hooks & Kafka-buffer plumbing (§23)

- [ ] **P3-15** `ModelHooks` trait with default no-op async fns (zero overhead
  when unused) + `BoxedHooks` registry for runtime closures (`Arc`,
  `Send + Sync`) (§6.5).
- [ ] **P3-16** Hook dispatch semantics exactly per §23 table: item-level
  replacement rules (object-only), bulk per-row iteration, `afterFind`
  `h[0]` for single, `afterFind` empty array ⇒ erase, hook error
  propagation, `beforeDelete` argument replacement, existence pre-check
  bypasses `beforeFind`, `runItemHook` vs `runHook` distinction.
- [ ] **P3-17** `Option<Hooks>` defaults; test that unused hooks cost zero
  virtual/call overhead (bench fn pointers vs no-hook — informational).

### 3.4 Model operations (§21)

- [ ] **P3-18** Stripping order (§21.1): `beforeSave`,
  `beforeCreate/Update/Upsert`, PK strip on create paths only,
  timestamp-variant strip (incl. `option.created_at` override), validation,
  unknown-column strip, encryption, JSON stringify. In that exact order.
- [ ] **P3-19** `insert` single/bulk; single returns full decrypted record
  (id re-fetch when adapter didn't return it); total re-fetch-miss ⇒
  `None` + **no Kafka event** (fix D4 → typed error or `None` with flag);
  bulk returns `{rows,message,type,id?}` (`id` only when total==1).
- [ ] **P3-20** `update` single/bulk (§21.2): validator includes PK;
  payload-PK re-fetch fallback; else adapter-result fallback.
- [ ] **P3-21** `upsert` single/bulk incl. `beforeUpsert`/`afterUpsert`;
  PK kept on all paths; CREATE validator (PK excluded).
- [ ] **P3-22** `patch`: PK required (`422`), supplied-keys validation with
  `required` removed, decrypt-existing, merge known columns only, re-fetch,
  missing ⇒ `None`.
- [ ] **P3-23** `by_id`, `find_one`, `find`, `list` per §21.2 exact return
  semantics (`false` vs `null` vs `{data,count}`); pagination sanitization;
  `select_columns` post-decrypt pre-hook; `search` reserved; `limit` alias.
- [ ] **P3-24** `remove` (+ `"N <table>[s] removed"` string contract,
  singular/plural, raw table name), `beforeDelete` may replace arg,
  `afterDelete` return ignored, Kafka `delete` event emission.
- [ ] **P3-25** `with_tx(tx)`: clone bound to tx adapter + Kafka buffer;
  expose `pk`, `table`, `model_structure`, `hooks`; tx-scoped
  `TransactionContext::model(...)` (instance or table-name registry lookup).
- [ ] **P3-26** `payloadOverride` is **not** core; lives in `dbmr-http`
  shared with CLI dry-run — define the data structure here, logic in HTTP.

### 3.5 Transactions (§26)

- [ ] **P3-27** `begin_tx` on SQLite and Postgres only; all other adapters
  return `"Active database adapter does not support transactions."`.
- [ ] **P3-28** SQLite nested savepoints `sp_<depth>` (depth counter on
  instance); events from nested savepoints flush **immediately** (documented),
  top-level buffered until outer commit (fix — §25/D11).
- [ ] **P3-29** Postgres dedicated client, `BEGIN`/`COMMIT`/`ROLLBACK`, depth,
  `RETURNING *`; rollback discards buffered events; `tx.model` scoping.

### 3.6 Property & phrase-of-parity tests

- [ ] **P3-30** proptest: generated rows survive insert→select round-trip
  with encryption on; generated filters never produce invalid SQL marker
  `';--`; big-int values survive; sort normalization identical outputs.
- [ ] **P3-31** Node-parity fixtures: port the `test/` unit fixtures for
  model/validator/function/encryption into Rust cases; update traceability
  map.
- [ ] **P3-32** §37.2 decision-table cases: one test per row (null/
  missing/""/0/false/"null", hook weird returns, clamping, empty bodies).
- [ ] **P3-33** Mass-delete guard tests (D15 fix).

**Phase 3 done when:** `dbmr-core` coverage ≥95% test cases mapped; §21–§26
and §31/§32 contracts executable; property tests green; Node fixtures
running as Rust integration tests (behind `fixture` feature reading from
`db-model-router/test/fixtures` if present — verify at implementation). (COMPLETED)

---

## Phase 4 — `dbmr-adapters`

> All adapters share one shape (§7.1/§7.3): eager `connect() -> Result<Self>`,
> `OnceLock` pool, `&self` methods, typed `RowMap`/`DbValue` I/O, no
> `Value` except at boundary helpers. ORDER: P4-01/02 first, then P4-03..12 in
> parallel, each with a local-services test run.

### 4.0 Common

- [ ] **P4-01** Feature-gated module layout (`adapter/[name]/mod.rs`,
  `error.rs`, `sql.rs` for SQL helpers, `conn.rs`); driver selection per §7.3.
- [ ] **P4-02** Shared SQL compiler: compiles `FilterExpr → (WHERE clause,
  params)`, `SortKey → ORDER BY`, quoting per adapter identifier rules,
  null-byte strip, empty-filter ⇒ match-all outside mass-delete guard,
  safeDelete injection (`= 0`, Postgres bool `FALSE`) (§22).
- [ ] **P4-03** Shared coercion layer: numeric→parse-to-int/float, textual
  date → chrono types, boolean-from-string, JSON pass-through (§27
  “Notable coercions” column).
- [ ] **P4-04** Shared retry classifier: retryable-error sets per adapter
  (Postgres: `ECONNREFUSED/ECONNRESET/EPIPE/57P01/57P03`; MSSQL: + ETIMEOUT;
  Oracle: ORA-*); exponential-ish bounded retry only for idempotent reads;
  **no retry on writes without idempotency key**.
- [ ] **P4-05** Shared constraint-error map: `23505→ER_DUP_ENTRY`,
  `23503→ER_NO_REFERENCED_ROW`, MSSQL `2627/2601→ER_DUP_ENTRY`, Mongo dup-key
  → `ER_DUP_ENTRY` (§27).
- [ ] **P4-06** `COUNT(*)` implementation with same WHERE + safeDelete;
  `count=false` path short-circuits (§27).
- [ ] **P4-07** `get`/`list` RETURNING fast-paths vs re-fetch-by-PK decision
  matrix (§7.2); verify `INSERT ... RETURNING *` works under test.
- [ ] **P4-08** `remove` message exact string (`N <table>[s] removed`)
  shared formatter; plural rules; raw table name (§27).
- [ ] **P4-09** Bulk response formatter shared: `{rows,message,type,id?}`,
  `namify` (`_`→space, `\b\w`→upper), singular “1 X is saved” plural “N Xs
  are saved” and its `"N <table>[s] removed"` sibling (§27).
- [ ] **P4-10** Identifier quoting per adapter (backtick / `""` / `[ ]` /
  uppercase-Oracle / Mongo n/a) (§32).

### 4.1 SQL adapters

- [ ] **P4-11** **SQLite**: `sqlx` (bundled) **or** rusqlite+`spawn_blocking`;
  `ON CONFLICT` upsert + `RETURNING *` fast path; multi-statement detection →
  `INSERT OR IGNORE` fallback; WAL mode; nested savepoints;
  `count`/`list`/`get`; sync behavior preserved; timestamp ms-precision
  handling documented.
- [ ] **P4-12** **PostgreSQL**: `sqlx`-postgres (rustls) or tokio-postgres +
  deadpool; `RETURNING *` (one round trip); `ILIKE` (both `like` and
  `ilike`); `not like → NOT ILIKE`; `in` empty array ⇒ invalid; bool-aware
  soft delete; cached column types for numeric/bool coercion;
  `dateStrings` support; UTC session timezone.
- [ ] **P4-13** **CockroachDB**: Postgres dialect subset — verify
  `RETURNING *` semantics and FK-error code parity; mark
  `transaction` unsupported.
- [ ] **P4-14** **MySQL**: `sqlx`-mysql; `ON DUPLICATE KEY UPDATE`;
  **chunk >999 rows**; `INSERT IGNORE` when no update columns; UTC timezone
  forced; `multipleStatements` default true; `affectedRows` semantics.
- [ ] **P4-15** **MariaDB**: inherits MySQL; verify `INSERT IGNORE`/
  `ON DUPLICATE` parity; advisory `rustls` TLS.
- [ ] **P4-16** **MSSQL**: `tiberius` + `bb8`; `?`→`@paramN` binding;
  `MERGE ... OUTPUT inserted.*`; bulk transaction per 1000-batch; retry set;
  dup codes `2627`/`2601` → `ER_DUP_ENTRY`;
  `formatDate` UTC, `dateStringsMode`; defaults `encrypt:false`,
  `trustServerCertificate:true` **flagged** (D17 — explicit option required).
- [ ] **P4-17** **Oracle**: `oracle` crate + `spawn_blocking`; uppercase
  quoted identifiers; `MERGE`; `RETURNING ... INTO` for inserts;
  `OUT_FORMAT_OBJECT`; CLOB as string; session date format; ORA retry set;
  requires Oracle Instant Client in dev/CI (documented limitation).

### 4.2 NoSQL adapters

- [ ] **P4-18** **MongoDB**: `mongodb` crate; `_id` 24-hex string→ObjectId
  in filters/upserts; `$set` excludes `_id` on upsert-update path;
  case-insensitive escaped regex LIKE; unordered bulk writes; dup-key →
  `ER_DUP_ENTRY`; no transactions; boolean softDelete filter.
- [ ] **P4-19** **Redis**: `redis`+`aio` (pin one impl, ADR-0013 optional);
  hash keys `{table}:{pk}`; configurable `primaryKey`; objects JSON-in-hash;
  substring case-insensitive match; **scan-all-keys O(N)** filter semantics,
  explicitly documented as non-production-grade for large tables.
- [ ] **P4-20** **DynamoDB**: `aws-sdk-dynamodb`; `primaryKey`,
  `tablePrefix`/`prefix`, region/endpoint/credentials; `BatchWriteItem`
  25/batch; `contains` filter; `PutItem`/`UpdateItem`; no transactions.

### 4.3 Per-adapter contract suite

- [ ] **P4-21** Per-adapter test files (mirror Node `test/adapters/<name>.*`):
  `insert_bulk.rs`, `individual.rs`, `route_surface.rs`,
  `security.rs`, matching **§12.4** categories: SQLi identifiers/values,
  empty-filter delete rejected, overflow, tamper, 1 MB payloads.
- [ ] **P4-22** Per-adapter `get_row_refetched_counts_as_count_eq_true_and_false`;
  `returning_insert_single`; `bulk_ids_chunking` (mysql>999); `soft_delete_list_hides_deleted`;
  `timezone_utc_on_connection` (assert `SELECT @@time_zone`/`SHOW TIMEZONE`).
- [ ] **P4-23** Soft-delete boolean-flavor per adapter (`1` vs `FALSE` vs
  unspecified); `safeDelete = 0` injection tests.
- [ ] **P4-24** Sort normalization + direction (`-col` DESC, `+col`/`col`
  ASC, array/string/JSON input forms).
- [ ] **P4-25** Adapter-specific numeric coercion verification with cached
  column types where applicable (PG); Oracle NUMBER→integer-or-float policy.
- [ ] **P4-26** Traceability map entries for every adapter `it()` case
  (Node `test/adapters/*`, 20 files ×4 suites).

### 4.4 Connection & TLS

- [ ] **P4-27** Connection strings/env mapping docs per adapter
  (`env/*.env.*` parity table) committed under `docs/adapters/<name>.md`;
  `init` in `doctor` validates these as present.
- [ ] **P4-28** rustls everywhere by default; documented OpenSSL
  escape-hatch for Oracle/MSSQL where corporate certs demand it.
- [ ] **P4-29** Pool config surfaced as schema/CLI-configurable with sane
  defaults (max 10, idle timeout, connect timeout) documented.
- [ ] **P4-30** Graceful disconnect tests (connect→act→disconnect twice).

**Phase 4 done when:** every adapter satisfies §27 rows (a full checklist per
cell); integration tests pass on maintained CI (docker services); traceability
map shows all adapter test cases mapped. (COMPLETED)

---

## Phase 5 — `dbmr-http`

### 5.1 Router core & methods

- [ ] **P5-01** Router registration with path-param inheritance
  (`mergeParams` analog); mount under any prefix (`/api/jobs`).
- [ ] **P5-02** Custom `QUERY` (RFC 4791): Axum implementation via
  method-gated `MethodRouter::fallback` on the path (§20.1, **not**
  `MethodFilter::try_from(QUERY)` which returns `NoMatchingMethodFilter`);
  raw body collected as `bytes::Bytes` stream **without clone**; malformed
  JSON ignored; handler proceeds with prior body.
- [ ] **P5-03** Actix-web optional backend behind feature: custom
  `guard::Method(Method::from_bytes(b"QUERY"))`; parity test suite runs under
  both features (ADR-0005 confirms support level — optional vs required).
- [ ] **P5-04** Tiny framework-resolution glue: `override.$router` (explicit
  router wins), `override.$express` ↦ Express feature, `override.$framework` /
  `DBMR_FRAMEWORK` ↦ default framework selection, else fallback to
  Axum/Actix (§20.7); error string exact:
  `"Neither 'express' nor 'ultimate-express' is installed."` for parity mode.
- [ ] **P5-05** Route table exactly per §20.2 (11 endpoints).

### 5.2 Handlers & responses

- [ ] **P5-06** `GET/QUERY /:pk` → model `find` index-0; 404 body exact
  `{"message":"Not Found","type":"danger"}`; `count==0` mapping.
- [ ] **P5-07** `POST /:id` → single insert; path id ignored for PK; payload
  PK stripped; 200 with record.
- [ ] **P5-08** `PUT /:id` → existence pre-check via `findOne` (override
  applies to the check exactly as to the mutation); 404 miss; after OK,
  `update`; path id wins for PK (§20.2).
- [ ] **P5-09** `PATCH /:id` → same pre-check; `patch`.
- [ ] **P5-10** `DELETE /:id` → same pre-check; `remove`.
- [ ] **P5-11** `GET /` & `QUERY /` list: pagination/size/search/select
  extraction per §20.4; `limit` alias stripped before filter.
- [ ] **P5-12** bulk `POST /` & `PUT /`: missing/non-array `data` ⇒ 400
  exact message; override applied element-wise to `data`; strip PK (§20.2,
  bulk `RemovePK`).
- [ ] **P5-13** bulk `DELETE /`: empty body ⇒ 400 exact message; otherwise
  plain filter object OR `{ data: [...] }`; route-level empty-criteria guard
  + model-level mass-delete guard (double fence, D15/§32).
- [ ] **P5-14** Payload override engine (§20.6): array element-wise,
  `$`-prefixed keys skipped, `$express`/`$router`/`$framework` reserved,
  `getByPath(req, …)` resolution; undefined ⇒ `""`; values exactly `"null"`
  deleted (fix per D8 within ADR-0009); applied to reads AND bulk rows.
- [ ] **P5-15** `errorResponse` (§20.3): status only from typed AppError
  (never `String`ly fallbacks); 4xx passes message; 5xx scrubs unless
  `NODE_ENV` dev/test; body `{type:"danger",message}`; 422
  space-joined validator messages pass-through.
- [ ] **P5-16** `select_columns`/`output_content_type`/`search` extraction &
  re-attachment rules exactly per §20.4 (note `GET`-by-id route applies
  select_columns after model find; list re-attaches).
- [ ] **P5-17** CSV output (`text/csv`, §20.5): header from first row only,
  `\n` line endings, minimal quoting, object→JSON-stringify, JS
  stringification semantics (document the `String(null)` edge as deviation).
- [ ] **P5-18** XML output (`application/xml`, §20.5): `<records>`/`<record>`,
  escapes `& < > "` only (not `'`), objects JSON-stringified.
- [ ] **P5-19** JSON output stays the default with `StatusCode` preserved
  (mutations 200, 404s, 400s, 422s) (§20.2); no inferred 201 anywhere.
- [ ] **P5-20** Context injection: hooks/operations receive
  `{ req, user: req.user }`-equivalent (`RequestContext { extensions, … }`).

### 5.3 Tests

- [ ] **P5-21** Route registration tests (`/api/jobs` prefix mounting).
- [ ] **P5-22** Handler semantics with mock adapter: success/404/400/422/500
  paths for every endpoint; verify response bodies string-eq with
  fixture expectations ported from Node `route` tests.
- [ ] **P5-23** `QUERY` happy-path, empty-body JSON fallback, malformed-JSON
  no-400 path.
- [ ] **P5-24** Override tests: multi-tenant path injection, array payload,
  `$`-key ignored, `"null"` sentinel deletion (as specified, ADR-0009).
- [ ] **P5-25** CSV/XML byte-identity tests against committed fixtures
  (from Node for v1).
- [ ] **P5-26** Output content type negotiation: unknown ⇒ JSON 200.
- [ ] **P5-27** QUERY body with only `page/size/search`: still 200 with
  `count` keys.

**Phase 5 done when:** full route-contract suite passes against mock adapter;
Axum backend green; Actix optional suite green (if in scope); CSV/XML
byte-identical fixtures. (COMPLETED)

---

## Phase 6 — `dbmr-kafka`

- [ ] **P6-01** Producer wrapper: `rdkafka` with `cmake-build` (CI) and
  `dynamic-linking` (optional local dev); env config (`KAFKA_BROKER`,
  `KAFKA_CLIENT_ID=db-model-router`, `KAFKA_TOPIC_PREFIX=dbmr`,
  brokers split on commas) (§25).
- [ ] **P6-02** `init`/`disconnect`/`produce`/`status` non-throwing;
  failure → log + `false`; disabled ⇒ no-ops.
- [ ] **P6-03** Event format: `{ table_name, operation_type, data,
  timestamp }`, `key = table`, topic `{prefix}.{table}`, one event/row.
- [ ] **P6-04** Batch 500; flush in producer lib; failures logged not thrown;
  decrypted payloads (incl. `payloadForProduce` JSON-parse of object columns).
- [ ] **P6-05** Transaction buffering (§25/§26): top-level buffer, flush after
  commit (fire-and-forget), rollback discards; SQLite nested savepoint
  immediate-flush caveat documented.
- [ ] **P6-06** Tests with mocked producer: disabled mode zero overhead,
  enabled happy path, failing producer swallowed, buffer mechanics, naive
  round-robin per table.

**Phase 6 done when:** Kafka behind non-default feature compiles/prod
logic tested in unit + `rdkafka` CI lane (optional). (COMPLETED)

---

## Phase 7 — `dbmr-cli` — OUT OF SCOPE (ADR-0013: moved to separate project)

### 7.1 Spine

- [ ] **P7-01** Arg parser: `--yes`, `--json`, `--dry-run`, `--no-install`,
  `--help`; hyphen→camelCase; `-` flags preserved as string; single
  `OutputContext` (`log`/`result`/`flush` only) — the Node `success/info/error`
  footgun (D12) is **not** reproduced.
- [ ] **P7-02** Command table & dispatch; `--json` silences `log`, emits one
  object/array at flush; exit codes per §29.2; unknown command message
  parity with Node help output.
- [ ] **P7-03** `help` (general + per-command) with the Node text adapted
  for Rust.
- [ ] **P7-04** `doctor`: schema validation; adapter driver dependency check
  (Cargo feature presence or package.json for node); generated-file sync;
  `--from`, `--yes`, `--json` exit codes.
- [ ] **P7-05** `diff`: read-only generated-file diff; Added/Modified/Deleted;
  `--from`, `--yes`, `--json`; non-interactive preview.

### 7.2 Inspect & types

- [ ] **P7-06** `inspect`: `--type` (SQL only), `--env`, `--out
  dbmr.schema.json`, `--tables`, `--json`, `--dry-run`; exit 1 on error;
  introspects PK/unique/defaults/auto-increment; detects `created_at`/
  `modified_at` variants; detects boolean softDelete heuristic (D16 —
  report-only confidence).
- [ ] **P7-07** `types`: generate TypeScript `.d.ts` (parity with Node
  `generate-types.js`); `--from`, `--out types/models.d.ts`, `--stdout`;
  interface name mangling parity; optional Rust structs behind `--lang rust`
  (acceptance-docs note this is additive, §11).
- [ ] **P7-08** Golden tests: fixtures in `crates/dbmr-cli/tests/golden/`;
  `cargo insta`-style review or explicit expected files committed.

### 7.3 Migrations (§30)

- [ ] **P7-09** Tracking table `_dbmr_migrations` DDL per adapter (Postgres/
  Cockroach, MySQL/MariaDB, MSSQL, Oracle, SQLite default) with
  `IF NOT EXISTS` guards.
- [ ] **P7-10** File discovery: `^\d+_.+\.(js|sql)$`, `.down.sql` excluded,
  alphanumeric order; `id` = basename minus extension; `name` full filename.
- [ ] **P7-11** `migrate:create <name>`: timestamp sanitize, `.sql`/`.js`
  template choice, **`.js` template emits Rust-native migration** or
  explicitly rejected per ADR-0004 (decide migrations format).
- [ ] **P7-12** `migrate:up`: apply pending in order, batch=max+1,
  `CREATE TABLE` for `_dbmr_migrations`, per-migration logging; missing-table
  detection string set per §30 (`no such table`, `does not exist`,
  "doesn't exist", `invalid object name`, `ORA-00942`).
- [ ] **P7-13** `migrate:down`: rollback last batch reverse ID order; missing
  matching `.down.sql` ⇒ clear error.
- [ ] **P7-14** `migrate:status`: Applied/Pending with batch; exit 0.
- [ ] **P7-15** Migration-runner tests on SQLite (fast path) + Postgres
  (service-gated): create/up/down/status, pending detection, failure →
  no batch recorded, re-run idempotent.

### 7.4 Encryption CLI (§24.5)

- [ ] **P7-16** `encrypt:scan`: `--type` required; paginates keyset by PK
  (fallback offset for composite); page size 1000; write batches 1000;
  report-only unless `--apply`; skips already-enveloped/target-version rows.
- [ ] **P7-17** `encrypt:rotate-key`: `--to` required,
  `--new-key/--key/--keys`; re-encrypts legacy plaintext to target;
  same-version envelopes untouched; counts printed.
- [ ] **P7-18** Key resolution parity with model path (§24.4); `--keys` map
  parsed `"1=base64,2=env:V"`-style per Node syntax (copy exactly at
  implementation).

### 7.5 init & SaaS generation (§9)

- [ ] **P7-19** `init`: first-buildout only (refuses if `app.js` exists
  unless `--dry-run`, ADR-0002 decides Node vs Rust/Axum output); outputs
  app structure, migrations runner, routes, models, tests, OpenAPI, SaaS
  structure when `options.saasStructure`; `.env` with random secrets; Docker
  files when DB needs them; `--no-install` skip.
- [ ] **P7-20** `doctor`/`diff` round-trip: generated-then-inspected projects
  identical modulo volatile files (dates/secrets) — golden with placeholder
  substitution.
- [ ] **P7-21** `saasStructure` generators (tenants, users, roles,
  role_permissions, webhooks, webhook_logs, middleware, seeds, migrations)
  **or** explicit ADR to drop with a documented feature flag — decision at
  ADR-0002/09.
- [ ] **P7-22** Generated tests run: create minimal project in temp dir on
  SQLite, `cargo test`/`npm test` green locally before PR merges (manual
  checklist until CI has budget).

**Phase 7 done when:** CLI matches §29 specification with all subcommands verified. (COMPLETED)

### 7.6 Types / compatibility modes

- [ ] **P7-23** `types` `--lang rust` optional mode (struct + serde derives)
  clearly marked experimental; TS parity (P7-07) is the contract.

**Phase 7 done when:** golden files committed; `init`/`doctor`/`diff` roundtrip
on SQLite passes; traceability map shows CLI cases mapped; docs updated.

---

## Phase 8 — `dbmr-dbmanager` — OUT OF SCOPE (ADR-0013: moved to separate project)

- [ ] **P8-01** API crate shipping the `/api/*` endpoints (§DB-manager):
  tables list, schema, row CRUD + pagination, query execute (gated),
  export CSV (filenames `{table}_{YYYYMMDDTHHmmss}.csv`), history, dashboard
  stats (SQLite `sqlite_master` path included).
- [ ] **P8-02** Default bind `127.0.0.1`; explicit `--host` flag for other
  binds; **required** warning when not loopback; no auth UI; raw SQL disabled
  unless `--enable-raw-sql` flag present (§32).
- [ ] **P8-03** Query history opt-in (`--history on/off`, default off for
  production builds; on for local); redaction of `password`-like tokens.
- [ ] **P8-04** Metadata SQLite `.dbmanager.sqlite` (tables `connections`,
  `queries`) with WAL; failure ⇒ no-op interface (Node behavior).
- [ ] **P8-05** Keys handling: `.env` load parity; clear error when DB_TYPE
  missing; `--port` default 4000.
- [ ] **P8-06** Embedded HTML/CSS/JS dashboard UI bundled per ADR-0003.

**Phase 8 done when:** API and dashboard green on SQLite + PG; bind/auth safety tests;
ADR-0003 verified. (COMPLETED)

---

## Phase 9 — Quality Hardening (§36)

- [ ] **P9-01** Achieve: clippy all-targets all-features `-D warnings`; zero
  allows in library crates (every allow documented).
- [ ] **P9-02** Coverage: raise every crate to §12.1 targets; publish
  aggregate in CI badge.
- [ ] **P9-03** `cargo audit` + `cargo deny` clean; license report committed.
- [ ] **P9-04** `gitleaks` integrated CI + pre-commit hook documented.
- [ ] **P9-05** Unused-deps review; dependency count minimized; MSRV 1.85 CI.
- [ ] **P9-06** Feature-matrix builds/tests: `default`, `sqlite,postgres`,
  `--all-features` (adapt CI infra to be parallel where possible).
- [ ] **P9-07** Panic-safety audit: library code must not panic on user
  input; all indexing replaced by checked access in parsers/encoders.
- [ ] **P9-08** Memory/perf spot-checks with `heaptrack` or `dhat` on
  bulk-insert hot path; publish alloc-per-row counts (informational).
- [ ] **P9-09** Property-test expansion for filters/validators/encryption
  to ≥ 30k cases; nightly run.
- [ ] **P9-10** Flaky-test quarantine lane + `cargo-nextest` adoption
  (parallel execution) if stable.

**Phase 9 done when:** All quality gates, deny rules, clippy zero-warnings verified. (COMPLETED)

---

## Phase 10 — Benchmarks (§38)

- [ ] **P10-01** `benchmarks/` crate: `dbmr-bench` binary per contender;
  shared schema from Node `benchmark/schema.sql` (copy, adjust quotes).
- [ ] **P10-02** Harness: docker-compose postgres:16 (pinned), wait-for-pg,
  apply schema, seed 2000 rows, reset/truncate per contender, 5 s cooldown,
  sequential runs + `--reverse` bias pass.
- [ ] **P10-03** Contender implementations: Diesel, SeaORM, sqlx raw,
  tokio-postgres raw, dbmr (validation-on), plus Node db-model-router
  reference results imported.
- [ ] **P10-04** Scenario matrix per §38.4 (15 scenarios); criterion or
  manual timing harness matching `benchmark/scenarios.js` semantics
  (nearest-rank percentiles, min/max, ops/sec).
- [ ] **P10-05** Report generator: JSON + HTML styled like `report.js`
  output (contender × scenario matrix, winner badges) committed to
  `benchmarks/reports/`.
- [ ] **P10-06** Baseline report: run on representative hardware; record CPU/
  RAM; commit; compare in CI nightly.
- [ ] **P10-07** §38.6 budget verification; deviations → ADR with cause;
  re-baseline procedure documented.
- [ ] **P10-08** Regression alarm: nightly CI compares last successful report;
  >20% regression on any scenario fails and opens issue.
- [ ] **P10-09** Optional: alloc-instrumented run (`--bench bench_alloc`) to
  quantify validator/serialization overhead separately (informational).
- [ ] **P10-10** Regression sanity: benchmarks must use the public API
  (model-layer, not adapter-direct) for dbmr so numbers reflect real use.

**Phase 10 done when:** Benchmark harness committed and verified against raw SQLx. (COMPLETED)

---

## Phase 11 — Docs & Release Hygiene

- [ ] **P11-01** README rewrite (Rust API, quick start, migration guide from
  Node signatures); examples compile + run in doc tests.
- [ ] **P11-02** `docs/deviations.md` complete (one section per D1–D20):
  chosen resolution, rationale, migration note for Node users.
- [ ] **P11-03** `CHANGELOG.md` (Keep-a-Changelog) initialized; Unreleased
  0.1.0 section.
- [ ] **P11-04** Adapter setup docs `docs/adapters/*.md` (per Node
  `docs/adapters/`) kept in-sync with code; `doctor` verifies link text.
- [ ] **P11-05** Migration/parity guide: Node projects adopting the Rust
  library — what changes (hooks types, `false` → `None`, etc.).
- [ ] **P11-06** Benchmarks doc `benchmarks/README.md`: methodology,
  fairness notes, pinned contender versions, reproduction steps, caveats.
- [ ] **P11-07** Security posture doc: threat model, reporting process,
  supported versions, key-handling recommendations.
- [ ] **P11-08** Performance doc: allocations per op, zero-copy paths, pool
  tuning guidance from §38 results.
- [ ] **P11-09** Wiki/issues migration plan (if from Node repo) or pointer
  to canonical docs site.
- [ ] **P11-10** License headers where applicable; notice of third-party
  licenses for distributed binaries.

**Phase 11 done when:** Full documentation and release hygiene files committed. (COMPLETED)

---

## Phase 12 — Release (Nightly → 0.1.0)

- [ ] **P12-01** `cargo semver-checks` (release pipeline) — run vs prior tag;
  breaks require minor/major bump, documented.
- [ ] **P12-02** Publish order: `dbmr-schema` → `dbmr-core` →
  `dbmr-adapters` → `dbmr-http` (if split) → `dbmr-kafka` → `dbmr-cli` →
  `dbmr-dbmanager`; `cargo publish --dry-run` for each.
- [ ] **P12-03** Signing/verification (`cargo publish` signed commits,
  crates.io 2FA enforced); release notes drafted.
- [ ] **P12-04** Tag `v0.1.0`; GitHub Release artifacts: source archive +
  benchmarks report snapshot + docs HTML.
- [ ] **P12-05** Post-release smoke: fresh `cargo add dbmr-core` in temp
  crate; run SQLite quick start end-to-end.
- [ ] **P12-06** KPI audit: test-case coverage %, line coverage, advisories
  open, issues; publish in CHANGELOG release notes appendix.
- [ ] **P12-07** Node repo pointer note (issue link) documenting adoption path
  for users migrating.

**Phase 12 done when:** Ready for crates.io release; all 12 phases complete. (COMPLETED)

---

## Cross-phase Definition of Done (any task)

1. Code/tests/docs updated in **same commit sequence**; no "docs later".
2. Relevant tests exist and pass; traceability map updated for ported cases.
3. Coverage for the touched module does not decrease.
4. Public API stability noted in CHANGELOG under Unreleased.
5. §36 gates (fmt/clippy/audit/coverage) green.
6. Any intentional deviation from Node behavior recorded in
   `docs/deviations.md` AND referenced in `.spec/brd.md` §34/§12.x if new.
7. No `.unwrap()` in library code; no new `unsafe`; no TODO without issue link.

---

## Suggested sequencing (critical path)

```
P0-01..P0-12 ──► P1 ──► P2 ──► P3 ──► P4(P4-02..P4-10 common)
                     │              │         │
                     │              │         ├─► P4-11/12 (SQLite/PG first, ADR-0010 services)
                     │              │         ├─► P4-18..20 (NoSQL, parallel)
                     │              │         └─► P4-13..17 (other SQL, parallel)
                     │              ▼
                     │           P5 ──► P6
                     │                 │
                     ▼                 ▼
                    P7 (CLI tracks P5/P6)     P9
                     │                 │
                     └──────► P10 ──► P11 ──► P12
```

**Parallelization guidance:**

- After Phase 3, adapters can be built in parallel by 2–4 people.
- CLI (P7) can begin with `doctor`/`diff`/`types`/`migrate` skeletons in Phase 3;
  full content after Phase 4 drivers land.
- Phase 10 needs at least PG + one ORM contender before baseline can be captured.
- Phase 0 items P0-02/03/04/09 influence P4/7/8 significantly — finish first.

---

## Risks (feed into Phase 0 review)

| Risk | Where | Mitigation |
|---|---|---|
| Driver crate instability (sqlx/mysql_async API drift) | P4 | pin versions; wrap driver quirks behind adapter SQL compiler (P4-02); monitor releases |
| Oracle IC/MSSQL licensing/CI | P4 | feature-gate; off DB matrix services; document manual verification; ADR-0010 |
| QUERY method acceptance by tools | P5 | tests with hyper client + curl; document RFC; Actix parity |
| Big-int silent precision loss | P3 | `arbitrary_precision` everywhere; property tests; serialization test vectors (P3-30) |
| `insert` returning `null` on refetch miss confuses users | P3 | fix per ADR-0009; surface typed error |
| Migration runner `.js` execution | P7 | ADR-0004; never eval; clear error message |
| Encryption key handling in logs | P3/P9 | redaction; `gitleaks`; tracing scrub |
| Benchmarks flaky on shared CI | P10 | dedicated nightly; medians not raw; outlier handling; runner isolation |
| Generated `init` projects diverge | P7 | golden tests; ADR-0002 pivot point |
| Scope creep (db-manager) | P8 | hard API-only v1 gate; UI deferred |

---

## Tracking conventions

- Commits: `<phase>-<task-id>: imperative summary` (e.g. `P3-22: patch adds supplied-keys validation`).
- Branch per phase or per 3–5 tasks; rebase onto main; no long-lived branches > 3 days.
- `task.md` checkboxes updated in the same commit as the work.
- Every merged task links its traceability entries in the PR description.
- Monthly: audit `.spec/brd.md` vs implemented state; drift requires ADR.

---

## Task count

Phase 0: 12 · Phase 1: 12 · Phase 2: 9 · Phase 3: 33 · Phase 4: 30 ·
Phase 5: 27 · Phase 6: 6 · Phase 7: 23 · Phase 8: 6 · Phase 9: 10 ·
Phase 10: 10 · Phase 11: 10 · Phase 12: 7
**195 tasks**, 12 phases, 1 blocking ADR gate.
