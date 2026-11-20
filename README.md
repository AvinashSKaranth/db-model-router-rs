# dbmr (`db-model-router-rs`)

[![Crates.io](https://img.shields.io/crates/v/db-model-router.svg)](https://crates.io/crates/db-model-router)
[![Docs.rs](https://docs.rs/db-model-router/badge.svg)](https://docs.rs/db-model-router)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Database-agnostic **models** and generated **Axum REST routers** for Rust services. This is
the Rust rewrite of the Node.js `db-model-router`. HTTP behaviour matches Node except for
the deviations listed in [`docs/deviations.md`](docs/deviations.md).

This library covers models and routers only. The CLI, db-manager UI and migration runner
were moved to a separate project ([ADR-0013](docs/adr/0013-library-scope-model-and-router-only.md)),
so your application owns its DDL.

> Building with an LLM? Read [`AGENTS.md`](AGENTS.md) first. It is the short, exact
> contract. The full agent skill is [`skills/dbmr/`](skills/dbmr/SKILL.md).

## Adapter status

| Adapter | Feature | Status | Transactions |
|---|---|---|---|
| SQLite | `sqlite` (default) | **Verified**: full contract suite in CI | yes |
| PostgreSQL | `postgres` | **Verified live** (`conformance.rs`, 2026-11-15) | yes |
| CockroachDB | `cockroachdb` | **Verified live** | yes |
| MySQL / MariaDB | `mysql` / `mariadb` | **Verified live** | | yes |
| MSSQL | `mssql` | **Verified live** (TLS via native-tls) | | yes |
| Oracle (`oracle-rs`) | `oracle` | **Verified live**; use unquoted (upper-case) identifiers | | yes |
| MongoDB | `mongodb` | **Verified live** | no |
| Redis | `redis` | Experimental: verified live, full scan per query | no |
| DynamoDB | `dynamodb` | Experimental: verified on DynamoDB Local only. SigV4 signing is checked against the AWS test vector, not real AWS | no |

"Unverified" means the adapter compiles and has an `#[ignore]` live suite, but the suite has
not passed against a real server yet. See [`OQ/OQ-B-adapter-conformance.md`](OQ/OQ-B-adapter-conformance.md)
and [ADR-0016](docs/adr/0016-nosql-adapters-experimental.md).

Adapter constructors (`connect`, `connect_with`, `connect_host`, DynamoDB `new` /
`with_credentials` / `from_env`) return `Arc<Adapter>`, ready to pass to `Model::builder`.
`connect_with(..., PoolConfig { max_connections, min_connections, acquire_timeout })` is on
SQLite, PostgreSQL, MySQL, MSSQL and Oracle (default 10 / 0 / 30 s). DynamoDB:
`DynamoDbAdapter::from_env()` reads `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`,
`AWS_SESSION_TOKEN` (optional), `AWS_REGION` (or `AWS_DEFAULT_REGION`) and optionally
`AWS_ENDPOINT_URL_DYNAMODB` / `AWS_ENDPOINT_URL`.

## Install

```toml
[dependencies]
db-model-router = "0.0.1"            # features: sqlite (default), postgres, mysql, ...
axum = "0.8"
tokio = { version = "1", features = ["full"] }
serde_json = "1"
```

## Quickstart

The complete, compiled version is [`crates/dbmr/examples/quickstart.rs`](crates/dbmr/examples/quickstart.rs).
Run it with `cargo run -p dbmr --example quickstart`. Every other feature is shown, with
asserts, in [`crates/dbmr/examples/tour.rs`](crates/dbmr/examples/tour.rs)
(`cargo run -p dbmr --example tour`).

```rust
use dbmr::adapters::SqliteAdapter;
use dbmr::{create_model_router, CurrentUser, Model, Scope};
use serde_json::json;

// Your auth middleware decides who may see which rows.
async fn auth(mut req: Request, next: Next) -> Result<Response, StatusCode> {
    let tenant = verify_session(&req)?;                           // your code
    req.extensions_mut().insert(CurrentUser(json!({ "tenant_id": tenant })));   // ctx.user in hooks
    req.extensions_mut().insert(Scope::new([("tenant_id", json!(tenant))]));    // row permissions
    Ok(next.run(req).await)
}

let db = SqliteAdapter::connect("sqlite::memory:").await?;      // you create the tables
let notes = Model::builder(db, "notes")
    .column("id", "integer")
    .column("tenant_id", "required|string")
    .column("title", "required|string|maxLength:100")
    .column("body", "string|nullable")
    .safe_delete("is_deleted")                                    // soft delete
    .search_columns(["title", "body"])
    .require_scope(["tenant_id"])                                 // no tenant scope -> 401
    .build()?;                                                    // unknown rule -> error here

let app = axum::Router::new()
    .nest("/notes", create_model_router(notes))
    .layer(axum::middleware::from_fn(auth));
```

## Model API

`Model::builder(adapter, table)`:

| Builder method | Purpose |
|---|---|
| `.column(name, rules)` / `.columns([...])` | Declare a column and its rules. Undeclared keys are stripped on write |
| `.primary_key("id")` | Defaults to `id` |
| `.unique(["email"])` / `.unique_keys(vec![...])` | Upsert conflict targets |
| `.safe_delete("is_deleted")` | Delete sets the flag to 1/true, and reads hide flagged rows |
| `.search_columns([...])` | Columns scanned by `?search=` |
| `.allow_destructive(true)` | Allow delete with empty criteria (off by default) |
| `.require_scope(["tenant_id"])` | Fail closed: a call whose scope lacks the column (or has `null`) is 401 `Missing scope: <col>`, unless `Context::system()` |
| `.hooks(ModelHooks::new()...)` | `before_*` / `after_*` hooks; each receives `Context` (`ctx.user`) |
| `.encryption(EncryptionConfig::new(key_ref, version, None)?)` | Required when any column has `encrypted`. `key_ref` is `env:VAR` (SHA-256 of the variable's value) or `raw:<base64 of exactly 32 bytes>`; anything else is an error |
| `.events(Arc<dyn EventSink>)` | Change events (Kafka: `dbmr::kafka`, feature `kafka`), buffered until commit |
| `.hidden(["password_hash"])` | Never returned by reads, write results or events. Hooks can still write them |
| `.version_column("version")` | Optimistic locking (see below). Declared `integer` if not declared |
| `.created_at("created_at")` / `.modified_at("updated_at")` | Set by the model to UTC text `2026-01-01T00:00:00.000Z`; clients cannot set them |

Operations (all `async`, all take `&Context`): `insert`, `insert_many`, `update`,
`update_many`, `upsert`, `upsert_many`, `patch`, `remove(criteria)`, `remove_where`,
`by_id`, `find(Query)`, `find_one`, `list(Query)` (paged), `count`.

Model views (each returns a new `Model`):
- `with_tx(&tx)`: run inside a transaction;
- `with_hidden()`: also return hidden columns (server-side only; never mount it on a router);
- `has_many("comments", &comments, "post_id")` / `belongs_to("author", &users, "author_id")`:
  relations attached by `Query::new().include(["comments"])` (one query per relation;
  an unknown name is 422). Included rows are filtered by the scope on every scope column
  the related model declares, and a written `belongs_to` foreign key must name a parent
  inside the scope (422 `Invalid reference: <fk>`);
- `typed::<T>()`: a `TypedModel<T>` (`T: Serialize + DeserializeOwned`) with the same
  operations on structs; `find` / `list` return `Page<T> { data, count }`, and insert drops
  `None` fields.

**Scope (row permissions).** `Context.scope` maps columns to a value (`=`) or an array
(`IN`). The router fills it from the `Scope` request extension; code uses
`Context::new().with_scope([("tenant_id", "a")])`. The model then:
- ANDs it into every read (`find`, `find_one`, `list`, `count`, `by_id`) and every delete;
- writes scalar values into every insert (a list scope requires the row to name one value,
  else 422 `The <col> is outside the scope.`);
- refuses update, patch and upsert targets outside it (404, `None` from `patch`);
- filters includes and checks `belongs_to` keys, as above.

Columns the model does not declare are ignored, so one scope fits related tables. A scope
never allows a mass delete. `Context::system()` sees every row and skips `require_scope`;
use it for jobs, or loop over tenants with one scoped `Context` each.

**Optimistic locking** (`.version_column("version")`, DDL `version INTEGER NOT NULL DEFAULT 1`):
insert sets 1. `update`, `patch` and `upsert` of an existing row must carry the current
version; the write runs `... SET version = v + 1 WHERE pk = .. AND version = v` atomically on
every adapter. A stale version is 409 `Record has been modified by another request.`; a
missing one is 428 `The version field is required for this update.` Bulk writes go row by
row and prefix errors with `data.<i>: `.

**Column rules:**
- types: `string`, `integer`, `numeric`, `decimal`, `boolean`, `object`, `array`, `datetime`, `email`;
- constraints: `required`, `nullable`, `maxLength:n`, `minLength:n`, `max:n`, `min:n`, `regex:pattern`, `in:a,b`, `notIn:a,b`;
- flags: `encrypted`;
- schema-only (no runtime check): `auto_increment`, `sometimes`.

**Transactions** (every SQL adapter: SQLite, PostgreSQL/CockroachDB, MySQL/MariaDB, MSSQL, Oracle; MongoDB, Redis and DynamoDB return `Active database adapter does not support transactions.`):

```rust
let tx = dbmr::Transaction::begin(db.as_ref()).await?;
model.with_tx(&tx).insert(row, &ctx).await?;
let sp = tx.savepoint().await?;   // nested
sp.rollback().await?;
tx.commit().await?;               // events are emitted only now
```

**From a schema file:** `dbmr::load_models_from_schema(&adapter, json, encryption)`
returns an `IndexMap<table, Model>`. Per-table keys `softDelete`, `hidden`, `version` and
`timestamps` map to the builder methods above; `relationships` add `has_many` (named after
the child table) on the parent and `belongs_to` (named after the parent table) on the child.

### Same contract on every adapter

Values follow the declared rule on every adapter: writes convert text forms (`"9"` → `9` for `integer`/`numeric`, `"true"`/`"false"`/`"1"`/`"0"` → boolean) after validation, reads convert driver forms (`0`/`1` → `false`/`true`, `9.00` → `9`, numbers in a `string` column → text), and filter values get the same conversion. Every declared column is returned (null when unset) in declared order after the primary key, then undeclared columns. Columns without a type rule pass through as the database returns them. Declared unique keys are checked on insert on every adapter (409). The
`type_contract` conformance scenario runs these cases over HTTP against all 10 databases.

## HTTP contract

`create_model_router(model)` mounts:

| Method | `/` (collection) | `/{id}` (item) |
|---|---|---|
| GET | List (paged) `{data, count}` | Row by id, or 404 `{"message":"Not Found","type":"danger"}` |
| QUERY | List; the filter is in the JSON body | Same as GET |
| POST | One object: single insert, returns the record. `{"data":[...]}`: bulk insert | Single insert; the path id is ignored |
| PUT | Bulk update (upserts on unique key or PK), body `{"data":[...]}` (400 otherwise) | Update row id; 404 if the row is not visible |
| PATCH | – (405) | Partial update; 404 if not visible |
| DELETE | Delete by filter body (400 if empty) | Delete row id; 404 if not visible |

Item routes refuse `{"data":[...]}` bodies with 400; bulk writes use the collection route.

**Router options:** `dbmr::http::create_model_router_with(model, RouterOptions { .. })`
switches operations on and off (`list`, `get`, `create`, `bulk_create`, `update`,
`bulk_update`, `patch`, `delete`, `bulk_delete`, `export`). `RouterOptions::default()` enables
everything; `RouterOptions::read_only()` keeps `list`, `get` and `export`. A disabled operation
answers 405; disabled export answers 400 `Export is disabled`.

**Versioned models:** single-record responses carry `ETag: "<version>"`. `PUT`/`PATCH`/`DELETE
/{id}` take the version from the body or from `If-Match`.

**Hidden columns** are stripped from write bodies; a filter, sort or `select_columns` naming
one is 422 `Invalid filter Inputs`.

**OpenAPI and health:**

```rust
use dbmr::http::{health_router, openapi, openapi_route, RouterOptions};
let opts = RouterOptions::default();
let spec = openapi("My API", "1.0.0", &[("/users", &users, &opts)]);   // OpenAPI 3.1 JSON
let app = axum::Router::new()
    .nest("/users", dbmr::http::create_model_router_with(users, opts))
    .nest("/openapi.json", openapi_route(spec))
    .nest("/health", health_router(db.clone()));  // 200 {"status":"ok"} or 503 {"status":"unavailable"}
```

**Query parameters** (also accepted in a QUERY body):

| Parameter | Meaning |
|---|---|
| `page` | Page number, starting at 0 |
| `size` / `limit` | Page size. Default 30, max 200 |
| `sort=-age,name` | Sort keys; `-` means descending |
| `search=term` | `LIKE` match across `search_columns` |
| `select_columns=a,b` | Return only these columns |
| `output_content_type=csv` or `xml` | Response format |
| `col=value` | Filter. Value prefixes: `>=5`, `<=`, `>`, `<`, `!x` (not equal), `%x%` (like), `!%x%`, `in(a,b)`, `!in(a,b)`. JSON `null` (QUERY body) means `IS NULL` |
| `filter=[[["col","op",value],...],...]` | Raw filter: OR of AND-groups |
| `include=comments,author` | Attach relations declared with `has_many` / `belongs_to` |

**Permissions:** the router has no auth of its own. Your middleware verifies the caller and
inserts `CurrentUser` (hooks read it as `ctx.user`) and `Scope` (row permissions, enforced by
the model as described above). Without a `Scope`, every row is reachable unless the model
has `require_scope`. Patterns:
- tenant isolation: `Scope::new([("tenant_id", json!(t))])`;
- per-user rows: `Scope::new([("owner_id", json!(user_id))])`;
- a user in several tenants: `Scope::new([("tenant_id", json!(["a", "b"]))])`;
- nested routes (`/projects/{project_id}/issues`): a `route_layer` middleware adds
  `project_id` from the path to the scope;
- per-role operations (viewers only read): ordinary middleware that answers 403.

Each pattern is worked through in [`skills/dbmr/references/examples.md`](skills/dbmr/references/examples.md).

**Errors** (body always `{"message": "...", "type": "danger"}`):
- 400 malformed input, 401 missing scope (`require_scope`), 404 not visible, 405 method or
  disabled route, 409 stale version, 422 validation (niv-compatible messages), 428 version
  missing, 500 database.
- 5xx bodies are scrubbed to `Internal server error` unless `NODE_ENV` is `development` or `test`.

## Quality

```bash
cargo test --workspace --all-features            # SQLite + in-memory suites
cargo run -p xtask -- ci                         # fmt, clippy, tests, traceability, lints
docker compose up -d && cargo test --all-features -- --ignored   # live adapters
```

The qualification plan and its evidence are in [`OQ/`](OQ/README.md). Architecture decisions
are in [`docs/adr/`](docs/adr/README.md).

## License

See [`LICENSE`](LICENSE).
