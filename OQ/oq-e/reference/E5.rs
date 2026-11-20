use axum::Router;
use dbmr::adapters::SqliteAdapter;
use dbmr::{create_model_router, DatabaseAdapter, Model};
use serde_json::json;
use std::sync::Arc;

pub async fn build_app(db: Arc<SqliteAdapter>) -> dbmr::Result<Router> {
    db.query("CREATE TABLE IF NOT EXISTS tasks (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, status TEXT NOT NULL, is_deleted INTEGER NOT NULL DEFAULT 0)", &[]).await?;
    let tasks = Model::builder(db, "tasks")
        .column("id", "integer")
        .column("title", "required|string")
        .column("status", "required|in:open,done")
        .safe_delete("is_deleted")
        .search_columns(["title"])
        .build()?;
    Ok(Router::new().nest("/tasks", create_model_router(tasks)))
}
