use axum::Router;
use dbmr::adapters::SqliteAdapter;
use dbmr::{create_model_router, DatabaseAdapter, Model};
use serde_json::json;
use std::sync::Arc;

pub async fn build_app(db: Arc<SqliteAdapter>) -> dbmr::Result<Router> {
    db.query(
        "CREATE TABLE IF NOT EXISTS books (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL,
         author TEXT NOT NULL, year INTEGER, isbn TEXT NOT NULL UNIQUE)",
        &[],
    )
    .await?;
    let books = Model::builder(db, "books")
        .column("id", "integer")
        .column("title", "required|string|maxLength:200")
        .column("author", "required|string")
        .column("year", "integer")
        .column("isbn", "required|string")
        .unique(["isbn"])
        .build()?;
    Ok(Router::new().nest("/books", create_model_router(books)))
}
