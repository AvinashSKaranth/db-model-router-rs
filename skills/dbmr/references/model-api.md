# Model API

```rust
use dbmr::core::filter::{FilterExpr, Operator};
use dbmr::core::types::row;     // row([("col", value.into()), ...]) -> RowMap
use dbmr::{Context, DbValue, Model, ModelHooks, Page, Query, RowMap, Scope, Transaction,
           TypedModel, AppError, EncryptionConfig, Result};
```

## Builder

`Model::builder(adapter: Arc<dyn DatabaseAdapter>, table)`. Adapter constructors return
`Arc<SqliteAdapter>` etc., which coerces at the call site (pass `db.clone()` to reuse it).

| Method | Effect |
|---|---|
| `.column(name, rules)` / `.columns([(name, rules), ...])` | Declare a column. Only declared columns are written. |
| `.primary_key("id")` | Default `id`. |
| `.unique(["email"])` (repeatable) / `.unique_keys(vec![vec![..]])` | Upsert conflict targets. |
| `.safe_delete("is_deleted")` | `remove` sets flag to 1/true; reads hide flagged rows. |
| `.search_columns(["title"])` | Columns for `search` (LIKE `%term%`, OR-ed). |
| `.allow_destructive(true)` | Allow `remove` with empty criteria. |
| `.require_scope(["tenant_id"])` | Fail closed: calls without that scope column are 401 `Missing scope: <col>` (see Scope). |
| `.hidden(["password_hash"])` | Never returned by reads, single-write results or events. Still written (e.g. by a hook). |
| `.version_column("version")` | Optimistic locking (see below). Auto-declared `integer` if not declared. |
| `.created_at("created_at")` | Set to current UTC time on insert; clients cannot set it. |
| `.modified_at("updated_at")` | Set to current UTC time on every write; clients cannot set it. |
| `.hooks(ModelHooks::new()...)` | See Hooks. |
| `.encryption(cfg)` / `.encryption_shared(Option<Arc<cfg>>)` | Required if any rule has `encrypted`. |
| `.events(Arc<dyn EventSink>)` | Change events. |
| `.options(ModelOptions{..})` | Replaces **all** options (incl. hidden/version/soft delete set earlier); call it first or not at all. |
| `.build()?` | Validates every rule. |

Shorthand: `Model::define(db, "users", &[("id","integer"),("email","required|email")], "id", &["email"])?`.

`Model` is cheap to clone (`Arc` inside). Getters: `pk()`, `table_name()`, `structure()`,
`options()`, `adapter()`, `hidden_columns()`, `version_column()`, `relations()`.

Derived models (each returns a new `Model`, the original is unchanged):

- `model.with_tx(&tx)`: bound to a transaction.
- `model.with_hidden()`: returns hidden columns too (server-side checks such as a password
  comparison). Never pass it to `create_model_router`.
- `model.has_many("comments", &comments, "post_id")` / `model.belongs_to("author", &users, "author_id")`:
  relations for `Query::include` (see Relations).
- `model.typed::<T>()`: `TypedModel<T>` (see Typed models).

## Rule strings

Pipe-separated: `"required|string|maxLength:100"`.

- Types: `string`, `integer`, `numeric`, `decimal`, `boolean`, `object`, `array`, `datetime`, `email`.
- Constraints: `required`, `nullable`, `maxLength:n`, `minLength:n`, `max:n`, `min:n`,
  `regex:pattern` (split at first `:` only), `in:a,b`, `notIn:a,b`.
- Flag: `encrypted`. Dotted name (`"profile.ssn"`) encrypts a key inside a JSON column.
- Schema-only, ignored at runtime: `auto_increment`, `sometimes`.

Messages (node-input-validator compatible), returned as 422: `The title field is mandatory.`,
`The age must be an integer.` ... Bulk messages are prefixed `data.<i>.`.

`object`/`array` columns are stored as JSON text where the DB has no JSON type and decoded
back to JSON on read (every adapter).

## Operations (all `async`, all take `&Context`)

