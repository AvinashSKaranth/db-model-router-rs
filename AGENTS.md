# AGENTS.md: building apps on dbmr

Exact contract for code generators. If this file and the code disagree, the code wins.
In that case, report the mismatch.

## Imports you need

```rust
use dbmr::adapters::SqliteAdapter;             // or PostgresAdapter, ... (feature-gated)
use dbmr::core::types::row;                    // row([("col", value.into()), ...]) -> RowMap
use dbmr::{create_model_router, Context, CurrentUser, DbValue, EncryptionConfig, Model, ModelHooks, Query,
           Scope, Transaction, AppError, Result};
```

## Rules that generated code gets wrong

1. **Tables must already exist.** dbmr has no migrations. Run your own DDL first.
2. **`Model::builder(...).build()` returns `Result`.** An unknown rule fails at build time
   with `Validation Rule: <rule> does not exists.`. Valid rules are listed in `README.md` →
   *Model API*.
3. **Single insert over HTTP is `POST /users` with one JSON object** (`POST /users/{any}`
   also works; the path id is ignored). Bulk insert is `POST /users` with `{"data":[...]}`.
   Item routes (`/users/{id}`) refuse `data` arrays with 400.
4. **Bulk responses are summaries**, `{"rows":n,"message":"...","type":"success"}`, not
   records. A single insert returns the stored record.
5. **Permissions = a `Scope` from your auth middleware**, not hand-written filters or
   hooks. The middleware does
   `req.extensions_mut().insert(CurrentUser(json!({...})))` (hooks read it as `ctx.user`)
   and `req.extensions_mut().insert(Scope::new([("tenant_id", json!(t))]))`. The model ANDs
   the scope into every read, update and delete, and writes it into every insert. A list
   value means IN. Rows outside the scope look missing (404).
6. **Nested child routes:** add the path parameter to the scope with a `route_layer`:
   `Router::new().nest("/products/{product_id}/comments", create_model_router(comments).route_layer(middleware::from_fn(product_scope)))`,
   where `product_scope` takes `Path<HashMap<String, String>>`, clones the request's
   `Scope` (or `Scope::default()`), inserts `product_id`, and re-inserts it.
7. **Every model method takes `&Context`.** Jobs use
   `Context::new().with_scope([("tenant_id", t)])` per tenant, or `Context::system()` when
   they must see every row. `Context::with_user(json!(..))` when hooks need the user.
8. **Transactions work on every SQL adapter** (SQLite, PostgreSQL/CockroachDB,
   MySQL/MariaDB, MSSQL, Oracle). MongoDB, Redis and DynamoDB return
   `Active database adapter does not support transactions.`. Use `model.with_tx(&tx)` for
   each model inside the transaction.
9. **Soft delete:** with `.safe_delete("col")`, `remove` sets the flag and reads hide those
   rows. The column must exist, with a 0/false default.
10. **Mass delete is blocked.** `remove(json!({}))` returns 400 unless the model was built
    with `.allow_destructive(true)`.
11. **Encrypted columns** (`"encrypted|string"`) require `.encryption(EncryptionConfig::new("env:KEY_VAR", 1, None)?)`,
    and `build()` fails without it. The key ref is `env:VAR` or `raw:<base64 of exactly 32 bytes>`;
    a bare string is an error.
12. **Filters over HTTP** use value prefixes (`?age=>=18`, `?name=%ann%`, `?id=in(1,2)`).
    URL-encode them (`%3E%3D18`). The raw form is `{"filter":[[["age",">=",18]]]}`
    (OR of AND-groups).
13. **Paging:** `page` starts at 0, `size` defaults to 30 and is capped at 200. `count` is
    the total match count, not the page length.
14. **Errors:**
    - status codes: 400 malformed input, 401 missing scope (`require_scope`), 404 not visible,
      405 method or disabled route, 409 stale version, 422 validation, 428 version missing,
      500 database;
    - body shape: `{"message","type":"danger"}`;
    - 500 messages are hidden unless `NODE_ENV=development|test`.
15. **Fail closed with `.require_scope(["tenant_id"])`.** Without it, a request with no
    `Scope` reaches every row. With it, a missing or `null` scope value is 401
    `Missing scope: tenant_id`; only `Context::system()` skips the check.
16. **Optimistic locking:** with `.version_column("version")` (DDL
    `version INTEGER NOT NULL DEFAULT 1`), every update/patch/upsert of an existing row must
    send the current `version` in the body or as `If-Match` (GET by id returns it as `ETag`).
    Stale → 409, missing → 428. Re-read and retry on 409.
17. **Hidden columns:** `.hidden(["password_hash"])` columns are never returned and are
    stripped from client writes; set them in a `before_*` hook. Filtering, sorting or
    selecting them is 422. Use `model.with_hidden()` server-side only (e.g. login checks).
18. **Read-only or partial routers:** `dbmr::http::create_model_router_with(model,
    RouterOptions::read_only())`. Disabled operations answer 405. Per-role route rules
    (viewers only read) are your own middleware.
19. **Adapter constructors return `Arc`.** Pass `SqliteAdapter::connect(url).await?` straight
    to `Model::builder`; do not wrap it in `Arc::new`.
20. **Relations follow the scope.** With a scope such as `{"tenant_id": "a"}`,
    `?include=` only returns related rows with the same `tenant_id` (when the related table
    has that column), and writing a `belongs_to` foreign key that points at another tenant's
    row is 422 `Invalid reference: <fk>`. Declare `belongs_to` on child models so writes are
    checked.
21. **Types follow the declared rule, not the database.** Declare every column with a type
    rule. `"9"` is stored as `9` in an `integer` column, booleans always read `true`/`false`,
    unset declared columns read `null`, and record keys are the primary key, then declared
    columns in order. A declared `.unique([...])` key is checked on insert on every adapter:
    409 `Duplicate value for unique key (..).`
22. **`list` pages, `find` does not.** `list` applies `page`/`size`; `find` returns every
    match (cap it with `.limit(n)`). Validation failures are `AppError::UnprocessableEntity`.
23. **Auth middleware must stay `Send`.** Read headers through `let h = req.headers();`.
    A closure that captures `&req` across `.await` breaks `middleware::from_fn` with a long
    `Service<Request>` trait error.

## Minimal working app

See `crates/dbmr/examples/quickstart.rs`, which is compiled and run with
`cargo run -p dbmr --example quickstart`. Every feature (scopes, jobs, role middleware,
nested routes, locking, relations, transactions, encryption, events, OpenAPI) is in
`crates/dbmr/examples/tour.rs` (`cargo run -p dbmr --example tour`), and explained in
`skills/dbmr/references/examples.md`.

## Verify your app

```bash
cargo test                      # use tower::ServiceExt::oneshot against the Router
```

`crates/dbmr-http/tests/route_tests.rs` shows the request/response patterns to copy;
`crates/dbmr-http/tests/scope_tests.rs` shows scope enforcement.
