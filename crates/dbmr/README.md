# db-model-router (`dbmr`)

Database-agnostic **models** and generated **Axum REST routers** for Rust services.
Rewrite of the Node.js `db-model-router` with high-performance async Rust and complete feature parity.

[![Crates.io](https://img.shields.io/crates/v/db-model-router.svg)](https://crates.io/crates/db-model-router)
[![Docs.rs](https://docs.rs/db-model-router/badge.svg)](https://docs.rs/db-model-router)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Features

- **10 Database Adapters**: SQLite, PostgreSQL, CockroachDB, MySQL, MariaDB, MSSQL, Oracle, MongoDB, Redis, DynamoDB.
- **Declarative Models**: Type validation, unique constraints, soft delete, auto timestamps, optimistic locking (ETag/If-Match), field encryption (AES-256-GCM).
- **Row-Level Scopes**: Security first. Enforces tenant, owner, or parent permissions on every query and mutation.
- **Generated Axum REST Routers**: Automatic list paging, filtering, searching, sorting, single & bulk CRUD, and OpenAPI generation.
- **Multi-Entity Relations**: `has_many` and `belongs_to` with eager loading via `?include=...`.
- **Change Events**: Optional CDC stream to Kafka topics.

## Installation

Add to your `Cargo.toml`:

```toml
[dependencies]
db-model-router = "0.0.1"        # features: sqlite (default), postgres, mysql, etc.
axum = "0.8"
tokio = { version = "1", features = ["full"] }
serde_json = "1"
```

In your Rust code, import via `dbmr`:

```rust
use dbmr::adapters::SqliteAdapter;
use dbmr::{create_model_router, Context, Model, Scope};
```

## Quick Example

```rust
use axum::Router;
use dbmr::adapters::SqliteAdapter;
use dbmr::{create_model_router, Model};

#[tokio::main]
async fn main() -> dbmr::Result<()> {
    // 1. Adapter: connect to database (your application runs DDL)
    let db = SqliteAdapter::connect("sqlite::memory:").await?;
    dbmr::DatabaseAdapter::query(
        db.as_ref(),
        "CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, content TEXT)",
        &[],
    ).await?;

    // 2. Model: define validation rules and behavior
    let posts = Model::builder(db, "posts")
        .column("id", "integer")
        .column("title", "required|string|maxLength:120")
        .column("content", "string|nullable")
        .build()?;

    // 3. Router: generate Axum REST router
    let app = Router::new().nest("/posts", create_model_router(posts));

    Ok(())
}
```

## Documentation & Examples

- [Feature Tour](https://github.com/AvinashSKaranth/db-model-router-rs/blob/master/crates/dbmr/examples/tour.rs)
- [Blog App Example](https://github.com/AvinashSKaranth/db-model-router-rs/blob/master/crates/dbmr/examples/blog_app.rs)
- [Full Documentation](https://docs.rs/db-model-router)
