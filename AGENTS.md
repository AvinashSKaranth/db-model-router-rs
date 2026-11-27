# AGENTS.md: building and maintaining dbmr (internal codebase contract)

> **Audience:** This document is the strict contract for AI agents **building, modifying, or fixing the `dbmr` library itself**.
> If you are an AI agent developing an application that **uses** `dbmr`, read [`skills/dbmr/SKILL.md`](skills/dbmr/SKILL.md) instead.
> The human-facing overview and quickstart is in [`README.md`](README.md).

---

## Codebase Architecture & Invariants

Following crate consolidation, all library modules live in `crates/dbmr/src/`:
- `core/`: Model definitions, validation rules engine, query/filter AST, encryption, row-level scopes, lifecycle hooks, and transactions.
- `schema/`: Declarative schema DSL parser, compiler, and validator.
- `adapters/`: Database adapters (SQLite, PostgreSQL, MySQL/MariaDB, MSSQL, Oracle, MongoDB, Redis, DynamoDB).
- `http/`: Axum router generation, REST CRUD handlers, OpenAPI 3.1 specification generator, and formatters.
- `kafka/`: Optional change data capture (CDC) event publisher (feature-gated behind `kafka`).

---

## Security & Correctness Invariants (MUST ALWAYS ENFORCE)

1. **Undeclared Scope Columns Rejected at Build Time**:
   - `ModelBuilder::build()` validates that every column named in `options.require_scope` is explicitly declared in `self.structure`.
   - If an undeclared column is supplied, `build()` fails with `AppError::Validation`.
   - The schema compiler and loader (`model_from_meta`) must carry `require_scope` from schema definitions.

2. **Item Route Path IDs Are Strict Literals**:
   - In `http/router.rs`, `req.id` from `/:resource/{id}` MUST be parsed once as a literal `DbValue` matching the declared PK type (integer or string).
   - Path IDs MUST NEVER be routed through `parse_filter_value` or general query filter parsing (which could misinterpret prefix characters like `>=` or `%` as operators).
   - In item `GET`, client query params must not override the PK filter; `q.limit` is set to `1` and `q.count` to `false` for efficiency.
   - In item `DELETE`, mutations MUST affect at most one row and use `FilterExpr::eq(pk, literal)`.

3. **Atomic Scoped Writes & Re-reads**:
   - In `core/model.rs`, optimistic locking updates (`locked_write`) MUST include `self.scope(ctx)?` directly in the SQL `UPDATE ... WHERE ...` statement.
   - Any re-fetch (`resolve_record`) MUST also append `self.scope(ctx)?` to the primary key filter.

4. **Hidden Columns Are Case-Insensitive & Trimmed**:
   - Column checks (`touches_hidden`) and stripping (`strip_hidden`) in `http/router.rs` MUST compare column identifiers in a trimmed, case-insensitive manner (`.trim().to_lowercase()`), preventing database collation or SQL compiler alias bypasses.

5. **Diagnostic Redaction for Sensitive Keys**:
   - `EncryptionConfig` in `core/encryption.rs` MUST NOT derive standard `Debug`.
   - It implements manual `std::fmt::Debug` redacting `active_key` to `"[REDACTED]"`.

6. **Individual and Bulk Route Parity**:
   - `create_model_router` MUST support both individual (`POST /resources` with `{...}`) and bulk (`POST /resources` with `{"data":[...]}`) writes.
   - The OpenAPI generator (`http/openapi.rs`) MUST document both the single-record and bulk-array request/response shapes via `oneOf`.

7. **DISCOVER Route Convention & Scaffolding Safety**:
   - Column filter discovery and autocomplete search use the custom HTTP method `DISCOVER` exclusively on the collection endpoint (`DISCOVER /:resource`).
   - It MUST NEVER be mounted or accepted on item routes (`/:resource/{id}`) or column-specific paths (`/:resource/{column}`); `DISCOVER` requests on item paths must return `405 Method Not Allowed`. This prevents scaffolding clashes when child tables or nested resources share names with columns.
   - Cardinality discovery probes up to 51 values: $\le 50$ distinct values return `mode: "fixed"` with the full enum list; $> 50$ returns `mode: "searchable"` requiring $\ge 3$ query characters.
   - For models with $\le 100$ columns, discovery is pre-warmed on application start and served from an in-memory cache, eliminating repetitive database queries for fixed column lookups.
   - Targeting hidden or encrypted columns returns `422 Unprocessable Entity`.
   - `OPTIONS` requests on collection and item routes return `200 OK` with RFC 9110 `Allow` headers dynamically reflecting enabled `RouterOptions`.

---

## Minimal Reference Implementations

- **Full Feature Tour**: [`crates/dbmr/examples/tour.rs`](file:///C:/Workspace/eForge/db-model-router-rs/crates/dbmr/examples/tour.rs) (`cargo run -p dbmr --example tour`)
- **Quickstart**: [`crates/dbmr/examples/quickstart.rs`](file:///C:/Workspace/eForge/db-model-router-rs/crates/dbmr/examples/quickstart.rs)
- **Blog Application Modular Project**: [`crates/dbmr/examples/blog_app/`](file:///C:/Workspace/eForge/db-model-router-rs/crates/dbmr/examples/blog_app/) (`cargo run -p dbmr --example blog_app`)

---

## Verifying the Codebase

Always run the full verification battery across the workspace before submitting changes:

```powershell
# 1. Code formatting
cargo fmt --all -- --check

# 2. Strict lints
cargo clippy --workspace --all-targets -- -D warnings

# 3. Security lints (SQL & log injection)
python scripts/ci_lints.py

# 4. Test suite
cargo test --workspace

# 5. Packaging dry run
cargo publish -p dbmr --dry-run
```

Key integration tests for reference patterns:
- [`crates/dbmr/tests/route_tests.rs`](file:///C:/Workspace/eForge/db-model-router-rs/crates/dbmr/tests/route_tests.rs): HTTP route request/response patterns.
- [`crates/dbmr/tests/router_features_tests.rs`](file:///C:/Workspace/eForge/db-model-router-rs/crates/dbmr/tests/router_features_tests.rs): Router options, OpenAPI, ETag/If-Match, hidden columns.
- [`crates/dbmr/tests/scope_tests.rs`](file:///C:/Workspace/eForge/db-model-router-rs/crates/dbmr/tests/scope_tests.rs): Row-level multi-tenant scope isolation.
- [`crates/dbmr/tests/model_contract.rs`](file:///C:/Workspace/eForge/db-model-router-rs/crates/dbmr/tests/model_contract.rs): Core model contracts and lifecycle hooks.
