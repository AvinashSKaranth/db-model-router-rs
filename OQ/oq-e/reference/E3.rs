use axum::extract::{Path, Request};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use dbmr::adapters::SqliteAdapter;
use dbmr::{create_model_router, DatabaseAdapter, Model, Scope};
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;

/// Books under /authors/{author_id}/books belong to that author.
async fn author_scope(Path(p): Path<HashMap<String, String>>, mut req: Request, next: Next) -> Response {
    req.extensions_mut().insert(Scope::new([("author_id", json!(p["author_id"]))]));
    next.run(req).await
}

fn books(db: Arc<SqliteAdapter>) -> dbmr::Result<Model> {
    Model::builder(db, "books")
        .column("id", "integer")
        .column("author_id", "required|integer")
        .column("title", "required|string")
        .build()
}

pub async fn build_app(db: Arc<SqliteAdapter>) -> dbmr::Result<Router> {
    db.query("CREATE TABLE IF NOT EXISTS authors (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)", &[]).await?;
    db.query("CREATE TABLE IF NOT EXISTS books (id INTEGER PRIMARY KEY AUTOINCREMENT, author_id INTEGER NOT NULL, title TEXT NOT NULL)", &[]).await?;
    let authors = Model::builder(db.clone(), "authors").column("id", "integer").column("name", "required|string").build()?;
    Ok(Router::new()
        .nest("/authors/{author_id}/books", create_model_router(books(db.clone())?).route_layer(middleware::from_fn(author_scope)))
        .nest("/authors", create_model_router(authors))
        .nest("/books", create_model_router(books(db)?)))
}
