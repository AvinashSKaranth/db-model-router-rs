# dbmr

Database-agnostic **models** and generated **Axum REST routers** for Rust services.
High-performance async Rust rewrite of the Node.js `db-model-router` with complete feature parity.

[![Crates.io](https://img.shields.io/crates/v/dbmr.svg)](https://crates.io/crates/dbmr)
[![Docs.rs](https://docs.rs/dbmr/badge.svg)](https://docs.rs/dbmr)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

## Features

- **10 Database Adapters**: SQLite, PostgreSQL, CockroachDB, MySQL, MariaDB, MSSQL, Oracle, MongoDB, Redis, DynamoDB.
- **Declarative Models**: Type validation, unique constraints, soft delete, auto timestamps, optimistic locking (ETag/If-Match), field encryption (AES-256-GCM).
- **Row-Level Scopes**: Security first. Enforces tenant, owner, or parent permissions on every query and mutation.
- **Generated Axum REST Routers**: Automatic list paging, filtering, searching, sorting, individual & bulk CRUD, and OpenAPI generation.
- **Multi-Entity Relations**: `has_many` and `belongs_to` with eager loading via `?include=...`.
- **Change Events**: Optional CDC stream to Kafka topics.

---

## Adding to Your Project

Add `dbmr` to your `Cargo.toml`:

```toml
[dependencies]
dbmr = "0.0.1"        # features: sqlite (default), postgres, mysql, etc.
axum = "0.8"
tokio = { version = "1", features = ["full"] }
serde_json = "1"
```

In your Rust code, import via `dbmr`:

```rust
use dbmr::adapters::SqliteAdapter;
use dbmr::{create_model_router, Context, Model, Scope};
```

---

## Adding the Skill to Your AI Agent Workspace

If you are using AI coding assistants (such as Claude Code, Cursor, GitHub Copilot, Antigravity, or other Agentic workflows), you can install the official `dbmr` agent skill directly into your workspace:

```bash
npx skills add https://github.com/AvinashSKaranth/db-model-router-rs --path skills/dbmr
```

This equips your AI agent with the complete API contract, security patterns, scope middleware implementations, and error-handling best practices for `dbmr`.

---

## Individual & Bulk Route Endpoints

Every `create_model_router(model)` router automatically exposes both individual and bulk operations:

| HTTP Method | Route | Type | Request Payload | Response |
|---|---|---|---|---|
| `GET` | `/resources` | Bulk | Query params (`?page=0&size=30&...`) | `{"data": [...], "count": total}` |
| `POST` | `/resources` | Individual | `{ "title": "My Post" }` | Created record `{ "id": 1, ... }` |
| `POST` | `/resources` | Bulk | `{"data": [{ "title": "P1" }, { "title": "P2" }]}` | `{"rows": 2, "message": "...", "type": "success"}` |
| `PUT` | `/resources` | Bulk | `{"data": [{ "id": 1, ... }, { "id": 2, ... }]}` | `{"rows": 2, "message": "...", "type": "success"}` |
| `DELETE` | `/resources` | Bulk | Non-empty filter criteria object | `{"rows": n, "message": "...", "type": "success"}` |
| `GET` | `/resources/:id` | Individual | None | Single record `{ "id": 1, ... }` |
| `PUT` | `/resources/:id` | Individual | `{ "title": "Updated" }` (full replace) | Updated record `{ "id": 1, ... }` |
| `PATCH` | `/resources/:id` | Individual | `{ "title": "Partial update" }` | Updated record `{ "id": 1, ... }` |
| `DELETE` | `/resources/:id` | Individual | None | `{"rows": 1, "message": "...", "type": "success"}` |

---

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

---

## Documentation & Examples

- [Blog App Modular Project (`index`, `migration`, `model`, `router`, `test`, `migrations/`)](https://github.com/AvinashSKaranth/db-model-router-rs/tree/master/crates/dbmr/examples/blog_app)
- [Complete Feature Tour](https://github.com/AvinashSKaranth/db-model-router-rs/blob/master/crates/dbmr/examples/tour.rs)
- [API Reference on Docs.rs](https://docs.rs/dbmr)
