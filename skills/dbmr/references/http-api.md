# HTTP API (`create_model_router`)

```rust
use dbmr::{create_model_router, CurrentUser, Scope};
use dbmr::http::{create_model_router_with, health_router, openapi, openapi_route, RouterOptions};
```

`create_model_router(model) -> axum::Router` serves every operation.
`create_model_router_with(model, RouterOptions)` serves a subset. Mount with
`Router::new().nest("/things", ...)`. Body limit 16 MiB. Row access follows the request's
`Scope` extension (see Scope and CurrentUser).

## Routes

| Method | `/` (collection) | `/{id}` (item) |
|---|---|---|
| GET | List `{data:[...], count}` | Record (+ `ETag` on versioned models), or 404 `{"message":"Not Found","type":"danger"}` |
| QUERY | List, params in JSON body | Same as GET |
| POST | One object: single insert. `{"data":[...]}`: bulk insert | Single insert; path id ignored |
| PUT | Bulk update/upsert `{"data":[...]}` (400 without) | Update row id (404 if not visible) |
| PATCH | 405 | Partial update (404 if not visible) |
| DELETE | Delete by filter body (400 if empty) | Delete row id (404 if not visible) |

Single writes return the stored record. Bulk returns `{"rows":n,"message":"...","type":"success"}`.
Item routes refuse `{"data":[...]}` bodies on every method with 400
`Bulk writes ({"data": [...]}) use the collection route`.
Every 405 has the JSON body `{"message":"Method Not Allowed","type":"danger"}`.

## RouterOptions

Fields (all `bool`): `list`, `get`, `create` (`POST /` with one object, `POST /{id}`),
`bulk_create` (`POST /` with `data`), `update` (`PUT /{id}`), `bulk_update` (`PUT /`), `patch`, `delete` (`DELETE /{id}`),
`bulk_delete` (`DELETE /`), `export` (csv/xml). `QUERY` counts as `list`/`get`.

```rust
// Read-only resource: list, get, export. Writes → 405.
let r = create_model_router_with(model, RouterOptions::read_only());

// No bulk delete, no export.
let opts = RouterOptions { bulk_delete: false, export: false, ..RouterOptions::default() };
let r = create_model_router_with(model, opts);
```

Disabled operation → 405. Disabled export → 400 `Export is disabled`.

## Query parameters (also QUERY body keys)

| Param | Meaning |
|---|---|
| `page` | from 0 |
| `size` / `limit` | default 30, max 200 |
| `sort=-age,name` | `-` desc |
| `search=term` | LIKE over `search_columns` |
| `select_columns=a,b` | projection |
| `include=comments,author` | attach relations (`has_many` / `belongs_to`); unknown name → 422 |
| `output_content_type=csv` \| `xml` | export format |
| `col=value` | filter, prefixes: `>=5` `<=` `>` `<` `!x` `%x%` `!%x%` `in(a,b)` `!in(a,b)` |
| `filter=[[["col","op",value],...],...]` | raw: OR of AND-groups |

In a QUERY body, JSON `null` means `IS NULL`. Naming a hidden column in a filter, `sort`,
`select_columns` or a delete body → 422 `Invalid filter Inputs`.

## Hidden columns

Columns in `.hidden([...])` are never in responses. The router also strips them from client
write bodies, so a client cannot set `password_hash`; set it in a `before_save` hook.

## Optimistic locking (models with `.version_column(..)`)

- `GET /{id}` and every single-write response carry `ETag: "<version>"`.
- `PUT`/`PATCH /{id}` need the version: in the body (`"version": 3`) or as `If-Match: "3"`
  (quotes and `W/` stripped; a body version wins).
- Stale → 409 `Record has been modified by another request.`; missing → 428
  `The version field is required for this update.`
- `DELETE /{id}` with `If-Match` deletes only at that version (stale → 409); without it,
  deletes unconditionally.
- Bulk `PUT /` items carry their own version; errors are prefixed `data.<i>: `.

## Scope and CurrentUser

The router has no auth. Your middleware verifies the caller and inserts two request
extensions; the router copies them into the `Context` of every model call:

| Extension | Becomes | Used for |
|---|---|---|
| `CurrentUser(json!({...}))` | `ctx.user` | hooks (audit columns, role checks) |
| `Scope::new([("col", json!(v)), ...])` | `ctx.scope` | row permissions, enforced by the model |

A scope value is a scalar (`=`) or an array (`IN`). The model ANDs it into every read
filter, update target and delete, writes scalar values into every insert, checks
`belongs_to` keys and filters `?include=` rows. No `Scope` = every row, unless the model has
`.require_scope([...])` (then 401 `Missing scope: <col>`).

