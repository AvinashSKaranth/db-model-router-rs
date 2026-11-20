//! # dbmr
//!
//! Database-agnostic data modeling and custom HTTP routers for Rust web services.
//!
//! ## Modules
//! - [`core`]: Core model operations, row types, encryption, and filter AST.
//! - [`schema`]: Schema parsing, validation, and generation.
//! - [`adapters`]: Multi-database adapters (`SQLite`, `PostgreSQL`, `MySQL`, etc.).
//! - [`http`]: Custom Axum router generation with QUERY method support.

pub mod adapters;
pub mod core;
pub mod http;
pub mod schema;

#[cfg(feature = "kafka")]
pub mod kafka;

pub use adapters::{DatabaseAdapter, TransactionAdapter};
pub use core::{
    AppError, Context, CurrentUser, DbValue, EncryptionConfig, Model, ModelHooks, Query, Result,
    RowMap, Scope, Transaction,
};
pub use core::{Page, TypedModel};
pub use http::create_model_router;

use indexmap::IndexMap;
use std::sync::{Arc, OnceLock};

static GLOBAL_DB: OnceLock<Arc<dyn DatabaseAdapter>> = OnceLock::new();

/// Initialize the global database adapter singleton.
///
/// # Errors
/// Returns [`AppError::Internal`] if the database adapter has already been initialized.
pub fn init_global(adapter: Arc<dyn DatabaseAdapter>) -> Result<()> {
    GLOBAL_DB
        .set(adapter)
        .map_err(|_| AppError::Internal("Database adapter already initialized".to_string()))
}

/// Retrieve the global database adapter singleton if set.
#[must_use]
pub fn global_db() -> Option<Arc<dyn DatabaseAdapter>> {
    GLOBAL_DB.get().cloned()
}

/// Build a model from `dbmr-schema` metadata. Pass the encryption config when the schema
/// marks columns `encrypted` (otherwise this fails, so ciphertext is never skipped).
///
/// # Errors
/// Invalid rule, or encrypted columns without `encryption`.
pub fn model_from_meta(
    adapter: Arc<dyn DatabaseAdapter>,
    meta: &schema::ModelMeta,
    encryption: Option<Arc<core::EncryptionConfig>>,
) -> Result<core::Model> {
    core::Model::builder(adapter, &meta.table)
        .structure(meta.structure.clone())
        .primary_key(&meta.primary_key)
        .unique_keys(meta.unique.clone())
        .options(core::ModelOptions {
            safe_delete: meta.option.safe_delete.clone(),
            created_at: meta.option.created_at.as_ref().map(|c| vec![c.clone()]),
            modified_at: meta.option.modified_at.as_ref().map(|m| vec![m.clone()]),
            search_columns: meta.option.search_columns.clone(),
            allow_destructive: false,
            hidden: meta.option.hidden.clone(),
            version_column: meta.option.version.clone(),
            // Timestamps named in the schema are set by the model.
            auto_created_at: meta.option.created_at.clone(),
            auto_modified_at: meta.option.modified_at.clone(),
            require_scope: meta.option.require_scope.clone(),
        })
        .encryption_shared(encryption)
        .build()
}

/// Parse schema JSON and build every model. `relationships` (and `parent`) become
/// `has_many` (`?include=<child table>`) on the parent and `belongs_to`
/// (`?include=<parent table>`) on the child.
///
/// # Errors
/// Schema parse error, or any [`model_from_meta`] error.
pub fn load_models_from_schema(
    adapter: &Arc<dyn DatabaseAdapter>,
    schema_json: &str,
    encryption: Option<&Arc<core::EncryptionConfig>>,
) -> Result<IndexMap<String, core::Model>> {
    let schema =
        schema::parse_schema(schema_json).map_err(|e| AppError::Validation(e.to_string()))?;
    let metas = schema::schema_to_model_meta(&schema);
    let mut map = IndexMap::new();
    for meta in metas {
        map.insert(
            meta.table.clone(),
            model_from_meta(adapter.clone(), &meta, encryption.cloned())?,
        );
    }
    // Relations point at the plain models (one level of include).
    let plain = map.clone();
    for rel in &schema.relationships {
        let (Some(parent), Some(child)) = (plain.get(&rel.parent), plain.get(&rel.child)) else {
            continue;
        };
        // A `parent`-derived relation whose key is the child's own pk (both `id`) would
        // join child.id = parent.id; skip it. Use `relationships` with `foreignKey`.
        if rel.foreign_key.is_empty() || rel.foreign_key == child.pk() {
            continue;
        }
        if let Some(m) = map.get_mut(&rel.parent) {
            *m = m.has_many(&rel.child, child, &rel.foreign_key);
        }
        if let Some(m) = map.get_mut(&rel.child) {
            *m = m.belongs_to(&rel.parent, parent, &rel.foreign_key);
        }
    }
    Ok(map)
}
