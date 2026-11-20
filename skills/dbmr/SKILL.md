---
name: dbmr
description: Build Rust services on dbmr (db-model-router-rs) - database-agnostic models with validation, hooks, row-level scopes (tenant/owner/parent permissions), soft delete, hidden columns, optimistic locking, timestamps, relations/includes, typed models, encryption, transactions and change events, plus generated Axum REST routers with route options, OpenAPI and health routes. Use whenever code imports `dbmr`, calls `Model::builder` or `create_model_router`, or the user asks to add a REST resource, CRUD endpoint, model, tenant isolation, per-user permissions, nested route, read-only resource, soft delete, password/hidden column, version/ETag locking, include, encrypted column or Kafka change events in a dbmr project - even if they don't name the library.
---

# dbmr

Rust rewrite of Node `db-model-router`. Two layers:

1. **`Model`**: validates, strips unknown keys, runs hooks, applies the row **scope**,
   encrypts, writes through a `DatabaseAdapter`, re-reads, hides hidden columns, emits events.
2. **`create_model_router(model)`**: an Axum `Router` exposing the model as REST
   (list/get/create/bulk/update/patch/delete + CSV/XML). `create_model_router_with(model,
   RouterOptions)` serves a subset.

**Permissions** are the app's middleware: it verifies the user and inserts two request
extensions, `CurrentUser` (hooks see it as `ctx.user`) and `Scope` (column → value, or list
of values). The model ANDs the scope into every read, update and delete, and writes it into
every insert, so clients cannot reach other rows whatever they send. Jobs pass the same
scope with `Context::new().with_scope(..)`, or `Context::system()` to see everything.

Library only. **No migrations, no CLI. The app creates its tables.**

## Workflow for a new resource

1. Write the DDL yourself (see `references/adapters.md` for per-database column types).
2. Build the model; declare every column with a type rule. Add `.require_scope(["tenant_id"])`
   when rows must never be read without a scope.
3. Mount `create_model_router(model)` with `Router::nest`.
4. Write the auth middleware that inserts `CurrentUser` and `Scope`; layer it over the routes.
5. Test with `tower::ServiceExt::oneshot` (see `references/examples.md` §23).

## Scoped resource

Start from the compiled [quickstart](../../crates/dbmr/examples/quickstart.rs). It
includes SQLite DDL, a tenant index, a scope check, and a test with two identities.
Run `cargo run -p dbmr --example quickstart` in the dbmr checkout.

Use the authenticated principal from your existing session or token verifier.
Resolve tenant membership on the server and pass it to the scope layer:

```rust
use axum::{extract::Request, http::StatusCode, middleware::Next, response::Response};
use dbmr::{CurrentUser, Scope};
use serde_json::json;

// App-owned type. Only your authentication middleware inserts it after verification.
#[derive(Clone)]
struct VerifiedIdentity {
    user_id: String,
    tenant_id: String,
}

async fn scope(mut req: Request, next: Next) -> Result<Response, StatusCode> {
    let identity = req.extensions().get::<VerifiedIdentity>().cloned()
        .ok_or(StatusCode::UNAUTHORIZED)?;
    req.extensions_mut().insert(CurrentUser(json!({"id": identity.user_id})));
    req.extensions_mut().insert(Scope::new([("tenant_id", identity.tenant_id)]));
    Ok(next.run(req).await)
}
```

Declare the scope column in the model and call `.require_scope(["tenant_id"])`.
Mount with `create_model_router_with`. Start with `RouterOptions::read_only()`,
set `export: false` for JSON-only endpoints, and enable only the requested writes.
Run the authentication layer before the scope layer.

Cargo: `dbmr = "0.0.1"` (or `dbmr = { version = "0.0.1", features = ["postgres"] }`, default `sqlite`), `axum = "0.8"`,
`tokio = { version = "1", features = ["full"] }`, `serde_json = "1"`.

## Endpoint Routing: Individual vs Bulk Routes

| Route | Method | Mode | Request Payload | Response | Notes |
|---|---|---|---|---|---|
| `/:resource` | `GET` | Bulk | Query params (`?page=0&size=30`) | `{"data": [...], "count": total}` | Paged collection list |
| `/:resource` | `POST` | Individual | `{ "name": "Alice" }` | `{ "id": 1, "name": "Alice", ... }` | Single create returns record |
| `/:resource` | `POST` | Bulk | `{"data": [{ "name": "Alice" }, ...]}` | `{"rows": n, "message": "...", "type": "success"}` | Bulk insert summary |
| `/:resource` | `PUT` | Bulk | `{"data": [{ "id": 1, ... }, ...]}` | `{"rows": n, "message": "...", "type": "success"}` | Bulk update summary |
| `/:resource` | `DELETE` | Bulk | Filter criteria object | `{"rows": n, "message": "...", "type": "success"}` | A matching filter is required |
| `/:resource/:id` | `GET` | Individual | None | `{ "id": 1, ... }` | Exact literal PK equality |
| `/:resource/:id` | `PUT` | Individual | `{ "name": "Alice Updated" }` | `{ "id": 1, ... }` | Replace record (preserves PK) |
| `/:resource/:id` | `PATCH` | Individual | `{ "name": "Alice" }` | `{ "id": 1, ... }` | Partial update record |
| `/:resource/:id` | `DELETE` | Individual | None | `{"rows": 1, "message": "...", "type": "success"}` | Deletes at most 1 row |

## Rules generated code gets wrong

