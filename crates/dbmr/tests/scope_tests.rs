//! Row scope (`Scope` request extension → `Context::scope`): every read, write and delete
//! stays inside it, whatever the body shape. Includes the adversarial cases that the old
//! override-rule design let through. SQLite in-memory.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::extract::{Path, Request};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use dbmr::adapters::SqliteAdapter;
use dbmr::core::types::json_to_row;
use dbmr::core::{Context, Model, ModelBuilder, Query, Scope};
use dbmr::http::create_model_router;
use serde_json::{json, Value};
use sqlx::sqlite::SqlitePoolOptions;
use std::collections::HashMap;
use std::sync::Arc;
use tower::ServiceExt;

async fn db() -> Arc<SqliteAdapter> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    for ddl in [
        "CREATE TABLE notes (id INTEGER PRIMARY KEY AUTOINCREMENT, tenant_id TEXT NOT NULL,
         title TEXT, code TEXT UNIQUE)",
        "CREATE TABLE books (id INTEGER PRIMARY KEY AUTOINCREMENT, author_id INTEGER NOT NULL,
         title TEXT)",
    ] {
        sqlx::query(ddl).execute(&pool).await.unwrap();
    }
    Arc::new(SqliteAdapter::new(pool))
}

fn notes(db: Arc<SqliteAdapter>) -> ModelBuilder {
    Model::builder(db, "notes")
        .column("id", "integer")
        .column("tenant_id", "required|string")
        .column("title", "string|nullable")
        .column("code", "string|nullable")
        .unique(["code"])
}

/// Test auth: `x-tenant: a` scopes to one tenant, `x-tenant: a,b` to several. A real app
/// takes this from its verified session.
async fn auth(mut req: Request, next: Next) -> Response {
    let tenants: Option<Vec<String>> = req
        .headers()
        .get("x-tenant")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.split(',').map(str::to_string).collect());
    match tenants {
        Some(t) if t.len() == 1 => {
            req.extensions_mut()
                .insert(Scope::new([("tenant_id", json!(t[0]))]));
        }
        Some(t) => {
            req.extensions_mut()
                .insert(Scope::new([("tenant_id", json!(t))]));
        }
        None => {}
    }
    next.run(req).await
}

fn app(m: Model) -> Router {
    Router::new()
        .nest("/notes", create_model_router(m))
        .layer(middleware::from_fn(auth))
}

