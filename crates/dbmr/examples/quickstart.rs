//! Minimal dbmr service: one model, hooks, a tenant-scoped REST router.
//!
//! Smoke run (no server):  cargo run -p dbmr --example quickstart
//! Serve on :3000:         cargo run -p dbmr --example quickstart -- --serve
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use dbmr::adapters::SqliteAdapter;
use dbmr::{create_model_router, CurrentUser, DbValue, Model, ModelHooks, Scope};
use serde_json::json;
use tower::ServiceExt;

/// Auth stub: real apps verify a session or token here. `CurrentUser` reaches hooks as
/// `ctx.user`; `Scope` limits every read and write of the request to the tenant's rows.
async fn auth(mut req: Request, next: Next) -> Result<Response, StatusCode> {
    let tenant = req
        .headers()
        .get("x-tenant")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?
        .to_string();
    req.extensions_mut()
        .insert(CurrentUser(json!({ "tenant_id": tenant })));
    req.extensions_mut()
        .insert(Scope::new([("tenant_id", json!(tenant))]));
    Ok(next.run(req).await)
}

#[tokio::main]
async fn main() -> dbmr::Result<()> {
    // 1. Adapter. Applications own DDL; dbmr never creates tables.
    let db = SqliteAdapter::connect("sqlite::memory:").await?;
    dbmr::DatabaseAdapter::query(
        db.as_ref(),
        "CREATE TABLE notes (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            tenant_id TEXT NOT NULL,
            title TEXT NOT NULL,
            body TEXT,
            is_deleted INTEGER NOT NULL DEFAULT 0
        )",
        &[],
    )
    .await?;

    // 2. Model. Unknown rules fail here, not at request time.
    let notes = Model::builder(db, "notes")
        .column("id", "integer")
        .column("tenant_id", "required|string")
        .column("title", "required|string|maxLength:100")
        .column("body", "string|nullable")
        .safe_delete("is_deleted")
        .search_columns(["title", "body"])
        // Fail closed: a request or job without a tenant scope is 401.
        .require_scope(["tenant_id"])
        .hooks(ModelHooks::new().before_save(|mut row, _ctx| async move {
            if let Some(DbValue::Text(t)) = row.get("title") {
                let trimmed = t.trim().to_string();
                row.insert("title".into(), trimmed.into());
            }
            Ok(row)
        }))
        .build()?;

    // 3. Router. The model applies the request's Scope to every read filter and write,
    //    so clients cannot cross tenants whatever they send.
    let app = Router::new()
        .nest("/notes", create_model_router(notes))
        .layer(middleware::from_fn(auth));

    if std::env::args().any(|a| a == "--serve") {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
            .await
            .expect("bind :3000");
        axum::serve(listener, app).await.expect("server");
        return Ok(());
    }

    // Smoke run: create as tenant A, then list as A and as B.
    let send = |method: &str, uri: &str, tenant: &str, body: Option<serde_json::Value>| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("x-tenant", tenant)
            .header("content-type", "application/json")
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap()
    };
    let res = app
        .clone()
        .oneshot(send(
            "POST",
            "/notes",
            "a",
            Some(json!({ "title": "  hello  " })),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    for (tenant, expected) in [("a", 1), ("b", 0)] {
        let res = app
            .clone()
            .oneshot(send("GET", "/notes", tenant, None))
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let page: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(page["count"], expected, "tenant {tenant}: {page}");
        println!("tenant {tenant}: {page}");
    }
    Ok(())
}
