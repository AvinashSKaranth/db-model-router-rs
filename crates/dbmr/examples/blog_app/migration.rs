//! Migration runner module for the Blog Application.
//!
//! Applies all SQL migration files from the `migrations/` folder sequentially
//! against the database adapter before the application begins serving requests.

use dbmr::core::types::DbValue;
use dbmr::DatabaseAdapter;
use std::sync::Arc;

/// Representation of an individual SQL migration script.
pub struct Migration {
    pub name: &'static str,
    pub sql: &'static str,
}

/// All ordered migration scripts located in the `migrations/` folder.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        name: "0001_create_authors.sql",
        sql: include_str!("migrations/0001_create_authors.sql"),
    },
    Migration {
        name: "0002_create_posts.sql",
        sql: include_str!("migrations/0002_create_posts.sql"),
    },
    Migration {
        name: "0003_create_comments.sql",
        sql: include_str!("migrations/0003_create_comments.sql"),
    },
    Migration {
        name: "0004_create_likes.sql",
        sql: include_str!("migrations/0004_create_likes.sql"),
    },
    Migration {
        name: "0005_create_shares.sql",
        sql: include_str!("migrations/0005_create_shares.sql"),
    },
];

/// Execute all migrations sequentially on the provided database adapter before the application starts.
///
/// Creates a `_migrations` tracking table if it does not already exist, and only executes
/// migrations that have not yet been recorded.
pub async fn run_migrations(db: &Arc<dyn DatabaseAdapter>) -> dbmr::Result<()> {
    // 1. Ensure migrations tracking table exists
    db.query(
        "CREATE TABLE IF NOT EXISTS _migrations (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL UNIQUE,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
        &[],
    )
    .await?;

    // 2. Run each migration in sequence if not already applied
    for migration in MIGRATIONS {
        let existing = db
            .query(
                "SELECT id FROM _migrations WHERE name = ?",
                &[DbValue::Text(migration.name.to_string())],
            )
            .await?;

        if existing.is_empty() {
            tracing::info!(migration = migration.name, "Applying migration");
            // Execute the migration DDL
            db.query(migration.sql, &[]).await?;
            // Record migration in _migrations
            db.query(
                "INSERT INTO _migrations (name) VALUES (?)",
                &[DbValue::Text(migration.name.to_string())],
            )
            .await?;
        }
    }

    Ok(())
}
