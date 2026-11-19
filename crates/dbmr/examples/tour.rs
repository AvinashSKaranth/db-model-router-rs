//! Feature tour: every dbmr feature in one runnable file, checked with asserts.
//!
//!     cargo run -p dbmr --example tour
//!
//! Each section is a function. `skills/dbmr/references/examples.md` copies from here, so
//! keep the two in step. Everything runs on in-memory SQLite.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::{Path, Request};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use dbmr::adapters::SqliteAdapter;
use dbmr::core::events::EventSink;
use dbmr::core::filter::{FilterExpr, Operator};
use dbmr::core::types::{row, row_to_json};
use dbmr::http::{create_model_router_with, health_router, openapi, openapi_route, RouterOptions};
use dbmr::{
    create_model_router, AppError, Context, CurrentUser, DatabaseAdapter, DbValue,
    EncryptionConfig, Model, ModelHooks, Query, Result, Scope, Transaction,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tower::ServiceExt;

// ------------------------------------------------------------------ helpers

/// A fresh in-memory database with the given DDL. dbmr never creates tables.
async fn db(ddl: &[&str]) -> Arc<dyn DatabaseAdapter> {
    let db = SqliteAdapter::connect("sqlite::memory:").await.unwrap();
    for sql in ddl {
        db.query(sql, &[]).await.unwrap();
    }
    db
}

/// Send one request to a router; returns status, headers and the body text.
async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Option<Value>,
) -> (StatusCode, HeaderMap, String) {
    let mut req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let req = req
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

fn js(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or_else(|_| panic!("not JSON: {text}"))
}

// ------------------------------------------------------------------ 1. CRUD and queries

async fn crud_and_queries() -> Result<()> {
    let db = db(&["CREATE TABLE users (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        email TEXT NOT NULL UNIQUE, -- upsert on a unique key needs the DB constraint
        age INTEGER,
        active INTEGER NOT NULL DEFAULT 1
    )"])
    .await;
    let users = Model::builder(db, "users")
        .column("id", "integer")
        .column("name", "required|string|maxLength:50")
        .column("email", "required|email")
        .column("age", "integer|nullable")
        .column("active", "boolean")
        .unique(["email"])
        .search_columns(["name", "email"])
        .build()?;
    let ctx = Context::new();

    // insert returns the stored record (types follow the rule: "36" -> 36).
    let ann = users
        .insert(
            row([
                ("name", "Ann".into()),
                ("email", "ann@x.io".into()),
                ("age", "36".into()),
            ]),
            &ctx,
        )
        .await?
        .unwrap();
    assert_eq!(ann["id"], DbValue::Int(1));
    assert_eq!(ann["age"], DbValue::Int(36));
    assert_eq!(ann["active"], DbValue::Bool(true));

    // Bulk insert returns a summary, not records.
    let summary = users
        .insert_many(
            vec![
                row([
                    ("name", "Bob".into()),
                    ("email", "bob@x.io".into()),
                    ("age", 17.into()),
                ]),
                row([
                    ("name", "Cid".into()),
                    ("email", "cid@x.io".into()),
                    ("age", 52.into()),
                ]),
                row([("name", "Dee".into()), ("email", "dee@x.io".into())]),
            ],
            &ctx,
        )
        .await?;
    assert_eq!(summary.rows, 3);

    // by_id, update (full row), patch (only the given columns), upsert (insert or update).
    assert_eq!(users.by_id(2, &ctx).await?.unwrap()["name"], "Bob".into());
    users
        .update(
            row([
                ("id", 2.into()),
                ("name", "Bobby".into()),
                ("email", "bob@x.io".into()),
                ("age", 18.into()),
            ]),
            &ctx,
        )
        .await?;
    let dee = users
        .patch(row([("id", 4.into()), ("age", 44.into())]), &ctx)
        .await?
        .unwrap();
    assert_eq!(
        (dee["name"].clone(), dee["age"].clone()),
        ("Dee".into(), 44.into())
    );
    // Upsert keys on a declared unique key when the row carries it, else on the pk.
    users
        .upsert(
            row([
                ("name", "Eve".into()),
                ("email", "ann@x.io".into()),
                ("age", 37.into()),
            ]),
            &ctx,
        )
        .await?;
    assert_eq!(users.by_id(1, &ctx).await?.unwrap()["name"], "Eve".into());

    // Query builder: AND conditions, sort ("-col" = descending).
    // list() returns one page (page from 0, size default 30, max 200); find() returns
    // every match (cap it with .limit(n)).
    let adults = users
        .list(
            Query::new()
                .and("age", Operator::Gte, 18)
                .eq("active", true)
                .sort("-age")
                .page(0)
                .size(2),
            &ctx,
        )
        .await?;
    assert_eq!(adults.count, 4, "count is all matches, not the page length");
    assert_eq!(adults.data.len(), 2);
    assert_eq!(adults.data[0]["name"], "Cid".into());
    let first_two = users.find(Query::new().sort("id").limit(2), &ctx).await?;
    assert_eq!(first_two.data.len(), 2);

    // OR of AND-groups with FilterExpr; select limits the returned columns.
    let teen_or_senior = FilterExpr::default()
        .and("age", Operator::Lt, 20)
        .or(FilterExpr::default().and("age", Operator::Gt, 50));
    let page = users
        .find(Query::filter(teen_or_senior).select(["id", "name"]), &ctx)
        .await?;
    assert_eq!(page.count, 2);
    assert_eq!(page.data[0].keys().collect::<Vec<_>>(), ["id", "name"]);

    // IN, LIKE and IS NULL.
    let q = Query::new().and("id", Operator::In, DbValue::Json(json!([1, 3])));
    assert_eq!(users.count(&q, &ctx).await?, 2);
    let q = Query::new().and("email", Operator::Like, "%@x.io");
    assert_eq!(users.count(&q, &ctx).await?, 4);
    let q = Query::new().and("age", Operator::IsNull, DbValue::Null);
    assert_eq!(users.count(&q, &ctx).await?, 0);

    // Full-text-ish search over search_columns, and find_one.
    let hit = users
        .find_one(Query::new().search("bobby"), &ctx)
        .await?
        .unwrap();
    assert_eq!(hit["id"], DbValue::Int(2));

    // The JSON filter form the HTTP layer uses works server-side too.
    let (q, _) = Query::from_json(&json!({"filter": [[["age", ">", 40]]]}), "id")?;
    assert_eq!(users.count(&q, &ctx).await?, 2);

    // Errors are typed. Match the variant or read status_code()/message().
    let err = users
        .insert(row([("email", "not-an-email".into())]), &ctx)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::UnprocessableEntity(_)), "{err:?}");
    assert_eq!(err.status_code(), 422);
    let dup = users
        .insert(
            row([("name", "X".into()), ("email", "bob@x.io".into())]),
            &ctx,
        )
        .await
        .unwrap_err();
    assert_eq!(dup.status_code(), 409, "{dup}");

    // Delete by criteria or by FilterExpr. An empty criteria (mass delete) is refused.
    assert_eq!(users.remove(json!({"id": 4}), &ctx).await?.rows, 1);
    let gone = users
        .remove_where(&FilterExpr::eq("name", "Cid"), &ctx)
        .await?;
    assert_eq!(gone.rows, 1);
    let err = users.remove(json!({}), &ctx).await.unwrap_err();
    assert_eq!(err.status_code(), 400, "{err}");
    println!("1. crud and queries: ok");
    Ok(())
}