```rust
async fn auth(mut req: Request, next: Next) -> Result<Response, StatusCode> {
    let h = req.headers();
    let tenant = h.get("x-tenant").and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?.to_string();
    req.extensions_mut().insert(CurrentUser(json!({ "tenant_id": tenant })));
    req.extensions_mut().insert(Scope::new([("tenant_id", json!(tenant))]));
    Ok(next.run(req).await)
}

let app = Router::new()
    .nest("/notes", create_model_router(notes))
    .layer(middleware::from_fn(auth));

// Nested child route: add the parent id from the path to the scope.
async fn product_scope(Path(p): Path<HashMap<String, String>>, mut req: Request, next: Next)
    -> Response
{
    let mut scope = req.extensions().get::<Scope>().cloned().unwrap_or_default();
    scope.0.insert("product_id".into(), json!(p["product_id"]));
    req.extensions_mut().insert(scope);
    next.run(req).await
}
Router::new().nest(
    "/products/{product_id}/comments",
    create_model_router(comments).route_layer(middleware::from_fn(product_scope)),
)
```

The item id param is internally `__dbmr_id`, so a parent `{id}` never clashes. More
patterns (owner scope, multi-tenant users, admins, role middleware): `examples.md` §15-18.

## OpenAPI and health

```rust
let opts = RouterOptions::default();
// Build the spec before the routers take ownership of the models.
let spec = openapi("My API", "1.0.0", &[("/users", &users, &opts)]);   // OpenAPI 3.1 JSON Value
let app = Router::new()
    .nest("/users", create_model_router_with(users, opts))
    .nest("/openapi.json", openapi_route(spec))   // GET → the spec
    .nest("/health", health_router(db.clone()));  // GET → 200 {"status":"ok"} | 503 {"status":"unavailable"}
```

The spec covers only enabled operations, omits hidden columns, and documents
`ETag`/`If-Match`/409/428 for versioned models. `QUERY` is not described (no OpenAPI verb).
`health_router` takes `Arc<dyn DatabaseAdapter>` and calls `DatabaseAdapter::ping()`.

## Context in hooks

Router-built `Context`: `ctx.user` = `CurrentUser` JSON (or `null`), `ctx.scope` = the
`Scope` map (or empty), `ctx.request` = `{method, path, params, query, headers, body}`,
`ctx.system` = `false`.

## Errors

`{"message": "...", "type": "danger"}`.

| Status | When |
|---|---|
| 400 | Malformed JSON (`Malformed JSON body`), bulk `PUT /` without `data`, `data` array on an item route, empty delete filter, `Export is disabled` |
| 401 | `Missing scope: <col>` (model has `require_scope`, request has no such `Scope` value, or it is `null`) |
| 403 | Only from your own middleware (dbmr never returns it) |
| 404 | Row missing, outside the scope, or soft-deleted |
| 405 | Unsupported or disabled method (JSON body) |
| 409 | Stale version (optimistic locking) |
| 422 | Rule failure, `Invalid id value`, `Invalid filter Inputs` (incl. hidden columns), `Unknown include "<name>"`, `Invalid reference: <fk>` (foreign key outside the scope), `The <col> is outside the scope.` (insert under a list scope), `Invalid field name` (Mongo `$` keys) |
| 428 | Versioned write without a version |
| 500 | Database/encryption; body `Internal server error` unless `NODE_ENV=development\|test` |

Custom errors from hooks: `AppError::BadRequest` 400, `Unauthorized` 401, `NotFound` 404,
`Conflict` 409, `UnprocessableEntity`/`Validation` 422, `PreconditionRequired` 428,
`Internal`/`Database`/`Encryption` 500. `dbmr::http::error_response(&err)` renders the same
body for your own handlers.

## Testing

```rust
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt; // oneshot

let res = app.clone().oneshot(
    Request::builder().method("POST").uri("/notes")
        .header("content-type", "application/json").header("x-tenant", "a")
        .body(Body::from(r#"{"title":"hi"}"#)).unwrap()
).await.unwrap();
assert_eq!(res.status(), StatusCode::OK);
let etag = res.headers().get("etag").cloned();   // versioned models only
let bytes = axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap();
let rec: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
```

Use `SqliteAdapter::connect("sqlite::memory:")` in tests (kept on one connection). Patterns
to copy: `crates/dbmr-http/tests/route_tests.rs`, `crates/dbmr-http/tests/router_features_tests.rs`.
