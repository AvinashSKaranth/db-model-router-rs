# Recipes

## Users with a password hash (hidden column + hash hook)

```rust
let users = Model::builder(db, "users")
    .column("id", "integer")
    .column("email", "required|email")
    .column("password_hash", "string")
    .hidden(["password_hash"])                    // never returned, stripped from HTTP writes
    .hooks(ModelHooks::new().before_save(|mut row, ctx| async move {
        // HTTP strips client-sent `password_hash`, but `password` is undeclared and still
        // reaches the hook (undeclared keys are dropped after it).
        if let Some(DbValue::Text(p)) = row.shift_remove("password") {
            row.insert("password_hash".into(), my_hash(&p).into());
        }
        Ok(row)
    }))
    .build()?;

// Login check (server-side only; never mount this clone in a router):
let u = users.with_hidden()
    .find_one(Query::new().eq("email", email), &Context::new()).await?;
```

Declare `password_hash` (otherwise it is dropped) and do not declare `password`.

## Optimistic locking (concurrent edits)

```rust
// DDL: version INTEGER NOT NULL DEFAULT 1
let docs = Model::builder(db, "docs")
    .column("id", "integer").column("title", "required|string")
    .version_column("version")
    .build()?;
```

Client flow:

1. `GET /docs/7` → body has `"version": 3`, header `ETag: "3"`.
2. `PUT /docs/7` (or `PATCH`) with `If-Match: "3"` (or `"version": 3` in the body).
3. `200` → new record and `ETag: "4"`. Keep that for the next edit.
4. `409 Record has been modified by another request.` → someone else saved first. Re-`GET`,
   show/merge their changes, retry with the new ETag. Do not blindly resend with a bumped
   version.
5. `428` → the client forgot the version; fix the client.

In Rust code (jobs), carry the version in the row:

```rust
match docs.patch(row([("id", 7.into()), ("version", 3.into()), ("title", "x".into())]), &ctx).await {
    Err(AppError::Conflict(_)) => { /* reload with by_id and retry */ }
    other => { other?; }
}
```

## Read-only or restricted resource

```rust
use dbmr::http::{create_model_router_with, RouterOptions};
let r = create_model_router_with(model, RouterOptions::read_only());   // writes → 405
let r = create_model_router_with(model,
    RouterOptions { delete: false, bulk_delete: false, ..RouterOptions::default() });
```

Per-role route rules (e.g. viewers only read) belong in your middleware: `examples.md` §17.
Rules that must also hold for jobs belong in `before_delete` / `before_save` using `ctx.user`.

## Typed structs

```rust
#[derive(serde::Serialize, serde::Deserialize)]
struct Note { id: Option<i64>, title: String, body: Option<String> }

let notes = model.typed::<Note>();
let saved = notes.insert(&Note { id: None, title: "hi".into(), body: None }, &ctx).await?;
let page = notes.list(Query::new().size(10), &ctx).await?;   // Page<Note> { data, count }
```

Insert drops `None` fields (column defaults apply). Hidden columns never come back, so leave
them out of `T` or make them `Option` with `#[serde(default)]`.

## Includes (post with comments and author)

```rust
let posts = posts.has_many("comments", &comments, "post_id")
                 .belongs_to("author", &users, "author_id");
// HTTP: GET /posts?include=comments,author   Rust: Query::new().include(["comments", "author"])
```

Build relations before mounting the router. The related models' hidden columns, soft delete
and `before_find` hooks apply. Tenancy is automatic: the request's `Scope`
(`ctx.scope`, e.g. `{"tenant_id": "A"}`) filters included rows on every scope column the
related model declares, and writing a `belongs_to` foreign key that points outside the scope
(another tenant's user or post) is 422 `Invalid reference: <fk>`. Declare `belongs_to` on the
child (`comments.belongs_to("post", &posts, "post_id")`) so comment writes are checked. A
shared row with `tenant_id` NULL is outside every scope; give global lookup tables no
`tenant_id` column.

## Background jobs

```rust
let ctx = Context::new().with_scope([("tenant_id", tenant)]);   // one tenant, same rules as HTTP
let ctx = Context::system();                                    // every row, skips require_scope
let ctx = Context::with_user(json!({"id": "system"})).with_scope([("tenant_id", tenant)]);
let stale = model.find(Query::new().and("updated_at", Operator::Lt, cutoff), &ctx).await?;
```

Cross-tenant job: list the tenants under `Context::system()`, then loop with one scoped
context per tenant (`examples.md` §14). Use `system()` only where all rows are intended.

## Audit columns

`.created_at("created_at").modified_at("updated_at")` for times. `before_create` sets
`created_by`, `before_save` sets `updated_by` from `ctx.user`; declare both columns.

## Kafka change events

Enable `kafka` on `dbmr`; the crate is re-exported as `dbmr::kafka`.

```rust
let producer = Arc::new(dbmr::kafka::KafkaProducer::new());
producer.init(None).await;   // reads KAFKA_BROKER, KAFKA_CLIENT_ID, KAFKA_TOPIC_PREFIX
let orders = Model::builder(db, "orders").column("id", "integer").events(producer.clone()).build()?;
```

`init` returns `false` (and events are dropped) when no broker is configured or reachable.
Use `127.0.0.1` not `localhost` for a local broker (IPv6 resolution delays drop events).

## Global adapter

`dbmr::init_global(adapter)?` once at startup, `dbmr::global_db()` anywhere. Prefer passing
the `Arc` explicitly; use the global only in code you cannot thread state through.

## Many tables from one schema, with docs and health

```rust
let adapter: Arc<dyn DatabaseAdapter> = db;
let models = dbmr::load_models_from_schema(&adapter, SCHEMA_JSON, None)?;
let opts = RouterOptions::default();
let docs: Vec<(String, Model)> = models.iter().map(|(t, m)| (format!("/{t}"), m.clone())).collect();
let refs: Vec<(&str, &Model, &RouterOptions)> = docs.iter().map(|(p, m)| (p.as_str(), m, &opts)).collect();
let mut app = Router::new()
    .nest("/openapi.json", openapi_route(openapi("API", "1.0.0", &refs)))
    .nest("/health", health_router(adapter.clone()));
for (table, model) in models {
    app = app.nest(&format!("/{table}"), create_model_router(model));
}
let app = app.layer(middleware::from_fn(auth));   // auth inserts CurrentUser + Scope
```