// ------------------------------------------------------------------ 2. soft delete

async fn soft_delete() -> Result<()> {
    let db = db(&["CREATE TABLE posts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        title TEXT NOT NULL,
        is_deleted INTEGER NOT NULL DEFAULT 0
    )"])
    .await;
    let posts = Model::builder(db.clone(), "posts")
        .column("id", "integer")
        .column("title", "required|string")
        .safe_delete("is_deleted")
        .build()?;
    let ctx = Context::new();
    posts
        .insert_many(
            vec![row([("title", "a".into())]), row([("title", "b".into())])],
            &ctx,
        )
        .await?;
    posts.remove(json!({"id": 1}), &ctx).await?;
    // Reads hide the flagged row; the row is still in the table.
    assert!(posts.by_id(1, &ctx).await?.is_none());
    assert_eq!(posts.count(&Query::new(), &ctx).await?, 1);
    let raw = db
        .query("SELECT is_deleted FROM posts WHERE id = 1", &[])
        .await?;
    assert_eq!(raw[0]["is_deleted"], DbValue::Int(1));
    println!("2. soft delete: ok");
    Ok(())
}

// ------------------------------------------------------------------ 3. hooks, audit, hidden columns

/// Stand-in for argon2/bcrypt. Never store plain passwords.
fn hash(pw: &str) -> String {
    format!("hashed:{pw}")
}

async fn hooks_audit_hidden() -> Result<()> {
    let db = db(&["CREATE TABLE accounts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        email TEXT NOT NULL,
        password_hash TEXT,
        created_by INTEGER,
        updated_by INTEGER,
        created_at TEXT,
        modified_at TEXT
    )"])
    .await;
    let accounts = Model::builder(db, "accounts")
        .column("id", "integer")
        .column("email", "required|email")
        .column("password_hash", "string|nullable")
        .column("created_by", "integer|nullable")
        .column("updated_by", "integer|nullable")
        .column("created_at", "string|nullable")
        .column("modified_at", "string|nullable")
        // Never returned; stripped from HTTP writes; 422 in HTTP filters, sorts, selects.
        .hidden(["password_hash"])
        // Timestamps the model fills in.
        .created_at("created_at")
        .modified_at("modified_at")
        .hooks(
            ModelHooks::new()
                // ctx.user is whatever the auth middleware (or a job) put there.
                .before_create(|mut r, ctx| async move {
                    r.insert("created_by".into(), ctx.user["id"].clone().into());
                    Ok(r)
                })
                .before_save(|mut r, ctx| async move {
                    r.insert("updated_by".into(), ctx.user["id"].clone().into());
                    // Normalize input on every insert/update/upsert.
                    if let Some(DbValue::Text(e)) = r.get("email") {
                        let lower = e.to_lowercase();
                        r.insert("email".into(), lower.into());
                    }
                    Ok(r)
                })
                // after_find can redact or enrich rows on the way out.
                .after_find(|rows, _ctx| async move { Ok(rows) }),
        )
        .build()?;

    let ctx = Context::with_user(json!({"id": 7}));
    let acc = accounts
        .insert(
            row([
                ("email", "Ann@X.io".into()),
                // Server code may write hidden columns; HTTP clients cannot.
                ("password_hash", hash("s3cret").into()),
            ]),
            &ctx,
        )
        .await?
        .unwrap();
    assert_eq!(acc["email"], "ann@x.io".into());
    assert_eq!(acc["created_by"], DbValue::Int(7));
    assert!(!acc.contains_key("password_hash"));
    assert_ne!(acc["created_at"], DbValue::Null);

    // Login check: with_hidden() reads hidden columns. Keep it server-side.
    let login = accounts
        .with_hidden()
        .find_one(Query::new().eq("email", "ann@x.io"), &Context::new())
        .await?
        .unwrap();
    assert_eq!(login["password_hash"], hash("s3cret").into());

    // Plain reads never return it. (Over HTTP, filtering/sorting/selecting a hidden
    // column is 422; server code may filter on it, as the login above does.)
    let back = accounts.by_id(1, &ctx).await?.unwrap();
    assert!(!back.contains_key("password_hash"));
    println!("3. hooks, audit, hidden: ok");
    Ok(())
}

// ------------------------------------------------------------------ 4. optimistic locking

async fn optimistic_locking() -> Result<()> {
    let db = db(&["CREATE TABLE docs (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        body TEXT NOT NULL,
        version INTEGER NOT NULL DEFAULT 1
    )"])
    .await;
    let docs = Model::builder(db, "docs")
        .column("id", "integer")
        .column("body", "required|string")
        .column("version", "integer")
        .version_column("version")
        .build()?;
    let ctx = Context::new();
    let d = docs
        .insert(row([("body", "v1".into())]), &ctx)
        .await?
        .unwrap();
    assert_eq!(d["version"], DbValue::Int(1));

    // Every update must carry the version it read.
    let missing = docs
        .patch(row([("id", 1.into()), ("body", "v2".into())]), &ctx)
        .await
        .unwrap_err();
    assert_eq!(missing.status_code(), 428);
    let d = docs
        .patch(
            row([
                ("id", 1.into()),
                ("body", "v2".into()),
                ("version", 1.into()),
            ]),
            &ctx,
        )
        .await?
        .unwrap();
    assert_eq!(d["version"], DbValue::Int(2));
    // A second writer still holding version 1 loses: re-read and retry.
    let stale = docs
        .patch(
            row([
                ("id", 1.into()),
                ("body", "v2b".into()),
                ("version", 1.into()),
            ]),
            &ctx,
        )
        .await
        .unwrap_err();
    assert!(matches!(stale, AppError::Conflict(_)));
    println!("4. optimistic locking: ok");
    Ok(())
}

// ------------------------------------------------------------------ 5. relations + typed models

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Book {
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<i64>,
    author_id: i64,
    title: String,
}

async fn relations_and_typed() -> Result<()> {
    let db = db(&[
        "CREATE TABLE authors (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)",
        "CREATE TABLE books (id INTEGER PRIMARY KEY AUTOINCREMENT,
            author_id INTEGER NOT NULL, title TEXT NOT NULL)",
    ])
    .await;
    let authors = Model::builder(db.clone(), "authors")
        .column("id", "integer")
        .column("name", "required|string")
        .build()?;
    let books = Model::builder(db, "books")
        .column("id", "integer")
        .column("author_id", "required|integer")
        .column("title", "required|string")
        .build()?;
    // Relations return new models. belongs_to also checks foreign keys on write.
    let books = books.belongs_to("author", &authors, "author_id");
    let authors = authors.has_many("books", &books, "author_id");
    let ctx = Context::new();

    authors
        .insert(row([("name", "Le Guin".into())]), &ctx)
        .await?;
    // Typed access: serde structs in and out.
    let typed = books.typed::<Book>();
    let b = typed
        .insert(
            &Book {
                id: None,
                author_id: 1,
                title: "Earthsea".into(),
            },
            &ctx,
        )
        .await?
        .unwrap();
    assert_eq!(b.id, Some(1));
    let all = typed.find(Query::new(), &ctx).await?;
    assert_eq!(all.data[0].title, "Earthsea");

    // ?include= in code: related rows are attached under the relation name.
    let with_books = authors.find(Query::new().include(["books"]), &ctx).await?;
    let a = row_to_json(&with_books.data[0]);
    assert_eq!(a["books"][0]["title"], "Earthsea");
    let b = books.find(Query::new().include(["author"]), &ctx).await?;
    assert_eq!(row_to_json(&b.data[0])["author"]["name"], "Le Guin");

    // Referential integrity is the database's job (declare FOREIGN KEY in DDL). With a
    // Scope, belongs_to also refuses a foreign key outside the scope: see the HTTP section.
    println!("5. relations and typed models: ok");
    Ok(())
}

// ------------------------------------------------------------------ 6. transactions

async fn transactions() -> Result<()> {
    let db = db(&["CREATE TABLE wallets (id INTEGER PRIMARY KEY, balance INTEGER NOT NULL)"]).await;
    let wallets = Model::builder(db.clone(), "wallets")
        .column("id", "integer")
        .column("balance", "required|integer")
        .build()?;
    let ctx = Context::new();
    wallets
        .insert_many(
            vec![
                row([("id", 1.into()), ("balance", 100.into())]),
                row([("id", 2.into()), ("balance", 0.into())]),
            ],
            &ctx,
        )
        .await?;

    // Transfer: both writes commit or neither does. Bind each model with with_tx.
    let tx = Transaction::begin(db.as_ref()).await?;
    let w = wallets.with_tx(&tx);
    w.patch(row([("id", 1.into()), ("balance", 60.into())]), &ctx)
        .await?;
    w.patch(row([("id", 2.into()), ("balance", 40.into())]), &ctx)
        .await?;
    // Savepoint: a nested unit that can roll back alone.
    let sp = tx.savepoint().await?;
    wallets
        .with_tx(&sp)
        .patch(row([("id", 2.into()), ("balance", 9999.into())]), &ctx)
        .await?;
    sp.rollback().await?;
    tx.commit().await?;
    assert_eq!(
        wallets.by_id(2, &ctx).await?.unwrap()["balance"],
        DbValue::Int(40)
    );

    // Rollback discards everything.
    let tx = Transaction::begin(db.as_ref()).await?;
    wallets
        .with_tx(&tx)
        .patch(row([("id", 1.into()), ("balance", 0.into())]), &ctx)
        .await?;
    tx.rollback().await?;
    assert_eq!(
        wallets.by_id(1, &ctx).await?.unwrap()["balance"],
        DbValue::Int(60)
    );
    println!("6. transactions: ok");
    Ok(())
}

// ------------------------------------------------------------------ 7. encryption + events

/// Collects change events. Real sinks publish to Kafka (`dbmr::kafka`), NATS, a queue...
#[derive(Default)]
struct Recorder(Mutex<Vec<(String, String)>>);

#[async_trait]
impl EventSink for Recorder {
    async fn emit(&self, table: &str, operation: &str, _data: Value) {
        self.0
            .lock()
            .unwrap()
            .push((table.to_string(), operation.to_string()));
    }
}

async fn encryption_and_events() -> Result<()> {
    let db = db(&["CREATE TABLE patients (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        name TEXT NOT NULL,
        ssn TEXT
    )"])
    .await;
    // Real apps use "env:DBMR_KEY" (key = SHA-256 of that variable's value; use a long
    // random secret). "raw:<base64 of 32 bytes>" is for tests. Version 1 tags the
    // ciphertext for key rotation.
    let key = "raw:AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=";
    let events = Arc::new(Recorder::default());
    let patients = Model::builder(db.clone(), "patients")
        .column("id", "integer")
        .column("name", "required|string")
        .column("ssn", "encrypted|string")
        .encryption(EncryptionConfig::new(key, 1, None)?)
        .events(events.clone())
        .build()?;
    let ctx = Context::new();
    let p = patients
        .insert(
            row([("name", "Ann".into()), ("ssn", "123-45-6789".into())]),
            &ctx,
        )
        .await?
        .unwrap();
    // The model decrypts on read; the table holds ciphertext.
    assert_eq!(p["ssn"], "123-45-6789".into());
    let raw = db.query("SELECT ssn FROM patients", &[]).await?;
    assert_ne!(raw[0]["ssn"], "123-45-6789".into());

    // Events are fire-and-forget (spawned), so give the runtime a moment.
    patients.remove(json!({"id": 1}), &ctx).await?;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let seen = events.0.lock().unwrap().clone();
    assert_eq!(
        seen,
        [
            ("patients".into(), "insert".into()),
            ("patients".into(), "delete".into())
        ]
    );
    println!("7. encryption and events: ok");
    Ok(())
}

// ------------------------------------------------------------------ 8. scope in code

async fn scope_in_code() -> Result<()> {
    let db = db(&["CREATE TABLE tasks (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        tenant_id TEXT NOT NULL,
        title TEXT NOT NULL
    )"])
    .await;
    let tasks = Model::builder(db, "tasks")
        .column("id", "integer")
        .column("tenant_id", "required|string")
        .column("title", "required|string")
        // Fail closed: no tenant_id in the scope -> 401, unless Context::system().
        .require_scope(["tenant_id"])
        .build()?;
    let a = Context::new().with_scope([("tenant_id", "a")]);
    let b = Context::new().with_scope([("tenant_id", "b")]);

    // Inserts take the scope value, whatever the caller sent.
    let t = tasks
        .insert(row([("title", "a1".into())]), &a)
        .await?
        .unwrap();
    assert_eq!(t["tenant_id"], "a".into());
    tasks.insert(row([("title", "b1".into())]), &b).await?;
    let t = tasks
        .insert(row([("tenant_id", "b".into()), ("title", "a2".into())]), &a)
        .await?
        .unwrap();
    assert_eq!(t["tenant_id"], "a".into());

    // Reads, updates and deletes only see the scope's rows.
    assert_eq!(tasks.count(&Query::new(), &a).await?, 2);
    assert!(tasks.by_id(2, &a).await?.is_none());
    // Out-of-scope rows look missing: patch returns None (404 over HTTP).
    let hijack = tasks
        .patch(row([("id", 2.into()), ("title", "hijack".into())]), &a)
        .await?;
    assert!(hijack.is_none());
    assert_eq!(tasks.remove(json!({"id": 2}), &a).await?.rows, 0);

    // A list scope means IN: a user who belongs to several tenants. Inserts under a list
    // scope must name one of the values (else 422 "The tenant_id is outside the scope.").
    let both = Context::new().with_scope([("tenant_id", json!(["a", "b"]))]);
    assert_eq!(tasks.count(&Query::new(), &both).await?, 3);
    let err = tasks
        .insert(
            row([("tenant_id", "c".into()), ("title", "x".into())]),
            &both,
        )
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 422, "{err}");

    // No scope at all -> 401 because of require_scope.
    let err = tasks
        .count(&Query::new(), &Context::new())
        .await
        .unwrap_err();
    assert_eq!(err.message(), "Missing scope: tenant_id");

    // Jobs: Context::system() sees every row on purpose (and is easy to grep for).
    let all = tasks
        .find(Query::new().select(["tenant_id"]), &Context::system())
        .await?;
    assert_eq!(all.count, 3);
    // Per-tenant job: run each tenant's work inside its own scope.
    let mut tenants: Vec<String> = all
        .data
        .iter()
        .filter_map(|r| r["tenant_id"].as_str().map(str::to_string))
        .collect();
    tenants.sort();
    tenants.dedup();
    for tenant in tenants {
        let ctx = Context::new().with_scope([("tenant_id", tenant.as_str())]);
        let n = tasks.count(&Query::new(), &ctx).await?;
        tasks
            .insert(
                row([("title", format!("digest of {n} tasks").into())]),
                &ctx,
            )
            .await?;
    }
    assert_eq!(tasks.count(&Query::new(), &a).await?, 3);
    println!("8. scope in code: ok");
    Ok(())
}

// ------------------------------------------------------------------ 9. HTTP

/// Auth: verify the session/token (stubbed as headers here), then hand the model layer
/// two things: `CurrentUser` (hooks read it as ctx.user) and `Scope` (row permissions).
async fn auth(mut req: Request, next: Next) -> std::result::Result<Response, StatusCode> {
    let h = req.headers();
    let header = |k| Some(h.get(k)?.to_str().ok()?.to_string());
    let (user, tenant, role) = (header("x-user"), header("x-tenant"), header("x-role"));
    let (Some(user), Some(tenant)) = (user, tenant) else {
        return Err(StatusCode::UNAUTHORIZED);
    };
    let role = role.unwrap_or_else(|| "viewer".into());
    req.extensions_mut().insert(CurrentUser(
        json!({"id": user, "tenant_id": tenant, "role": role}),
    ));
    // Admins see every tenant: simply set no Scope (and no require_scope on the model).
    req.extensions_mut()
        .insert(Scope::new([("tenant_id", json!(tenant))]));
    Ok(next.run(req).await)
}

/// Route-level permission: viewers may only read.
async fn writers_only(req: Request, next: Next) -> Response {
    let role = req
        .extensions()
        .get::<CurrentUser>()
        .and_then(|u| u.0["role"].as_str().map(str::to_string));
    let read = matches!(req.method().as_str(), "GET" | "HEAD" | "QUERY");
    if read || role.as_deref() == Some("editor") {
        next.run(req).await
    } else {
        (
            StatusCode::FORBIDDEN,
            axum::Json(json!({"message": "Forbidden", "type": "danger"})),
        )
            .into_response()
    }
}

/// Nested route: add the parent id from the path to the tenant scope.
async fn project_scope(
    Path(p): Path<HashMap<String, String>>,
    mut req: Request,
    next: Next,
) -> Response {
    let mut scope = req.extensions().get::<Scope>().cloned().unwrap_or_default();
    scope.0.insert("project_id".into(), json!(p["project_id"]));
    req.extensions_mut().insert(scope);
    next.run(req).await
}

async fn http() -> Result<()> {
    let db = db(&[
        "CREATE TABLE projects (id INTEGER PRIMARY KEY AUTOINCREMENT,
            tenant_id TEXT NOT NULL, name TEXT NOT NULL,
            version INTEGER NOT NULL DEFAULT 1)",
        "CREATE TABLE issues (id INTEGER PRIMARY KEY AUTOINCREMENT,
            tenant_id TEXT NOT NULL, project_id INTEGER NOT NULL,
            title TEXT NOT NULL, priority INTEGER NOT NULL DEFAULT 3)",
    ])
    .await;
    let projects = Model::builder(db.clone(), "projects")
        .column("id", "integer")
        .column("tenant_id", "required|string")
        .column("name", "required|string")
        .column("version", "integer")
        .version_column("version")
        .search_columns(["name"])
        .require_scope(["tenant_id"])
        .build()?;
    let issues = Model::builder(db.clone(), "issues")
        .column("id", "integer")
        .column("tenant_id", "required|string")
        .column("project_id", "required|integer")
        .column("title", "required|string")
        .column("priority", "integer")
        .require_scope(["tenant_id"])
        .build()?;
    let issues = issues.belongs_to("project", &projects, "project_id");
    let projects = projects.has_many("issues", &issues, "project_id");

    let spec = openapi(
        "Tracker",
        "1.0.0",
        &[
            ("/projects", &projects, &RouterOptions::default()),
            ("/reports/issues", &issues, &RouterOptions::read_only()),
        ],
    );
    let api = Router::new()
        .nest("/projects", create_model_router(projects.clone()))
        .nest(
            "/projects/{project_id}/issues",
            create_model_router(issues.clone()).route_layer(middleware::from_fn(project_scope)),
        )
        // Read-only view: writes answer 405.
        .nest(
            "/reports/issues",
            create_model_router_with(issues.clone(), RouterOptions::read_only()),
        )
        // Custom subset: no deletes at all, no CSV/XML export.
        .nest(
            "/archive/projects",
            create_model_router_with(
                projects.clone(),
                RouterOptions {
                    delete: false,
                    bulk_delete: false,
                    export: false,
                    ..RouterOptions::default()
                },
            ),
        )
        .layer(middleware::from_fn(writers_only))
        .layer(middleware::from_fn(auth)); // last layer runs first
    let app = Router::new()
        .merge(api)
        .nest("/health", health_router(db))
        .nest("/openapi.json", openapi_route(spec));

    let ed = [("x-user", "u1"), ("x-tenant", "acme"), ("x-role", "editor")];
    let viewer = [("x-user", "u2"), ("x-tenant", "acme")];
    let other = [
        ("x-user", "u3"),
        ("x-tenant", "globex"),
        ("x-role", "editor"),
    ];

    // Single create: POST / with one object. tenant_id comes from the scope.
    let (s, _, t) = call(
        &app,
        "POST",
        "/projects",
        &ed,
        Some(json!({"name": "Apollo"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert_eq!(js(&t)["tenant_id"], "acme");
    // Bulk create: POST / with {"data": [...]} returns a summary.
    let (_, _, t) = call(
        &app,
        "POST",
        "/projects",
        &ed,
        Some(json!({"data": [{"name": "Gemini"}, {"name": "Mercury"}]})),
    )
    .await;
    assert_eq!(js(&t)["rows"], 2);
    call(
        &app,
        "POST",
        "/projects",
        &other,
        Some(json!({"name": "Rival"})),
    )
    .await;

    // Role check from the permission middleware.
    let (s, _, _) = call(
        &app,
        "POST",
        "/projects",
        &viewer,
        Some(json!({"name": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // No credentials at all.
    let (s, _, _) = call(&app, "GET", "/projects", &[], None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // Tenancy: acme sees 3 projects, globex 1; globex cannot read acme's row 1.
    let (_, _, t) = call(&app, "GET", "/projects", &viewer, None).await;
    assert_eq!(js(&t)["count"], 3);
    let (s, _, _) = call(&app, "GET", "/projects/1", &other, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Filters (URL-encoded prefixes), sort, paging, search, select.
    let (_, _, t) = call(
        &app,
        "GET",
        "/projects?name=%25e%25&sort=-name&page=0&size=1",
        &viewer,
        None,
    )
    .await;
    let v = js(&t);
    assert_eq!(
        (v["count"].clone(), v["data"][0]["name"].clone()),
        (json!(2), json!("Mercury"))
    );
    let (_, _, t) = call(
        &app,
        "GET",
        "/projects?id=%3E%3D2&select_columns=id,name",
        &viewer,
        None,
    )
    .await;
    assert_eq!(js(&t)["count"], 2);
    let (_, _, t) = call(&app, "GET", "/projects?id=in(1,3)", &viewer, None).await;
    assert_eq!(js(&t)["count"], 2);
    let (_, _, t) = call(&app, "GET", "/projects?search=gem", &viewer, None).await;
    assert_eq!(js(&t)["data"][0]["name"], "Gemini");
    // QUERY method with a JSON body: OR of AND-groups.
    let (_, _, t) = call(
        &app,
        "QUERY",
        "/projects",
        &viewer,
        Some(json!({"filter": [[["name", "=", "Apollo"]], [["name", "=", "Gemini"]]]})),
    )
    .await;
    assert_eq!(js(&t)["count"], 2);

    // Nested route: project_id and tenant_id both come from the scope.
    let (s, _, t) = call(
        &app,
        "POST",
        "/projects/1/issues",
        &ed,
        Some(json!({"title": "Fuel leak"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert_eq!(js(&t)["project_id"], 1);
    // Another tenant cannot attach an issue to acme's project: 422 Invalid reference.
    let (s, _, t) = call(
        &app,
        "POST",
        "/projects/1/issues",
        &other,
        Some(json!({"title": "x"})),
    )
    .await;
    assert_eq!(
        (s, js(&t)["message"].clone()),
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            json!("Invalid reference: project_id")
        )
    );
    let (_, _, t) = call(&app, "GET", "/projects/2/issues", &viewer, None).await;
    assert_eq!(js(&t)["count"], 0);
    // include= follows relations, inside the same scope.
    let (_, _, t) = call(&app, "GET", "/projects/1?include=issues", &viewer, None).await;
    assert_eq!(js(&t)["issues"][0]["title"], "Fuel leak");

    // Optimistic locking over HTTP: ETag on read, If-Match on write.
    let (_, h, _) = call(&app, "GET", "/projects/1", &viewer, None).await;
    let etag = h["etag"].to_str().unwrap().to_string();
    assert_eq!(etag, "\"1\"");
    let (s, _, _) = call(
        &app,
        "PATCH",
        "/projects/1",
        &ed,
        Some(json!({"name": "Apollo 11"})),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);
    let mut with_etag = ed.to_vec();
    with_etag.push(("if-match", &etag));
    let (s, _, _) = call(
        &app,
        "PATCH",
        "/projects/1",
        &with_etag,
        Some(json!({"name": "Apollo 11"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = call(
        &app,
        "PATCH",
        "/projects/1",
        &with_etag,
        Some(json!({"name": "late"})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Export: CSV or XML instead of JSON.
    let (_, h, t) = call(
        &app,
        "GET",
        "/projects?output_content_type=csv&select_columns=id,name",
        &viewer,
        None,
    )
    .await;
    assert!(h["content-type"].to_str().unwrap().starts_with("text/csv"));
    assert!(t.starts_with("id,name\n1,Apollo 11"), "{t}");

    // Read-only and custom routers answer 405 for disabled operations.
    let (s, _, _) = call(
        &app,
        "POST",
        "/reports/issues",
        &ed,
        Some(json!({"title": "x"})),
    )
    .await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
    let (s, _, _) = call(&app, "DELETE", "/archive/projects/2", &ed, None).await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);

    // Bulk delete needs criteria; a scope alone never allows a mass delete.
    let (s, _, _) = call(&app, "DELETE", "/projects", &ed, Some(json!({}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (_, _, t) = call(
        &app,
        "DELETE",
        "/projects",
        &ed,
        Some(json!({"name": "Mercury"})),
    )
    .await;
    assert_eq!(js(&t)["rows"], 1);

    // Health and OpenAPI.
    let (s, _, t) = call(&app, "GET", "/health", &[], None).await;
    assert_eq!((s, js(&t)["status"].clone()), (StatusCode::OK, json!("ok")));
    let (_, _, t) = call(&app, "GET", "/openapi.json", &[], None).await;
    assert!(js(&t)["paths"].get("/projects").is_some());
    println!("9. http: ok");
    Ok(())
}

// ------------------------------------------------------------------ 10. schema file

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

async fn schema_file() -> Result<()> {
    let db = db(&[
        "CREATE TABLE products (id INTEGER PRIMARY KEY AUTOINCREMENT,
        title TEXT NOT NULL, price REAL NOT NULL)",
    ])
    .await;
    let mut models = dbmr::load_models_from_schema(&db, SCHEMA, None)?;
    let products = models.shift_remove("products").unwrap();
    let app = Router::new().nest("/products", create_model_router(products));
    let (s, _, t) = call(
        &app,
        "POST",
        "/products",
        &[],
        Some(json!({"title": "Pen", "price": 1.5})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    println!("10. schema file: ok");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    crud_and_queries().await?;
    soft_delete().await?;
    hooks_audit_hidden().await?;
    optimistic_locking().await?;
    relations_and_typed().await?;
    transactions().await?;
    encryption_and_events().await?;
    scope_in_code().await?;
    http().await?;
    schema_file().await?;
    println!("tour complete");
    Ok(())
}
