//! Application bootstrap and lifecycle coordinator for the Blog Application.
//!
//! Connects to the database, executes migrations before the application starts,
//! initializes domain models, and creates the Axum HTTP router.

use axum::Router;
use dbmr::adapters::SqliteAdapter;
use dbmr::DatabaseAdapter;
use std::sync::Arc;

use super::migration;
use super::model::{self, BlogModels};
use super::router;

/// Bootstrap the application stack:
/// 1. Connect to SQLite database
/// 2. Run all pending migrations from the `migrations/` folder
/// 3. Build domain models with validation rules, hooks, and relations
/// 4. Build Axum REST router
pub async fn create_app() -> dbmr::Result<(Router, Arc<dyn DatabaseAdapter>, BlogModels)> {
    create_app_with_db("sqlite::memory:").await
}

/// Bootstrap the application stack against a specific database connection string.
pub async fn create_app_with_db(
    db_url: &str,
) -> dbmr::Result<(Router, Arc<dyn DatabaseAdapter>, BlogModels)> {
    // 1. Initialize Database adapter
    let db: Arc<dyn DatabaseAdapter> = SqliteAdapter::connect(db_url).await?;

    // 2. Run migrations from the migrations folder before the app starts
    migration::run_migrations(&db).await?;

    // 3. Build domain models
    let models = model::build_all(&db)?;

    // 4. Build Axum HTTP router
    let app = router::build_router(&models)?;

    Ok((app, db, models))
}

/// Run the HTTP server on the specified address.
pub async fn run_server(addr: &str) -> Result<(), Box<dyn std::error::Error>> {
    let (app, _db, _models) = create_app().await?;
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("🚀 Blog App HTTP Server listening on http://{addr}");
    println!("   API Endpoints:");
    println!("   - Authors:   http://{addr}/authors");
    println!("   - Posts:     http://{addr}/posts");
    println!("   - Comments:  http://{addr}/comments");
    println!("   - Likes:     http://{addr}/likes");
    println!("   - Shares:    http://{addr}/shares");
    println!("   - Nested:    http://{addr}/posts/:id/comments");
    println!("   - OpenAPI:   http://{addr}/openapi.json");
    println!("   - Health:    http://{addr}/health");

    axum::serve(listener, app).await?;
    Ok(())
}