| Call | Returns | Notes |
|---|---|---|
| `insert(row, ctx)` | `Result<Option<RowMap>>` | Stored record (re-read, decrypted, hidden columns removed). |
| `insert_many(rows, ctx)` | `Result<WriteSummary>` | `{rows, message}` |
| `update(row, ctx)` | `Result<Option<RowMap>>` | Full validation incl. PK. |
| `update_many(rows, ctx)` | `Result<WriteSummary>` | |
| `upsert(row, ctx)` | `Result<Option<RowMap>>` | Conflict target: first unique key every row fully supplies, else PK, else plain insert. |
| `upsert_many(rows, ctx)` | `Result<WriteSummary>` | |
| `patch(row, ctx)` | `Result<Option<RowMap>>` | Only supplied keys validated/changed; PK required (`422 Primary key is required for patch`). `None` = no such row (or outside the scope). |
| `remove(criteria: Value, ctx)` | `Result<WriteSummary>` | Criteria: scalar PK, `[1,2]` PK IN, `{"status":"old"}`, `{"filter":[...]}`. |
| `remove_where(&FilterExpr, ctx)` | `Result<WriteSummary>` | Same guard/hooks. |
| `by_id(id, ctx)` | `Result<Option<RowMap>>` | `null`/`""` id → `422 Invalid id value`. |
| `find(Query, ctx)` | `Result<FindResult>` | `{data, count}`; every match, no paging; `limit` caps rows. |
| `find_one(Query, ctx)` | `Result<Option<RowMap>>` | |
| `list(Query, ctx)` | `Result<FindResult>` | Paged: `page`, `size` (default 30, max 200). |
| `count(&Query, ctx)` | `Result<u64>` | |

Rows: `RowMap = IndexMap<CompactString, DbValue>`. Build with
`row([("title", "hi".into()), ("qty", 3.into())])`. `DbValue` variants: `Null`, `Bool`,
`Int(i128)`, `Float`, `Decimal`, `Text`, `Bytes`, `Json`. Convert with
`dbmr::core::types::{row_to_json, json_to_row}` and `DbValue::to_json()`.

## Optimistic locking (`.version_column("version")`)

DDL: `version INTEGER NOT NULL DEFAULT 1`.

- Insert sets it to 1. Clients cannot choose it.
- `update` / `patch` / `upsert` of an existing row must carry the current value. The write is
  `SET ..., version = v + 1 WHERE pk = .. AND version = v`, atomic on every adapter.
- Stale value: `AppError::Conflict` 409 `Record has been modified by another request.`
- Missing value: `AppError::PreconditionRequired` 428 `The version field is required for this update.`
- Row does not exist: `Ok(None)` (HTTP 404). An upsert that creates a new row needs no version.
- `*_many` run row by row; errors are prefixed `data.<i>: `.
- `remove` takes the version as ordinary criteria (`json!({"id": 1, "version": 3})`); it is
  optional there.

Constants: `dbmr::core::{CONFLICT_MESSAGE, VERSION_REQUIRED_MESSAGE}`.

## Timestamps

`.created_at(col)` / `.modified_at(col)` store RFC 3339 UTC text, e.g.
`2026-01-01T00:00:00.000Z` (a `TEXT` or native timestamp column both work). Clients cannot set
them. Without these builder calls the default `created_at`/`modified_at` name variants
(`createdAt`, `updated_at`, ...) are still stripped from client writes, and the database
default/trigger must fill them.

## Query and FilterExpr

```rust
let q = Query::new()
    .eq("status", "active")
    .and("age", Operator::Gte, 18)
    .sort("-created_at,name")        // '-' = desc
    .select(["id", "name"])
    .search("ann")                    // over search_columns
    .include(["comments"])            // relations, see below
    .page(0).size(50);                // list; .limit(n) for find
let q = q.with_count(false);          // skip COUNT(*) when you don't need it

// OR of AND-groups: (a=1 AND b=2) OR (c IS NULL)
let f = FilterExpr::eq("a", 1).and("b", Operator::Eq, 2)
    .or(FilterExpr::default().and("c", Operator::IsNull, DbValue::Null));
let q = Query::filter(f);
```

Operators: `Eq NotEq Gt Lt Gte Lte Like NotLike In NotIn IsNull IsNotNull`. For `In`, pass a JSON array (`json!([1,2])`) or
`DbValue::Json(json!([1,2]))`. Filter values on `integer`/`numeric` columns are coerced to
numbers. `Query::from_json(&json, pk)` parses the HTTP payload form (incl. `include`).

## Types and record shape (same on every adapter)

Values follow the declared rule on every adapter: writes convert text forms (`"9"` → `9` for `integer`/`numeric`, `"true"`/`"false"`/`"1"`/`"0"` → boolean) after validation, reads convert driver forms (`0`/`1` → `false`/`true`, `9.00` → `9`, numbers in a `string` column → text), and filter values get the same conversion. Every declared column is returned (null when unset) in declared order after the primary key, then undeclared columns. Columns without a type rule pass through as the database returns them.

