use crate::core::error::{AppError, Result};
use crate::core::filter::FilterExpr;
use crate::core::types::{DbValue, FindResult, GetOptions, RowMap, SortKey, WriteSummary};
use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait DatabaseAdapter: Send + Sync + 'static {
    async fn disconnect(&self) -> Result<()>;
    async fn is_connected(&self) -> bool;

    async fn get(
        &self,
        table: &str,
        filter: &FilterExpr,
        sort: &[SortKey],
        safe_delete: Option<&str>,
        options: GetOptions,
    ) -> Result<FindResult>;

    async fn list(
        &self,
        table: &str,
        filter: &FilterExpr,
        sort: &[SortKey],
        safe_delete: Option<&str>,
        page: u64,
        size: u64,
    ) -> Result<FindResult>;

    async fn count(
        &self,
        table: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64>;

    /// Insert new rows. A primary-key collision must fail without changing the existing
    /// row. Guarded model upserts use this operation when their conditional update misses.
    async fn insert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        unique: &[Vec<String>],
    ) -> Result<WriteSummary>;

    async fn upsert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        unique: &[Vec<String>],
    ) -> Result<WriteSummary>;

    async fn remove_rows(
        &self,
        table: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<WriteSummary>;

    /// `UPDATE table SET <set> WHERE <filter>`, skipping soft-deleted rows when
    /// `safe_delete` is set. Returns the number of rows changed. Used for optimistic
    /// locking and scoped writes, so the filter must be enforced atomically with each
    /// row mutation.
    async fn update_where(
        &self,
        table: &str,
        set: &RowMap,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64>;

    async fn query(&self, sql: &str, params: &[DbValue]) -> Result<Vec<RowMap>>;

    /// Cheap liveness check for readiness probes. SQL default: `SELECT 1`.
    async fn ping(&self) -> Result<()> {
        self.query("SELECT 1", &[]).await.map(|_| ())
    }

    fn escape_id(&self, name: &str) -> String;

    /// Inspect distinct values of a column (for filter discovery and autocomplete).
    async fn distinct_values(
        &self,
        table: &str,
        column: &str,
        safe_delete: Option<&str>,
        filter: Option<&FilterExpr>,
        search: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DbValue>> {
        let empty_filter = FilterExpr::empty();
        let f = filter.unwrap_or(&empty_filter);
        let needle = search.filter(|s| !s.is_empty()).map(str::to_lowercase);
        let sort = [crate::core::types::SortKey {
            column: column.into(),
            direction: crate::core::types::SortDirection::Asc,
        }];
        let mut seen = std::collections::HashSet::new();
        let mut out = Vec::new();
        let batch_size: u64 = 100;
        let mut offset: u64 = 0;
        loop {
            let res = self
                .get(
                    table,
                    f,
                    &sort,
                    safe_delete,
                    GetOptions {
                        count: false,
                        limit: Some(batch_size),
                        offset: Some(offset),
                    },
                )
                .await?;
            if res.data.is_empty() {
                break;
            }
            let count = res.data.len() as u64;
            for row in res.data {
                if let Some(val) = row.get(column) {
                    if val.is_null() {
                        continue;
                    }
                    if let Some(ref q) = needle {
                        if !val.to_string().to_lowercase().contains(q) {
                            continue;
                        }
                    }
                    if seen.insert(val.to_json().to_string()) {
                        out.push(val.clone());
                        if out.len() >= limit {
                            return Ok(out);
                        }
                    }
                }
            }
            offset += count;
            if count < batch_size {
                break;
            }
        }
        Ok(out)
    }

    /// Start a transaction. SQLite, PostgreSQL/CockroachDB, MySQL/MariaDB and MSSQL implement this;
    /// every other adapter returns [`NO_TX_MESSAGE`].
    async fn begin_tx(&self) -> Result<Arc<dyn TransactionAdapter>> {
        Err(AppError::Internal(NO_TX_MESSAGE.to_string()))
    }
}

/// Connection-pool settings for `connect_with` on every adapter that pools.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    pub max_connections: u32,
    pub min_connections: u32,
    pub acquire_timeout: std::time::Duration,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            max_connections: 10,
            min_connections: 0,
            acquire_timeout: std::time::Duration::from_secs(30),
        }
    }
}

/// Exact error message for adapters without transaction support (§26).
pub const NO_TX_MESSAGE: &str = "Active database adapter does not support transactions.";

/// A transaction-scoped adapter: every [`DatabaseAdapter`] operation runs inside the
/// transaction. Use [`crate::core::transaction::Transaction`] rather than this trait directly;
/// it adds savepoints and event buffering.
#[async_trait]
pub trait TransactionAdapter: DatabaseAdapter {
    async fn commit(&self) -> Result<()>;
    async fn rollback(&self) -> Result<()>;
    /// Upcast (trait upcasting needs Rust 1.86; MSRV is 1.85). Implement as `self`.
    fn as_adapter(self: Arc<Self>) -> Arc<dyn DatabaseAdapter>;
}

#[async_trait]
impl<T: DatabaseAdapter + ?Sized> DatabaseAdapter for Arc<T> {
    async fn disconnect(&self) -> Result<()> {
        (**self).disconnect().await
    }

    async fn is_connected(&self) -> bool {
        (**self).is_connected().await
    }

    async fn get(
        &self,
        table: &str,
        filter: &FilterExpr,
        sort: &[SortKey],
        safe_delete: Option<&str>,
        options: GetOptions,
    ) -> Result<FindResult> {
        (**self)
            .get(table, filter, sort, safe_delete, options)
            .await
    }

    async fn list(
        &self,
        table: &str,
        filter: &FilterExpr,
        sort: &[SortKey],
        safe_delete: Option<&str>,
        page: u64,
        size: u64,
    ) -> Result<FindResult> {
        (**self)
            .list(table, filter, sort, safe_delete, page, size)
            .await
    }

    async fn count(
        &self,
        table: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64> {
        (**self).count(table, filter, safe_delete).await
    }

    async fn insert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        (**self).insert_rows(table, data, unique).await
    }

    async fn upsert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        (**self).upsert_rows(table, data, unique).await
    }

    async fn remove_rows(
        &self,
        table: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<WriteSummary> {
        (**self).remove_rows(table, filter, safe_delete).await
    }

    async fn update_where(
        &self,
        table: &str,
        set: &RowMap,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64> {
        (**self).update_where(table, set, filter, safe_delete).await
    }

    async fn query(&self, sql: &str, params: &[DbValue]) -> Result<Vec<RowMap>> {
        (**self).query(sql, params).await
    }

    async fn ping(&self) -> Result<()> {
        (**self).ping().await
    }

    fn escape_id(&self, name: &str) -> String {
        (**self).escape_id(name)
    }

    async fn distinct_values(
        &self,
        table: &str,
        column: &str,
        safe_delete: Option<&str>,
        filter: Option<&FilterExpr>,
        search: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DbValue>> {
        (**self)
            .distinct_values(table, column, safe_delete, filter, search, limit)
            .await
    }

    async fn begin_tx(&self) -> Result<Arc<dyn TransactionAdapter>> {
        (**self).begin_tx().await
    }
}