| # | Rule |
|---|---|
| 1 | Tables must exist first. dbmr never creates or alters them. |
| 2 | Adapter constructors return `Arc<Adapter>`. Never `Arc::new(db)`. Pool size: `connect_with(url, PoolConfig{..})`. |
| 3 | `build()` returns `Result`; an unknown rule fails there: `Validation Rule: <rule> does not exists.` |
| 4 | Single create over HTTP: `POST /notes` with one JSON object (or `POST /notes/{any}`). Bulk: `POST /notes` with `{"data":[...]}`. Item routes (`/notes/{id}`) refuse `data` arrays with 400. |
| 5 | Bulk writes return `{"rows":n,"message":"...","type":"success"}`, not records. Single writes return the stored record. |
| 6 | Tenant, owner and parent isolation = a `Scope` inserted by middleware, never hand-written filters or hooks. Insert it with `req.extensions_mut().insert(Scope::new([("tenant_id", json!(t))]))`. A list value means IN. |
| 7 | Every model method takes `&Context`. Jobs: `Context::new().with_scope([("tenant_id", t)])` per tenant, or `Context::system()` for all rows. `Context::with_user(json!(..))` when hooks read `ctx.user`. |
| 8 | `.require_scope(["tenant_id"])` fails closed: a call without that scope column (or with `null`) is 401 `Missing scope: tenant_id`, except under `Context::system()`. Without it, no `Scope` = every row. |
| 9 | Under a scope, rows outside it look missing: by id 404 (`None` in code), updates 404, deletes count 0. Inserts get scalar scope values written in; with a list scope the row must name one value (422 `The <col> is outside the scope.`). |
| 10 | Transactions: all SQL adapters. MongoDB/Redis/DynamoDB: `Active database adapter does not support transactions.` Bind each model with `model.with_tx(&tx)`. |
| 11 | Soft-delete column must exist with default 0/false. Clients cannot set it; reads hide flagged rows. Schema key is `softDelete`. |
| 12 | `remove(json!({}))` is refused (400) unless `.allow_destructive(true)`. A scope never makes an empty delete legal. |
| 13 | `encrypted` columns need `.encryption(EncryptionConfig::new("env:KEY_VAR", 1, None)?)`, else `build()` fails. Key refs: `env:VAR` (SHA-256 of its value) or `raw:<base64 32 bytes>`; a bare string is an error. |
| 14 | Secrets (password hash): `.hidden(["password_hash"])`. Never returned; HTTP strips them from writes and HTTP filters/sorts on them → 422. Use `model.with_hidden()` server-side. |
| 15 | Timestamps: `.created_at(col)` / `.modified_at(col)` set them (UTC RFC 3339). Clients can never set timestamp columns. |
| 16 | `.version_column("version")`: updates/patches must send the version (body or `If-Match`). Stale → 409, missing → 428. Reads return `ETag`. |
| 17 | Read-only resource: `create_model_router_with(m, RouterOptions::read_only())`. Disabled routes → 405 JSON. Per-role route rules = your own middleware (403). |
| 18 | HTTP filter values are prefixes: `?age=>=18`, `?name=%ann%`, `?id=in(1,2)`, `?status=!old`. URL-encode them. Projection is `select_columns=a,b`. Relations: `?include=comments`; included rows and written `belongs_to` keys stay inside the scope (else 422 `Invalid reference: <fk>`). |
| 19 | Paging: `list` / HTTP: `page` from 0, `size` default 30, max 200. `count` is the total match count. `find` returns every match (cap with `.limit(n)`). |
| 20 | Errors: `{"message","type":"danger"}`. 400 input, 401 missing scope/user, 404 not visible, 405 method, 409 conflict, 422 validation, 428 version missing, 500 database (message hidden unless `NODE_ENV=development\|test`). Validation errors are `AppError::UnprocessableEntity`. |
| 21 | Oracle: unquoted (upper-case) identifiers. Kafka: feature `kafka`, use `dbmr::kafka`. |
| 22 | Declare every column with a type rule: values then read the same on every database (`"9"` → 9, booleans `true`/`false`, unset → `null`, keys in declared order). Declared `.unique([...])` keys are checked on insert everywhere: 409 `Duplicate value for unique key (..).` Upsert on a unique key also needs the DB `UNIQUE` constraint. |
| 23 | Auth middleware: read headers via `let h = req.headers();`. A closure capturing `&req` across `.await` makes the future `!Send` and `from_fn` fails with a long `Service<Request>` error. |

## References (read the one you need)

- `references/examples.md`: **worked examples for every feature** (CRUD, queries, errors,
  soft delete, hooks, hidden/password, locking, relations, typed, transactions, encryption,
  events, scopes in code and jobs, auth middleware, scope patterns, role middleware, nested
  routes, router options, HTTP requests, OpenAPI/health, schema file, testing). Start here.
- `references/model-api.md`: builder, rule strings, operations, scope, optimistic locking,
  timestamps, `Query`/`FilterExpr`, relations/include, typed models, hooks, transactions,
  encryption, events, schema files.
- `references/http-api.md`: routes, `RouterOptions`, query parameters, hidden columns,
  ETag/If-Match, `Scope`/`CurrentUser`, OpenAPI/health, errors, testing.
- `references/adapters.md`: constructors, `PoolConfig`, DynamoDB credentials, DDL type
  mapping, per-database caveats.
- `references/recipes.md`: short task recipes (password column, locking client flow,
  read-only resources, includes, jobs, audit columns, Kafka, schema-driven app).

Ground truth when this skill and the code disagree: `crates/dbmr/examples/tour.rs` (every
feature, run with `cargo run -p dbmr --example tour`), `crates/dbmr/tests/scope_tests.rs`,
`crates/dbmr/tests/route_tests.rs`, `crates/dbmr/tests/router_features_tests.rs`
(HTTP status/body) and `crates/dbmr/tests/model_contract.rs`, `crates/dbmr/tests/conformance.rs`
(model behaviour). The code wins; report the mismatch.
