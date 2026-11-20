//! `dbmr-core`: models, filters, validation, field encryption, hooks, events and
//! transactions. Database access goes through the [`DatabaseAdapter`] trait
//! (implementations live in `dbmr-adapters`).

pub mod adapter;
pub mod encryption;
pub mod error;
pub mod events;
pub mod filter;
pub mod hooks;
pub mod json;
pub mod model;
pub mod query;
pub mod rules;
pub mod transaction;
pub mod typed;
pub mod types;

pub use adapter::{DatabaseAdapter, PoolConfig, TransactionAdapter, NO_TX_MESSAGE};
pub use encryption::{decrypt, encrypt, is_encrypted, EncryptionConfig};
pub use error::{AppError, Result};
pub use events::EventSink;
pub use filter::{
    apply_search, data_to_filter, normalize_sort, object_to_filter, parse_filter_value, Condition,
    FilterExpr, Operator,
};
pub use hooks::{BulkOp, Context, CurrentUser, ModelHooks, Scope};
pub use json::{json_safe_parse, json_stringify};
pub use model::{
    Model, ModelBuilder, ModelInstance, ModelOptions, Relation, RelationKind, CONFLICT_MESSAGE,
    VERSION_REQUIRED_MESSAGE,
};
pub use query::Query;
pub use transaction::Transaction;
pub use typed::{Page, TypedModel};
pub use types::{
    json_to_row, row, row_to_json, DbValue, FindResult, GetOptions, RowMap, SortDirection, SortKey,
    WriteSummary,
};
