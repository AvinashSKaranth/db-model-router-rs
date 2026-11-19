# Examples

Every snippet here is taken from `crates/dbmr/examples/tour.rs`, which is compiled and run
with `cargo run -p dbmr --example tour`. The exceptions are marked *(snippet)*. When a
snippet and the tour disagree, the tour is right.

Shared imports:

```rust
use std::{collections::HashMap, sync::{Arc, Mutex}};
use axum::{body::Body, extract::{Path, Request}, http::StatusCode,
           middleware::{self, Next}, response::{IntoResponse, Response}, Router};
use dbmr::adapters::SqliteAdapter;
use dbmr::core::events::EventSink;
use dbmr::core::filter::{FilterExpr, Operator};
use dbmr::core::types::{row, row_to_json};
use dbmr::http::{create_model_router_with, health_router, openapi, openapi_route, RouterOptions};
use dbmr::{create_model_router, AppError, Context, CurrentUser, DatabaseAdapter, DbValue,
           EncryptionConfig, Model, ModelHooks, Query, Result, Scope, Transaction};
use serde_json::{json, Value};
```

Contents:

1. [Adapter, DDL, model](#1-adapter-ddl-model)
2. [Create, read, update, patch, upsert](#2-create-read-update-patch-upsert)
3. [Queries](#3-queries)
4. [Errors](#4-errors)
5. [Delete, the mass-delete guard, soft delete](#5-delete-the-mass-delete-guard-soft-delete)
6. [Hooks: normalize, audit, role checks](#6-hooks-normalize-audit-role-checks)
7. [Hidden password column and login](#7-hidden-password-column-and-login)
8. [Optimistic locking](#8-optimistic-locking)
9. [Relations and include](#9-relations-and-include)
10. [Typed models](#10-typed-models)
11. [Transactions and savepoints](#11-transactions-and-savepoints)
12. [Encrypted columns](#12-encrypted-columns)
13. [Change events](#13-change-events)
14. [Scope in code: tenants, list scopes, jobs](#14-scope-in-code-tenants-list-scopes-jobs)
15. [HTTP: auth middleware and scope](#15-http-auth-middleware-and-scope)
16. [HTTP: scope patterns](#16-http-scope-patterns)
17. [HTTP: route permissions by role](#17-http-route-permissions-by-role)
18. [HTTP: nested routes](#18-http-nested-routes)
19. [HTTP: read-only and partial routers](#19-http-read-only-and-partial-routers)
20. [HTTP: requests](#20-http-requests)
21. [OpenAPI and health](#21-openapi-and-health)
22. [Schema file](#22-schema-file)
23. [Testing a router](#23-testing-a-router)

## 1. Adapter, DDL, model

dbmr never creates tables. Run your DDL, then declare every column with a type rule.

```rust
let db = SqliteAdapter::connect("sqlite::memory:").await?;   // Arc<SqliteAdapter>; no Arc::new
db.query("CREATE TABLE users (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    email TEXT NOT NULL UNIQUE,   -- upsert on a unique key needs the DB constraint
    age INTEGER,
    active INTEGER NOT NULL DEFAULT 1)", &[]).await?;

let users = Model::builder(db, "users")
    .column("id", "integer")
    .column("name", "required|string|maxLength:50")
    .column("email", "required|email")
    .column("age", "integer|nullable")
    .column("active", "boolean")
    .unique(["email"])                  // 409 on duplicate insert, upsert conflict target
    .search_columns(["name", "email"])  // used by Query::search / ?search=
    .build()?;                          // unknown rule -> error here
let ctx = Context::new();
```

## 2. Create, read, update, patch, upsert

```rust
// insert returns the stored record. Values follow the rule: "36" -> 36, 1 -> true.
let ann = users.insert(row([("name", "Ann".into()), ("email", "ann@x.io".into()),
                            ("age", "36".into())]), &ctx).await?.unwrap();
assert_eq!(ann["age"], DbValue::Int(36));
assert_eq!(ann["active"], DbValue::Bool(true));

// Bulk writes return a summary { rows, message }, not records.
let s = users.insert_many(vec![
    row([("name", "Bob".into()), ("email", "bob@x.io".into()), ("age", 17.into())]),
    row([("name", "Cid".into()), ("email", "cid@x.io".into()), ("age", 52.into())]),
], &ctx).await?;
assert_eq!(s.rows, 2);

let bob = users.by_id(2, &ctx).await?;                       // Option<RowMap>

// update = full row (all required columns); patch = only the given columns + pk.
users.update(row([("id", 2.into()), ("name", "Bobby".into()),
                  ("email", "bob@x.io".into()), ("age", 18.into())]), &ctx).await?;
users.patch(row([("id", 3.into()), ("age", 53.into())]), &ctx).await?;  // None if no such row

// upsert keys on the first declared unique key the row carries, else on the pk.
users.upsert(row([("name", "Eve".into()), ("email", "ann@x.io".into())]), &ctx).await?;
// update_many / upsert_many take Vec<RowMap> and return WriteSummary.
```

## 3. Queries

`list` returns one page; `find` returns every match (cap it with `.limit(n)`). `count` on
both is the total number of matches.

```rust
// AND conditions, sort ("-" = descending), paging (page from 0, size default 30, max 200).
let page = users.list(
    Query::new().and("age", Operator::Gte, 18).eq("active", true)
        .sort("-age").page(0).size(2),
    &ctx).await?;
// page.data: Vec<RowMap> (2 rows), page.count: every match

// OR of AND-groups.
let teen_or_senior = FilterExpr::default().and("age", Operator::Lt, 20)
    .or(FilterExpr::default().and("age", Operator::Gt, 50));
let r = users.find(Query::filter(teen_or_senior).select(["id", "name"]), &ctx).await?;

// IN, LIKE, IS NULL.
users.count(&Query::new().and("id", Operator::In, DbValue::Json(json!([1, 3]))), &ctx).await?;
users.count(&Query::new().and("email", Operator::Like, "%@x.io"), &ctx).await?;
users.count(&Query::new().and("age", Operator::IsNull, DbValue::Null), &ctx).await?;

// Search over search_columns, first match, limit.
let hit = users.find_one(Query::new().search("bobby"), &ctx).await?;
let first_two = users.find(Query::new().sort("id").limit(2), &ctx).await?;

// The JSON form the HTTP layer uses also works in code.
let (q, _) = Query::from_json(&json!({"filter": [[["age", ">", 40]]]}), "id")?;
let n = users.count(&q, &ctx).await?;

// Include relations, skip the count query.
let q = Query::new().include(["books"]).with_count(false);
```

Operators: `Eq NotEq Gt Lt Gte Lte Like NotLike In NotIn IsNull IsNotNull`.

## 4. Errors

Every call returns `dbmr::Result<T>`. Match the `AppError` variant, or use
`status_code()` and `message()` (the HTTP status and body text).

```rust
let err = users.insert(row([("email", "not-an-email".into())]), &ctx).await.unwrap_err();
assert!(matches!(err, AppError::UnprocessableEntity(_)));     // rule failure
assert_eq!(err.status_code(), 422);

let dup = users.insert(row([("name", "X".into()), ("email", "bob@x.io".into())]), &ctx)
    .await.unwrap_err();
assert_eq!(dup.status_code(), 409);                            // Duplicate value for unique key

match users.patch(row([("id", 1.into()), ("age", 1.into())]), &ctx).await {
    Ok(Some(rec)) => { /* updated */ }
    Ok(None) => { /* no such row, or outside the scope -> 404 over HTTP */ }
    Err(AppError::Conflict(_)) => { /* stale version: re-read and retry */ }
    Err(e) => return Err(e),
}
```

Variants and statuses: `BadRequest` 400, `Unauthorized` 401, `NotFound` 404, `Conflict` 409,
`PreconditionRequired` 428, `UnprocessableEntity`/`Validation` 422,
`Internal`/`Database`/`Encryption` 500. Return any of them from a hook to abort the call.

## 5. Delete, the mass-delete guard, soft delete

```rust
users.remove(json!({"id": 4}), &ctx).await?;                         // by criteria object
users.remove(json!(4), &ctx).await?;                                 // by pk
users.remove(json!([1, 2]), &ctx).await?;                            // pk IN
users.remove_where(&FilterExpr::eq("name", "Cid"), &ctx).await?;     // by FilterExpr

// Empty criteria = mass delete: 400 unless the model has .allow_destructive(true).
assert_eq!(users.remove(json!({}), &ctx).await.unwrap_err().status_code(), 400);
```

Soft delete: `remove` sets a flag and reads hide flagged rows. The column must exist with a
0/false default.

```rust
// DDL: is_deleted INTEGER NOT NULL DEFAULT 0
let posts = Model::builder(db.clone(), "posts")
    .column("id", "integer")
    .column("title", "required|string")
    .safe_delete("is_deleted")
    .build()?;
posts.remove(json!({"id": 1}), &ctx).await?;
assert!(posts.by_id(1, &ctx).await?.is_none());            // hidden from reads
let raw = db.query("SELECT is_deleted FROM posts WHERE id = 1", &[]).await?;
assert_eq!(raw[0]["is_deleted"], DbValue::Int(1));          // still in the table
```

## 6. Hooks: normalize, audit, role checks

Hooks get `(value, Context)` and return `Result<value>`. `ctx.user` is the `CurrentUser`
JSON from the request (or what a job passed to `Context::with_user`).

```rust
// DDL adds: created_by INTEGER, updated_by INTEGER, created_at TEXT, modified_at TEXT
let accounts = Model::builder(db, "accounts")
    .column("id", "integer")
    .column("email", "required|email")
    .column("created_by", "integer|nullable")
    .column("updated_by", "integer|nullable")
    .column("created_at", "string|nullable")
    .column("modified_at", "string|nullable")
    .created_at("created_at")        // set on insert (UTC RFC 3339); clients cannot set it
    .modified_at("modified_at")      // set on every write
    .hooks(ModelHooks::new()
        .before_create(|mut r, ctx| async move {
            r.insert("created_by".into(), ctx.user["id"].clone().into());
            Ok(r)
        })
        .before_save(|mut r, ctx| async move {               // insert, update, upsert, patch
            r.insert("updated_by".into(), ctx.user["id"].clone().into());
            if let Some(DbValue::Text(e)) = r.get("email") {
                let lower = e.to_lowercase();
                r.insert("email".into(), lower.into());
            }
            Ok(r)
        })
        .after_find(|rows, _ctx| async move { Ok(rows) }))   // redact or enrich reads
    .build()?;

let ctx = Context::with_user(json!({"id": 7}));
let acc = accounts.insert(row([("email", "Ann@X.io".into())]), &ctx).await?.unwrap();
assert_eq!(acc["email"], "ann@x.io".into());
assert_eq!(acc["created_by"], DbValue::Int(7));
```

Role check inside the model *(snippet)*. Prefer route middleware (§17) for per-route rules;
use a hook when the rule must hold for jobs too.

```rust
ModelHooks::new().before_delete(|criteria, ctx| async move {
    if ctx.user["role"] != "admin" {
        return Err(AppError::Unauthorized("Only admins delete".into()));
    }
    Ok(criteria)
})
```

Other hooks: `before_update`, `before_upsert`, `before_find` (gets the `FilterExpr`),
`after_save`, `after_create`, `after_update`, `after_upsert`, `after_delete`,
`after_bulk(|op: BulkOp, summary, ctx| ...)`.

## 7. Hidden password column and login

Hidden columns are never returned. Over HTTP they are stripped from write bodies, and
filtering, sorting or selecting them is 422. Server code can write and filter them.

```rust
let accounts = Model::builder(db, "accounts")
    .column("id", "integer")
    .column("email", "required|email")
    .column("password_hash", "string|nullable")
    .hidden(["password_hash"])
    .build()?;

// Server code sets the hash (hash() stands in for argon2/bcrypt).
let acc = accounts.insert(row([("email", "ann@x.io".into()),
                               ("password_hash", hash("s3cret").into())]), &ctx).await?.unwrap();
assert!(!acc.contains_key("password_hash"));

// Login: with_hidden() returns hidden columns. Never mount it in a router.
let login = accounts.with_hidden()
    .find_one(Query::new().eq("email", "ann@x.io"), &Context::new()).await?.unwrap();
assert_eq!(login["password_hash"], hash("s3cret").into());
```

Sign-up over HTTP *(snippet)*: the client sends `password`, which is not a declared column.
Undeclared keys still reach `before_save` and are dropped after it, so the hook can hash it.

```rust
.hooks(ModelHooks::new().before_save(|mut r, _ctx| async move {
    if let Some(DbValue::Text(p)) = r.shift_remove("password") {
        r.insert("password_hash".into(), hash(&p).into());
    }
    Ok(r)
}))
```

## 8. Optimistic locking

```rust
// DDL: version INTEGER NOT NULL DEFAULT 1
let docs = Model::builder(db, "docs")
    .column("id", "integer")
    .column("body", "required|string")
    .column("version", "integer")
    .version_column("version")
    .build()?;
let d = docs.insert(row([("body", "v1".into())]), &ctx).await?.unwrap();   // version 1

// No version -> 428. Current version -> ok, bumps it. Old version -> 409.
let e = docs.patch(row([("id", 1.into()), ("body", "v2".into())]), &ctx).await.unwrap_err();
assert_eq!(e.status_code(), 428);
docs.patch(row([("id", 1.into()), ("body", "v2".into()), ("version", 1.into())]), &ctx).await?;
let e = docs.patch(row([("id", 1.into()), ("body", "x".into()), ("version", 1.into())]), &ctx)
    .await.unwrap_err();
assert!(matches!(e, AppError::Conflict(_)));
```

Over HTTP: `GET /{id}` returns `ETag: "1"`; send it back as `If-Match` (see §20).

## 9. Relations and include

```rust
let books = books.belongs_to("author", &authors, "author_id");   // object or null
let authors = authors.has_many("books", &books, "author_id");    // array

let r = authors.find(Query::new().include(["books"]), &ctx).await?;
assert_eq!(row_to_json(&r.data[0])["books"][0]["title"], "Earthsea");
let r = books.find(Query::new().include(["author"]), &ctx).await?;
assert_eq!(row_to_json(&r.data[0])["author"]["name"], "Le Guin");
```

One extra query per relation, one level deep. Relations return new models, so build them
before mounting routers. Under a scope (§14), included rows stay inside it and a
`belongs_to` key pointing outside it is 422 `Invalid reference: <fk>`. Without a scope,
referential integrity is the database's job (`FOREIGN KEY` in your DDL).

## 10. Typed models

```rust
#[derive(Serialize, Deserialize)]
struct Book {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<i64>,
    author_id: i64,
    title: String,
}

let typed = books.typed::<Book>();
let b = typed.insert(&Book { id: None, author_id: 1, title: "Earthsea".into() }, &ctx)
    .await?.unwrap();
assert_eq!(b.id, Some(1));
let page = typed.find(Query::new(), &ctx).await?;      // Page<Book> { data, count }
typed.patch(&json!({"id": 1, "title": "A Wizard of Earthsea"}), &ctx).await?;
```

## 11. Transactions and savepoints

All SQL adapters. MongoDB, Redis and DynamoDB return
`Active database adapter does not support transactions.`

```rust
let tx = Transaction::begin(db.as_ref()).await?;
let w = wallets.with_tx(&tx);                     // bind every model you use
w.patch(row([("id", 1.into()), ("balance", 60.into())]), &ctx).await?;
w.patch(row([("id", 2.into()), ("balance", 40.into())]), &ctx).await?;

let sp = tx.savepoint().await?;                   // nested unit
wallets.with_tx(&sp).patch(row([("id", 2.into()), ("balance", 9999.into())]), &ctx).await?;
sp.rollback().await?;                             // undo only the savepoint

tx.commit().await?;                               // or tx.rollback().await?
```

Change events raised inside a transaction are sent only after the outermost commit.

## 12. Encrypted columns

```rust
// Production: "env:DBMR_KEY" (the key is the SHA-256 of that variable's value; use a long
// random secret). "raw:<base64 of exactly 32 bytes>" is for tests. 1 = key version, stored
// with the ciphertext for rotation.
let patients = Model::builder(db.clone(), "patients")
    .column("id", "integer")
    .column("name", "required|string")
    .column("ssn", "encrypted|string")             // build() fails without .encryption
    .encryption(EncryptionConfig::new("raw:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=", 1, None)?)
    .build()?;
let p = patients.insert(row([("name", "Ann".into()), ("ssn", "123-45-6789".into())]), &ctx)
    .await?.unwrap();
assert_eq!(p["ssn"], "123-45-6789".into());                      // decrypted on read
let raw = db.query("SELECT ssn FROM patients", &[]).await?;
assert_ne!(raw[0]["ssn"], "123-45-6789".into());                  // ciphertext at rest
```

Key rotation *(snippet)*: new writes use the active key; old versions stay readable.

```rust
let history = HashMap::from([("1".to_string(), "env:DBMR_KEY_V1".to_string())]);
EncryptionConfig::new("env:DBMR_KEY_V2", 2, Some(&history))?
```

## 13. Change events

One event per successful write (`insert`, `update`, `upsert`, `delete`), fire-and-forget.

```rust
#[derive(Default)]
struct Recorder(Mutex<Vec<(String, String)>>);

#[async_trait::async_trait]
impl EventSink for Recorder {
    async fn emit(&self, table: &str, operation: &str, _data: Value) {
        self.0.lock().unwrap().push((table.into(), operation.into()));
    }
}

let events = Arc::new(Recorder::default());
let patients = Model::builder(db, "patients") /* columns */ .events(events.clone()).build()?;
```

Kafka *(snippet; feature `kafka`)*:

```rust
let producer = Arc::new(dbmr::kafka::KafkaProducer::new());
producer.init(None).await;   // KAFKA_BROKER, KAFKA_CLIENT_ID, KAFKA_TOPIC_PREFIX
let orders = Model::builder(db, "orders").column("id", "integer")
    .events(producer.clone()).build()?;
```

## 14. Scope in code: tenants, list scopes, jobs

A scope is a map of column to value carried by the `Context`. The model ANDs it into every
read, update and delete, and writes it into every insert. Rows outside it look missing.

```rust
let tasks = Model::builder(db, "tasks")
    .column("id", "integer")
    .column("tenant_id", "required|string")
    .column("title", "required|string")
    .require_scope(["tenant_id"])        // fail closed: no tenant_id in scope -> 401
    .build()?;
let a = Context::new().with_scope([("tenant_id", "a")]);
let b = Context::new().with_scope([("tenant_id", "b")]);

// Inserts take the scope value, whatever the caller sent.
let t = tasks.insert(row([("tenant_id", "b".into()), ("title", "a2".into())]), &a)
    .await?.unwrap();
assert_eq!(t["tenant_id"], "a".into());

// Reads, updates and deletes only see the scope's rows.
assert!(tasks.by_id(2, &a).await?.is_none());                         // b's row
assert!(tasks.patch(row([("id", 2.into()), ("title", "x".into())]), &a).await?.is_none());
assert_eq!(tasks.remove(json!({"id": 2}), &a).await?.rows, 0);

// A list means IN: a user in several tenants. Inserts must name one of the values,
// else 422 "The tenant_id is outside the scope."
let both = Context::new().with_scope([("tenant_id", json!(["a", "b"]))]);
tasks.count(&Query::new(), &both).await?;

// Missing scope with require_scope -> 401 "Missing scope: tenant_id".
let e = tasks.count(&Query::new(), &Context::new()).await.unwrap_err();
assert_eq!(e.message(), "Missing scope: tenant_id");

// Context::system() skips require_scope and sees every row. Use it on purpose, in jobs.
let all = tasks.find(Query::new().select(["tenant_id"]), &Context::system()).await?;
```

Per-tenant job: list the tenants as system, then do each tenant's work in its own scope.

```rust
let mut tenants: Vec<String> = all.data.iter()
    .filter_map(|r| r["tenant_id"].as_str().map(str::to_string)).collect();
tenants.sort();
tenants.dedup();
for tenant in tenants {
    let ctx = Context::new().with_scope([("tenant_id", tenant.as_str())]);
    let n = tasks.count(&Query::new(), &ctx).await?;
    tasks.insert(row([("title", format!("digest of {n} tasks").into())]), &ctx).await?;
}
```

Scope rules:

- A scope column the model does not declare is ignored for that model, so one scope works
  for related tables with and without `tenant_id`.
- `null` in a scope is 401 (it would otherwise match nothing or everything).
- An empty list matches no rows.
- A scope never permits a mass delete: `remove(json!({}))` is still 400.
- `ctx.user` and `ctx.scope` are separate: hooks read the user; only the scope limits rows.

## 15. HTTP: auth middleware and scope

The router knows nothing about auth. Your middleware verifies the session or token and puts
two request extensions in: `CurrentUser` (becomes `ctx.user`) and `Scope` (becomes
`ctx.scope`). Without a `Scope`, every row is reachable, unless the model has
`require_scope`, in which case it is 401.

```rust
async fn auth(mut req: Request, next: Next) -> std::result::Result<Response, StatusCode> {
    // Real apps verify a session cookie or JWT here.
    let h = req.headers();
    let header = |k| Some(h.get(k)?.to_str().ok()?.to_string());
    let (user, tenant, role) = (header("x-user"), header("x-tenant"), header("x-role"));
    let (Some(user), Some(tenant)) = (user, tenant) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let role = role.unwrap_or_else(|| "viewer".into());
    req.extensions_mut()
        .insert(CurrentUser(json!({"id": user, "tenant_id": tenant, "role": role})));
    req.extensions_mut().insert(Scope::new([("tenant_id", json!(tenant))]));
    Ok(next.run(req).await)
}

let app = Router::new()
    .nest("/projects", create_model_router(projects))
    .nest("/issues", create_model_router(issues))
    .layer(middleware::from_fn(auth));
```

Read headers through `req.headers()` as above. A closure that captures `&req` across the
`await` makes the middleware future `!Send`, and `from_fn` then fails with a long
`Service<Request>` trait error.

## 16. HTTP: scope patterns

*(snippets; same mechanism as §15)*

```rust
// Each user sees only their own rows (owner_id is set on insert from the scope).
req.extensions_mut().insert(Scope::new([("owner_id", json!(user_id))]));

// Tenant and owner together: AND of both.
req.extensions_mut().insert(Scope::new([("tenant_id", json!(t)), ("owner_id", json!(u))]));

// A user in several tenants: list = IN. Inserts must send one of them.
req.extensions_mut().insert(Scope::new([("tenant_id", json!(["acme", "globex"]))]));

// Admins: insert no Scope. Works only on models without require_scope; for those, give
// the admin a list scope of every tenant instead.
if role != "admin" {
    req.extensions_mut().insert(Scope::new([("tenant_id", json!(tenant))]));
}
```

## 17. HTTP: route permissions by role

Row access is the scope. Which operations a role may call is ordinary middleware.

```rust
async fn writers_only(req: Request, next: Next) -> Response {
    let role = req.extensions().get::<CurrentUser>()
        .and_then(|u| u.0["role"].as_str().map(str::to_string));
    let read = matches!(req.method().as_str(), "GET" | "HEAD" | "QUERY");
    if read || role.as_deref() == Some("editor") {
        next.run(req).await
    } else {
        (StatusCode::FORBIDDEN, axum::Json(json!({"message": "Forbidden", "type": "danger"})))
            .into_response()
    }
}

let api = Router::new()
    .nest("/projects", create_model_router(projects))
    .layer(middleware::from_fn(writers_only))
    .layer(middleware::from_fn(auth));      // the last layer runs first
```

## 18. HTTP: nested routes

Add the parent id from the path to the scope with a `route_layer` on the child router. The
child sees and writes only rows of that parent, and the tenant scope still applies.

```rust
async fn project_scope(Path(p): Path<HashMap<String, String>>, mut req: Request, next: Next)
    -> Response
{
    let mut scope = req.extensions().get::<Scope>().cloned().unwrap_or_default();
    scope.0.insert("project_id".into(), json!(p["project_id"]));
    req.extensions_mut().insert(scope);
    next.run(req).await
}

Router::new().nest(
    "/projects/{project_id}/issues",
    create_model_router(issues).route_layer(middleware::from_fn(project_scope)),
)
```

`POST /projects/1/issues {"title": "Fuel leak"}` stores `project_id = 1` and the caller's
`tenant_id`. With `issues.belongs_to("project", &projects, "project_id")`, another tenant
posting to `/projects/1/issues` gets 422 `Invalid reference: project_id`.

## 19. HTTP: read-only and partial routers

```rust
// list, get, export. Writes -> 405.
create_model_router_with(issues, RouterOptions::read_only())

// Everything except deletes and CSV/XML export.
create_model_router_with(projects, RouterOptions {
    delete: false, bulk_delete: false, export: false, ..RouterOptions::default()
})
```

Fields: `list`, `get`, `create` (single `POST`), `bulk_create` (`POST /` with `data`),
`update`, `bulk_update`, `patch`, `delete`, `bulk_delete`, `export`.

## 20. HTTP: requests

Taken from the tour's HTTP section; `>=`, `%` and `,` must be URL-encoded by real clients.

| Request | Result |
|---|---|
| `POST /projects {"name":"Apollo"}` | 200, the stored record (`tenant_id` from the scope) |
| `POST /projects {"data":[{"name":"Gemini"},{"name":"Mercury"}]}` | 200 `{"rows":2,...}` |
| `POST /projects/1 {"data":[...]}` | 400: bulk writes use the collection route |
| `GET /projects` | `{"data":[...],"count":3}` |
| `GET /projects/1` as another tenant | 404 |
| `GET /projects?name=%25e%25&sort=-name&page=0&size=1` | LIKE, sort, paging |
| `GET /projects?id=%3E%3D2&select_columns=id,name` | `id >= 2`, projection |
| `GET /projects?id=in(1,3)` | IN |
| `GET /projects?search=gem` | LIKE over `search_columns` |
| `QUERY /projects {"filter":[[["name","=","Apollo"]],[["name","=","Gemini"]]]}` | OR |
| `GET /projects/1?include=issues` | record with an `issues` array |
| `GET /projects/1` (versioned) | header `ETag: "1"` |
| `PATCH /projects/1 {"name":"x"}` without version | 428 |
| `PATCH /projects/1` with `If-Match: "1"` | 200, version becomes 2 |
| same request again | 409 |
| `GET /projects?output_content_type=csv&select_columns=id,name` | `text/csv` |
| `DELETE /projects {}` | 400 (mass delete) |
| `DELETE /projects {"name":"Mercury"}` | `{"rows":1,...}` |
| `POST /reports/issues` on a read-only router | 405 |

## 21. OpenAPI and health

```rust
// Build the spec before the routers take the models.
let spec = openapi("Tracker", "1.0.0", &[
    ("/projects", &projects, &RouterOptions::default()),
    ("/reports/issues", &issues, &RouterOptions::read_only()),
]);
let app = Router::new()
    .merge(api)
    .nest("/health", health_router(db))            // 200 {"status":"ok"} | 503
    .nest("/openapi.json", openapi_route(spec));   // GET -> OpenAPI 3.1 JSON
```

## 22. Schema file

```rust
const SCHEMA: &str = r#"{
  "adapter": "sqlite3",
  "framework": "axum",
  "tables": {
    "products": {
      "columns": { "id": "auto_increment", "title": "required|string", "price": "required|numeric" },
      "pk": "id",
      "unique": [],
      "search_columns": ["title"]
    }
  }
}"#;

let mut models = dbmr::load_models_from_schema(&db, SCHEMA, None)?;   // IndexMap<table, Model>
let products = models.shift_remove("products").unwrap();
let app = Router::new().nest("/products", create_model_router(products));
```

A request `Scope` applies to schema models too. `require_scope` and hooks are not part of
the schema format; build the model with `Model::builder` when you need them.

## 23. Testing a router

```rust
use tower::ServiceExt; // oneshot

let res = app.clone().oneshot(
    Request::builder().method("POST").uri("/projects")
        .header("content-type", "application/json")
        .header("x-user", "u1").header("x-tenant", "acme").header("x-role", "editor")
        .body(Body::from(r#"{"name":"Apollo"}"#)).unwrap(),
).await.unwrap();
assert_eq!(res.status(), StatusCode::OK);
let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
let rec: Value = serde_json::from_slice(&bytes).unwrap();
```

Use `SqliteAdapter::connect("sqlite::memory:")` in tests; it keeps one connection, so the
in-memory database survives between calls.
