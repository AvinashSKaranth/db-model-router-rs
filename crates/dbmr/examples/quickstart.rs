//! Minimal dbmr service: one model, hooks, a tenant-scoped REST router.
//!
//! Smoke run (no server):  cargo run -p dbmr --example quickstart
//! Serve on :3000:         set DBMR_DEMO_TOKEN_A and DBMR_DEMO_TOKEN_B, then pass --serve
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::extract::Request;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use dbmr::adapters::SqliteAdapter;
use dbmr::http::{create_model_router_with, RouterOptions};
use dbmr::{AppError, CurrentUser, DbValue, Model, ModelHooks, Scope};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;

/// The demo maps server-configured bearer credentials to tenant membership.
type DemoTokens = Arc<[String; 2]>;

async fn auth(
    State(tokens): State<DemoTokens>,
    mut req: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let tenant = tokens
        .iter()
        .position(|expected| expected == token)
        .map(|i| ["a", "b"][i])
        .ok_or(StatusCode::UNAUTHORIZED)?;
    req.extensions_mut()
        .insert(CurrentUser(json!({ "tenant_id": tenant })));
    req.extensions_mut()
        .insert(Scope::new([("tenant_id", json!(tenant))]));
    Ok(next.run(req).await)
}

#[tokio::main]
async fn main() -> dbmr::Result<()> {
    let serve = std::env::args().any(|a| a == "--serve");
    let tokens = if serve {
        let token = |name| {
            std::env::var(name)
                .map_err(|_| AppError::Validation(format!("Set {name} to a secret bearer token.")))
        };
        let a = token("DBMR_DEMO_TOKEN_A")?;
        let b = token("DBMR_DEMO_TOKEN_B")?;
        if a.len() < 32 || b.len() < 32 || a == b {
            return Err(AppError::Validation(
                "Use distinct demo tokens of at least 32 characters.".into(),
            ));
        }
        Arc::new([a, b])
    } else {
        Arc::new(["fixture-a".into(), "fixture-b".into()])
    };
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
    dbmr::DatabaseAdapter::query(
        db.as_ref(),
        "CREATE INDEX notes_tenant_visible ON notes (tenant_id, is_deleted, id)",
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
    let options = RouterOptions {
        create: true,
        export: false,
        ..RouterOptions::read_only()
    };
    let app = Router::new()
        .nest("/notes", create_model_router_with(notes, options))
        .layer(middleware::from_fn_with_state(tokens.clone(), auth));

    if serve {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
            .await
            .expect("bind :3000");
        axum::serve(listener, app).await.expect("server");
        return Ok(());
    }

    // Smoke run: create as tenant A, then list as A and as B.
    let send = |method: &str, uri: &str, token: &str, body: Option<serde_json::Value>| {
        Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
            .unwrap()
    };
    let res = app
        .clone()
        .oneshot(send(
            "POST",
            "/notes",
            &tokens[0],
            Some(json!({ "title": "  hello  " })),
        ))
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    for (i, expected) in [1, 0].into_iter().enumerate() {
        let res = app
            .clone()
            .oneshot(send("GET", "/notes", &tokens[i], None))
            .await
            .unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let page: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(page["count"], expected, "tenant {i}: {page}");
        println!("tenant {i}: {page}");
    }
    Ok(())
}