async fn call(app: &Router, method: &str, uri: &str, tenant: &str, body: Value) -> (u16, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");
    if !tenant.is_empty() {
        b = b.header("x-tenant", tenant);
    }
    let res = app
        .clone()
        .oneshot(b.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// Tenant a owns note 1, tenant b owns note 2 (`b-secret`).
async fn seeded() -> (Model, Router) {
    let m = notes(db().await).build().unwrap();
    let ctx = Context::new();
    for (t, title, code) in [("a", "a-note", "A1"), ("b", "b-secret", "B1")] {
        m.insert(
            json_to_row(&json!({"tenant_id": t, "title": title, "code": code})),
            &ctx,
        )
        .await
        .unwrap();
    }
    let app = app(m.clone());
    (m, app)
}

/// Tenant b's view of its own rows.
async fn b_rows(m: &Model) -> Vec<Value> {
    let ctx = Context::new().with_scope([("tenant_id", "b")]);
    let r = m.list(Query::new(), &ctx).await.unwrap();
    r.data.iter().map(dbmr::core::row_to_json).collect()
}

#[tokio::test]
async fn reads_stay_inside_the_scope_whatever_the_filter_shape() {
    let (_, app) = seeded().await;
    for body in [
        json!({"data": [{"id": 2}]}),
        json!({"filter": [[["id", "=", 2]]]}),
        json!({"id": 2}),
        json!({"tenant_id": "b"}),
        json!({"filter": [[["tenant_id", "=", "b"]], [["id", ">", 0]]]}),
    ] {
        let (st, v) = call(&app, "QUERY", "/notes", "a", body.clone()).await;
        assert_eq!(st, 200, "{body}: {v}");
        assert!(
            v["data"]
                .as_array()
                .unwrap()
                .iter()
                .all(|r| r["tenant_id"] == "a"),
            "{body} leaked: {v}"
        );
    }
    let (st, _) = call(&app, "GET", "/notes/2", "a", json!({})).await;
    assert_eq!(st, 404);
    let (_, v) = call(&app, "GET", "/notes?tenant_id=b", "a", json!({})).await;
    assert_eq!(v["count"], 0);
}

#[tokio::test]
async fn deletes_stay_inside_the_scope() {
    let (m, app) = seeded().await;
    for body in [json!({"data": [{"id": 2}]}), json!({"id": 2})] {
        let (st, v) = call(&app, "DELETE", "/notes", "a", body.clone()).await;
        assert_eq!((st, &v["rows"]), (200, &json!(0)), "{body}: {v}");
    }
    let (st, _) = call(&app, "DELETE", "/notes/2", "a", json!({})).await;
    assert_eq!(st, 404);
    let (st, _) = call(
        &app,
        "DELETE",
        "/notes/1",
        "a",
        json!({"data": [{"id": 2}]}),
    )
    .await;
    assert_eq!(st, 400, "item routes refuse bulk bodies");
    assert_eq!(b_rows(&m).await.len(), 1);
}

#[tokio::test]
async fn updates_cannot_reach_or_move_rows_across_scopes() {
    let (m, app) = seeded().await;
    // Bulk PUT naming b's row: 404, nothing changed.
    let (st, v) = call(
        &app,
        "PUT",
        "/notes",
        "a",
        json!({"data": [{"id": 2, "title": "overwritten"}]}),
    )
    .await;
    assert_eq!(
        (st, &v["message"]),
        (404, &json!("data.0: Not Found")),
        "{v}"
    );
    for (method, body) in [
        ("PUT", json!({"title": "x"})),
        ("PATCH", json!({"title": "x"})),
    ] {
        let (st, _) = call(&app, method, "/notes/2", "a", body).await;
        assert_eq!(st, 404, "{method}");
    }
    let rows = b_rows(&m).await;
    assert_eq!((rows.len(), &rows[0]["title"]), (1, &json!("b-secret")));

    // Own row: the scope column is forced, so a row cannot be moved to another tenant.
    for (method, uri, body) in [
        ("PUT", "/notes/1", json!({"title": "t", "tenant_id": "b"})),
        ("PATCH", "/notes/1", json!({"tenant_id": "b"})),
        (
            "PUT",
            "/notes",
            json!({"data": [{"id": 1, "title": "t", "tenant_id": "b"}]}),
        ),
    ] {
        let (st, v) = call(&app, method, uri, "a", body).await;
        assert_eq!(st, 200, "{method} {uri}: {v}");
    }
    let (_, v) = call(&app, "GET", "/notes/1", "a", json!({})).await;
    assert_eq!(v["tenant_id"], "a");
    assert_eq!(b_rows(&m).await.len(), 1);

    // Upsert on a unique key that belongs to another tenant: 404, b's row untouched.
    let ctx = Context::new().with_scope([("tenant_id", "a")]);
    let e = m
        .upsert(json_to_row(&json!({"code": "B1", "title": "stolen"})), &ctx)
        .await
        .unwrap_err();
    assert_eq!(e.status_code(), 404);
    assert_eq!(b_rows(&m).await[0]["title"], "b-secret");
}

#[tokio::test]
async fn inserts_get_the_scope_value() {
    let (m, app) = seeded().await;
    let (st, v) = call(
        &app,
        "POST",
        "/notes",
        "a",
        json!({"data": [{"tenant_id": "b", "title": "injected"}]}),
    )
    .await;
    assert_eq!(st, 200, "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/notes",
        "a",
        json!({"tenant_id": "b", "title": "x"}),
    )
    .await;
    assert_eq!((st, &v["tenant_id"]), (200, &json!("a")), "{v}");
    let (st, v) = call(
        &app,
        "POST",
        "/notes/0",
        "a",
        json!({"title": "no tenant sent"}),
    )
    .await;
    assert_eq!((st, &v["tenant_id"]), (200, &json!("a")), "{v}");
    assert_eq!(b_rows(&m).await.len(), 1, "nothing landed in b");
}

#[tokio::test]
async fn a_list_scope_reads_all_and_writes_one_of_them() {
    let (_, app) = seeded().await;
    let (_, v) = call(&app, "GET", "/notes?sort=id", "a,b", json!({})).await;
    assert_eq!(v["count"], 2);
    let (_, v) = call(&app, "GET", "/notes", "a,c", json!({})).await;
    assert_eq!(v["count"], 1);
    for body in [
        json!({"title": "none"}),
        json!({"title": "c", "tenant_id": "c"}),
    ] {
        let (st, v) = call(&app, "POST", "/notes", "a,b", body.clone()).await;
        assert_eq!(
            (st, &v["message"]),
            (422, &json!("The tenant_id is outside the scope.")),
            "{body}"
        );
    }
    let (st, v) = call(
        &app,
        "POST",
        "/notes",
        "a,b",
        json!({"title": "ok", "tenant_id": "b"}),
    )
    .await;
    assert_eq!((st, &v["tenant_id"]), (200, &json!("b")), "{v}");
    let (st, _) = call(&app, "PATCH", "/notes/2", "a,b", json!({"tenant_id": "c"})).await;
    assert_eq!(st, 422);
    let (st, _) = call(&app, "PATCH", "/notes/2", "a,b", json!({"title": "both"})).await;
    assert_eq!(st, 200);
}

#[tokio::test]
async fn require_scope_fails_closed_and_system_opts_out() {
    let m = notes(db().await)
        .require_scope(["tenant_id"])
        .build()
        .unwrap();
    let app = app(m.clone());
    let (st, v) = call(&app, "GET", "/notes", "", json!({})).await;
    assert_eq!(
        (st, v),
        (
            401,
            json!({"message": "Missing scope: tenant_id", "type": "danger"})
        )
    );
    let (st, _) = call(&app, "POST", "/notes", "", json!({"title": "x"})).await;
    assert_eq!(st, 401);

    // Undeclared require_scope column fails at build time
    let res = notes(db().await)
        .require_scope(["non_existent_tenant_column"])
        .build();
    assert!(res.is_err());
    let err = res.err().unwrap();
    assert_eq!(err.status_code(), 422);
    assert!(err.message().contains("not declared in model structure"));

    // Jobs: an unscoped Context is refused; Context::system() is the explicit opt-out.
    let row = json_to_row(&json!({"tenant_id": "a", "title": "job"}));
    let e = m.insert(row.clone(), &Context::new()).await.unwrap_err();
    assert_eq!(
        (e.status_code(), e.message()),
        (401, "Missing scope: tenant_id")
    );
    m.insert(row, &Context::system()).await.unwrap();
    let all = m.list(Query::new(), &Context::system()).await.unwrap();
    assert_eq!(all.count, 1);
    // A null scope value is never "no restriction".
    let e = m
        .list(
            Query::new(),
            &Context::system().with_scope([("tenant_id", Value::Null)]),
        )
        .await
        .unwrap_err();
    assert_eq!(e.status_code(), 401);
    let (_, v) = call(&app, "GET", "/notes", "a", json!({})).await;
    assert_eq!(v["count"], 1);
}

/// Nested child route: a `route_layer` middleware turns the path parameter into the scope.
async fn author_scope(
    Path(p): Path<HashMap<String, String>>,
    mut req: Request,
    next: Next,
) -> Response {
    req.extensions_mut()
        .insert(Scope::new([("author_id", json!(p["author_id"]))]));
    next.run(req).await
}

#[tokio::test]
async fn nested_routes_scope_by_path_parameter() {
    let books = Model::builder(db().await, "books")
        .column("id", "integer")
        .column("author_id", "required|integer")
        .column("title", "required|string")
        .build()
        .unwrap();
    let app = Router::new().nest(
        "/authors/{author_id}/books",
        create_model_router(books).route_layer(middleware::from_fn(author_scope)),
    );
    let (st, v) = call(&app, "POST", "/authors/1/books", "", json!({"title": "t1"})).await;
    assert_eq!((st, &v["author_id"]), (200, &json!(1)), "{v}");
    let (_, v) = call(&app, "GET", "/authors/1/books", "", json!({})).await;
    assert_eq!(v["count"], 1);
    let (_, v) = call(&app, "GET", "/authors/2/books", "", json!({})).await;
    assert_eq!(v["count"], 0);
    for m in ["GET", "PUT", "PATCH", "DELETE"] {
        let (st, _) = call(&app, m, "/authors/2/books/1", "", json!({"title": "x"})).await;
        assert_eq!(st, 404, "{m}");
    }
}
