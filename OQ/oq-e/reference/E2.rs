use axum::extract::Request;
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use dbmr::adapters::SqliteAdapter;
use dbmr::{create_model_router, CurrentUser, DatabaseAdapter, Model, Scope};
use serde_json::json;
use std::sync::Arc;

async fn auth(mut req: Request, next: Next) -> Result<Response, StatusCode> {
    let t = req.headers().get("x-tenant").and_then(|v| v.to_str().ok()).ok_or(StatusCode::UNAUTHORIZED)?.to_string();
    req.extensions_mut().insert(CurrentUser(json!({ "tenant_id": t })));
    req.extensions_mut().insert(Scope::new([("tenant_id", json!(t))]));
    Ok(next.run(req).await)
}

pub async fn build_app(db: Arc<SqliteAdapter>) -> dbmr::Result<Router> {
    db.query("CREATE TABLE IF NOT EXISTS notes (id INTEGER PRIMARY KEY AUTOINCREMENT, tenant_id TEXT NOT NULL, title TEXT NOT NULL, body TEXT)", &[]).await?;
    let notes = Model::builder(db, "notes")
        .column("id", "integer")
        .column("tenant_id", "required|string")
        .column("title", "required|string")
        .column("body", "string|nullable")
        .require_scope(["tenant_id"])
        .build()?;
    Ok(Router::new()
        .nest("/notes", create_model_router(notes))
        .layer(middleware::from_fn(auth)))
}