Declared unique keys (`.unique(["sku", "grp"])`) are checked on insert, single and bulk, on
every adapter: 409 `Duplicate value for unique key (sku, grp).` (bulk: `data.<i>: ` prefix).
Keys with a NULL part never conflict. The check is not atomic on MongoDB/Redis/DynamoDB; add
a unique index there if concurrent inserts can race.

## Relations and include

```rust
let posts = posts
    .has_many("comments", &comments, "post_id")     // comments.post_id = posts.id → array
    .belongs_to("author", &users, "author_id");     // posts.author_id = users.id → object or null
let page = posts.list(Query::new().include(["comments", "author"]), &ctx).await?;
```

Each included relation costs one extra query (no N+1). Includes go one level deep (the
related model's own relations are not followed). An unknown name → 422 `Unknown include "<name>"`.
Works on `find`, `find_one`, `list` (HTTP `?include=` on list and `GET /{id}`).

Under a scope (next section), included rows are filtered on each scope column the related
model declares, and every write checks `belongs_to` foreign keys against the parent inside
the scope: outside → 422 `Invalid reference: <fk>` (bulk: `data.<i>: ` prefix). Without a
scope there is no reference check; use a `FOREIGN KEY` in the DDL.

## Scope (row permissions)

`Context.scope` is a map of column → value (`=`) or array (`IN`). HTTP requests get it from
the `Scope` extension your auth middleware inserts; code builds it with
`Context::new().with_scope([("tenant_id", "a")])`. Columns the model does not declare are
ignored for that model.

| Operation | Effect of the scope |
|---|---|
| `find` / `find_one` / `list` / `count` / `by_id` | ANDed into every OR-group, after `before_find` |
| `insert` / `insert_many` | scalar values are written into the row (overriding the caller); with an array the row must name one value, else 422 `The <col> is outside the scope.` |
| `update` / `upsert` (+ `_many`) | the target row (by PK, or by the conflict key for upsert) must be inside the scope, else 404 / `data.<i>: Not Found`; the scope values are written as for insert |
| `patch` | PK AND scope; `None` if outside |
| `remove` / `remove_where` | ANDed in; the mass-delete guard still looks at the caller's criteria only |
| includes, `belongs_to` checks | see Relations |

Fail closed with `.require_scope(["tenant_id"])`: a missing or `null` scope value → 401
`Missing scope: tenant_id`. A `null` value in any declared scope column is 401 too.
`Context::system()` skips `require_scope` and has an empty scope: every row, for jobs.

```rust
let a = Context::new().with_scope([("tenant_id", "a")]);
let many = Context::new().with_scope([("tenant_id", json!(["a", "b"]))]);
let audit = Context::with_user(json!({"id": 7})).with_scope([("tenant_id", "a")]);
let job = Context::system();
```

The update/upsert target check reads before it writes (not atomic). A concurrent write that
moves the row between the two statements is not covered; keep scope columns immutable.
Worked examples: `examples.md` §14-18.

## Typed models

```rust
#[derive(serde::Serialize, serde::Deserialize)]
struct Note { id: Option<i64>, title: String, body: Option<String> }

let notes: TypedModel<Note> = model.typed::<Note>();
let saved: Option<Note> = notes.insert(&Note { id: None, title: "hi".into(), body: None }, &ctx).await?;
let page: Page<Note> = notes.list(Query::new(), &ctx).await?;   // page.data, page.count
let patched = notes.patch(&json!({"id": 1, "title": "new"}), &ctx).await?;
```

Methods: `insert insert_many update update_many upsert upsert_many patch by_id find find_one list`
(same semantics as `Model`; `*_many` return `WriteSummary`). `insert`/`insert_many` drop
`None` fields so column defaults apply; updates send them as `NULL`. `patch` takes any
`Serialize` containing the PK. `typed.model()` returns the row model.

## Hooks

Each hook gets `(value, Context)` and returns `Result<value>` (an `Err` aborts the call and
maps to its HTTP status).

```rust
let hooks = ModelHooks::new()
    .before_save(|mut row, ctx| async move {
        if let Some(uid) = ctx.user.get("id").and_then(|v| v.as_str()) {
            row.insert("updated_by".into(), uid.into());
        }
        Ok(row)
    })
    .before_delete(|criteria, ctx| async move {
        if ctx.user.get("role").and_then(|v| v.as_str()) != Some("admin") {
            return Err(AppError::BadRequest("Only admins delete".into()));
        }
        Ok(criteria)
    })
    .after_find(|rows, _ctx| async move { Ok(rows) });
```

| Hook | Value | Runs on |
|---|---|---|
| `before_save` | `RowMap` | every single write, then the op-specific one |
| `before_create` / `before_update` / `before_upsert` | `RowMap` | that op (patch runs `before_save` + `before_update`) |
| `before_delete` | `serde_json::Value` criteria | `remove`, may replace criteria |
| `before_find` | `FilterExpr` | reads, may add conditions |
| `after_save`, `after_create`, `after_update`, `after_upsert` | `RowMap` | single writes; **the returned row is what the caller/HTTP gets** |
| `after_find` | `Vec<RowMap>` | every read (list, find, by_id) |
| `after_delete` | `WriteSummary` | after remove |
| `after_bulk(op, summary, ctx)` | `()` | `*_many` |

Order of a single write: before hooks → strip timestamps / soft-delete flag → validate →
drop undeclared keys → set version (insert) and app timestamps → encrypt → write → re-read →
decrypt → after hooks (they still see hidden columns) → remove hidden columns → emit event.

## Transactions (SQLite, PostgreSQL/CockroachDB, MySQL/MariaDB, MSSQL, Oracle)

```rust
let tx = Transaction::begin(db.as_ref()).await?;
let m = model.with_tx(&tx);           // per model
m.insert(row, &ctx).await?;
let sp = tx.savepoint().await?;       // nested
sp.rollback().await?;
tx.commit().await?;                   // events are emitted only now
```

Dropping without commit rolls back once every `with_tx` model clone is dropped too; prefer
explicit `rollback()` (Oracle can hold row locks briefly otherwise). MongoDB, Redis and
DynamoDB return `Active database adapter does not support transactions.` from `begin`.

## Encryption

```rust
// Key refs: "env:VAR" (key = SHA-256 of the variable's value) or
// "raw:<base64 of exactly 32 bytes>". Any other string is an error.
let mut old = std::collections::HashMap::new();
old.insert("1".to_string(), "env:OLD_KEY".to_string());
let cfg = EncryptionConfig::new("env:NEW_KEY", 2, Some(&old))?;   // rotation: old keys decrypt
let people = Model::builder(db, "people")
    .column("ssn", "encrypted|string").encryption(cfg).build()?;
```

Values are AES-256-GCM `enc:v<version>:<base64>`. Encrypted columns cannot be filtered or
sorted meaningfully. `EncryptionConfig::new` fails (`AppError::Encryption`) for a bare
string, an unset/empty env var, or a `raw:` key that is not 32 bytes.

## Events

```rust
#[async_trait::async_trait]
impl dbmr::core::events::EventSink for MySink {
    async fn emit(&self, table: &str, operation: &str, data: serde_json::Value) { /* ... */ }
}
Model::builder(db, "orders").events(Arc::new(MySink)).build()?;
```

`operation` ∈ `insert|update|upsert|delete`; `data` is plaintext without hidden columns.
Fire-and-forget (spawned, errors ignored); buffered until outermost commit inside a
transaction. Kafka: see `recipes.md`.

## Schema file

```rust
let adapter: Arc<dyn DatabaseAdapter> = db;   // declare with this type
let models = dbmr::load_models_from_schema(&adapter, r#"{
  "adapter": "postgres", "framework": "axum",
  "tables": {
    "users": { "pk": "id",
               "columns": { "id": "integer", "email": "required|email",
                            "password_hash": "string", "version": "integer", "is_deleted": "boolean",
                            "created_at": "datetime", "updated_at": "datetime" },
               "unique": [["email"]], "softDelete": "is_deleted",
               "search_columns": ["email"],
               "hidden": ["password_hash"], "version": "version",
               "timestamps": { "created_at": "created_at", "modified_at": "updated_at" } },
    "posts": { "pk": "id", "columns": { "id": "integer", "user_id": "required|integer" } }
  },
  "relationships": [ { "parent": "users", "child": "posts", "foreignKey": "user_id" } ]
}"#, None)?;   // IndexMap<table, Model>; pass Some(&Arc<EncryptionConfig>) for encrypted columns
```

Table keys: `pk`, `columns`, `unique`, `softDelete` (camelCase; `soft_delete` is ignored),
`search_columns`, `hidden` (existing columns), `version` (an existing integer column; unlike
the builder it is not auto-declared), `timestamps` (set by the model, like
`.created_at`/`.modified_at`), `parent`. Each `relationships` entry wires `has_many` named
after the child table on the parent (`users?include=posts`) and `belongs_to` named after the
parent table on the child (`posts?include=users`).
