use crate::adapters::compiler::{
    compile_order_by, compile_update, compile_where, format_remove_message, int_or_text, summary,
    SqlDialect,
};
use crate::core::adapter::{DatabaseAdapter, PoolConfig, TransactionAdapter};
use crate::core::error::{AppError, Result};
use crate::core::filter::FilterExpr;
use crate::core::types::{DbValue, FindResult, GetOptions, RowMap, SortKey, WriteSummary};
use async_trait::async_trait;
use compact_str::CompactString;
use sqlx::sqlite::{
    SqliteArguments, SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow,
};
use sqlx::{Column, Row, Sqlite, TypeInfo, ValueRef};
use std::fmt::Write;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::Mutex;

const D: SqlDialect = SqlDialect::Sqlite;

type Tx = Arc<Mutex<Option<sqlx::Transaction<'static, Sqlite>>>>;

#[derive(Clone)]
enum Conn {
    Pool(SqlitePool),
    Tx(Tx),
}

/// SQLite adapter. The same type serves as the transaction-scoped adapter returned by
/// [`DatabaseAdapter::begin_tx`].
#[derive(Clone)]
pub struct SqliteAdapter {
    conn: Conn,
}

impl SqliteAdapter {
    /// Connect to a SQLite database by connection string / path.
    ///
    /// # Errors
    /// Returns [`AppError::Database`] if connection fails.
    pub async fn connect(url: &str) -> Result<Arc<Self>> {
        Self::connect_with(url, PoolConfig::default()).await
    }

    /// [`Self::connect`] with explicit pool settings. An in-memory database ignores
    /// `cfg` sizing and always keeps exactly one connection.
    ///
    /// # Errors
    /// Returns [`AppError::Database`] if connection fails.
    pub async fn connect_with(url: &str, cfg: PoolConfig) -> Result<Arc<Self>> {
        let opts = SqliteConnectOptions::from_str(url)
            .map_err(|e| AppError::Database(format!("Invalid SQLite URL: {e}")))?
            .create_if_missing(true);

        // An in-memory database is dropped when its last connection closes (pool idle
        // reaping would silently wipe it): keep exactly one connection open forever.
        let memory = url.contains(":memory:") || url.contains("mode=memory");
        let pool = if memory {
            SqlitePoolOptions::new()
                .max_connections(1)
                .min_connections(1)
                .idle_timeout(None)
                .max_lifetime(None)
        } else {
            SqlitePoolOptions::new()
                .max_connections(cfg.max_connections)
                .min_connections(cfg.min_connections)
        }
        .acquire_timeout(cfg.acquire_timeout)
        .connect_with(opts)
        .await
        .map_err(|e| AppError::Database(format!("Failed to connect to SQLite: {e}")))?;

        Ok(Arc::new(Self::new(pool)))
    }

    #[must_use]
    pub fn new(pool: SqlitePool) -> Self {
        Self {
            conn: Conn::Pool(pool),
        }
    }

    async fn begin(&self) -> Result<Self> {
        let Conn::Pool(pool) = &self.conn else {
            return Err(AppError::Internal(
                "Transaction already open; use Transaction::savepoint for nesting.".into(),
            ));
        };
        let tx = pool
            .begin()
            .await
            .map_err(|e| AppError::Database(format!("SQLite begin transaction error: {e}")))?;
        Ok(Self {
            conn: Conn::Tx(Arc::new(Mutex::new(Some(tx)))),
        })
    }

    async fn fetch_all(&self, sql: &str, params: &[DbValue]) -> Result<Vec<SqliteRow>> {
        let q = bind_all(sqlx::query(sql), params);
        let res = match &self.conn {
            Conn::Pool(p) => q.fetch_all(p).await,
            Conn::Tx(t) => {
                let mut g = t.lock().await;
                let tx = g.as_mut().ok_or_else(closed)?;
                q.fetch_all(&mut **tx).await
            }
        };
        res.map_err(|e| AppError::Database(format!("SQLite query error: {e}")))
    }

    async fn execute(&self, sql: &str, params: &[DbValue]) -> Result<u64> {
        let q = bind_all(sqlx::query(sql), params);
        let res = match &self.conn {
            Conn::Pool(p) => q.execute(p).await,
            Conn::Tx(t) => {
                let mut g = t.lock().await;
                let tx = g.as_mut().ok_or_else(closed)?;
                q.execute(&mut **tx).await
            }
        };
        res.map(|r| r.rows_affected())
            .map_err(|e| AppError::Database(format!("SQLite query error: {e}")))
    }

    /// Insert (or upsert on `target`) each row; the caller provides atomicity.
    async fn write_rows(
        &self,
        table: &str,
        data: &[RowMap],
        target: Option<&[String]>,
    ) -> Result<WriteSummary> {
        let mut last = None;
        for row in data {
            let cols: Vec<String> = row.keys().map(|k| D.quote_identifier(k)).collect();
            let mut sql = format!(
                "INSERT INTO {} ({}) VALUES ({})",
                D.quote_identifier(table),
                cols.join(", "),
                vec!["?"; row.len()].join(", ")
            );
            if let Some(target) = target {
                let set: Vec<String> = row
                    .keys()
                    .filter(|k| !target.iter().any(|t| t == k.as_str()))
                    .map(|k| {
                        let q = D.quote_identifier(k);
                        format!("{q} = excluded.{q}")
                    })
                    .collect();
                let conflict: Vec<String> = target.iter().map(|c| D.quote_identifier(c)).collect();
                let _ = write!(sql, " ON CONFLICT ({})", conflict.join(", "));
                if set.is_empty() {
                    sql.push_str(" DO NOTHING");
                } else {
                    let _ = write!(sql, " DO UPDATE SET {}", set.join(", "));
                }
            }
            sql.push_str(" RETURNING *");
            let values: Vec<DbValue> = row.values().cloned().collect();
            last = self
                .fetch_all(&sql, &values)
                .await?
                .first()
                .map(row_to_rowmap);
        }
        Ok(summary(table, data.len() as u64, last))
    }

    /// Multi-row writes on a pool run inside an internal transaction (all or nothing).
    async fn bulk(
        &self,
        table: &str,
        data: &[RowMap],
        target: Option<&[String]>,
    ) -> Result<WriteSummary> {
        if data.len() < 2 || matches!(self.conn, Conn::Tx(_)) {
            return self.write_rows(table, data, target).await;
        }
        let tx = self.begin().await?;
        match tx.write_rows(table, data, target).await {
            Ok(s) => {
                tx.commit().await?;
                Ok(s)
            }
            Err(e) => {
                let _ = tx.rollback().await;
                Err(e)
            }
        }
    }
}

fn closed() -> AppError {
    AppError::Database("Transaction already closed".to_string())
}

/// Decode by the stored value's type (SQLite is dynamically typed); a declared BOOLEAN
/// column holding an integer decodes as `Bool`.
fn row_to_rowmap(row: &SqliteRow) -> RowMap {
    let mut map = RowMap::new();
    for (i, col) in row.columns().iter().enumerate() {
        let val = match row.try_get_raw(i) {
            Ok(raw) if !raw.is_null() => {
                let storage = raw.type_info().name().to_string();
                match (col.type_info().name(), storage.as_str()) {
                    ("BOOLEAN", "INTEGER") => row
                        .try_get_unchecked::<i64, _>(i)
                        .map_or(DbValue::Null, |v| DbValue::Bool(v != 0)),
                    (_, "INTEGER") => row
                        .try_get_unchecked::<i64, _>(i)
                        .map_or(DbValue::Null, |v| DbValue::Int(i128::from(v))),
                    (_, "REAL") => row
                        .try_get_unchecked::<f64, _>(i)
                        .map_or(DbValue::Null, DbValue::Float),
                    (_, "BLOB") => row
                        .try_get_unchecked::<Vec<u8>, _>(i)
                        .map_or(DbValue::Null, DbValue::Bytes),
                    _ => row
                        .try_get_unchecked::<String, _>(i)
                        .map_or(DbValue::Null, DbValue::Text),
                }
            }
            _ => DbValue::Null,
        };
        map.insert(CompactString::new(col.name()), val);
    }
    map
}

#[async_trait]
impl DatabaseAdapter for SqliteAdapter {
    async fn disconnect(&self) -> Result<()> {
        if let Conn::Pool(p) = &self.conn {
            p.close().await;
        }
        Ok(())
    }

    async fn is_connected(&self) -> bool {
        match &self.conn {
            Conn::Pool(p) => !p.is_closed(),
            Conn::Tx(t) => t.lock().await.is_some(),
        }
    }

    async fn get(
        &self,
        table: &str,
        filter: &FilterExpr,
        sort: &[SortKey],
        safe_delete: Option<&str>,
        options: GetOptions,
    ) -> Result<FindResult> {
        let w = compile_where(D, filter, safe_delete, 0);
        let limit_sql = match (options.limit, options.offset) {
            (Some(l), Some(o)) => format!(" LIMIT {l} OFFSET {o}"),
            (Some(l), None) => format!(" LIMIT {l}"),
            _ => String::new(),
        };
        let sql = format!(
            "SELECT * FROM {}{}{}{}",
            D.quote_identifier(table),
            w.clause,
            compile_order_by(D, sort),
            limit_sql
        );
        let rows = self.fetch_all(&sql, &w.params).await?;
        let count = if options.count {
            self.count(table, filter, safe_delete).await?
        } else {
            rows.len() as u64
        };
        Ok(FindResult {
            data: rows.iter().map(row_to_rowmap).collect(),
            count,
        })
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
        let options = GetOptions {
            count: true,
            limit: Some(size),
            offset: Some(page.saturating_mul(size)),
        };
        self.get(table, filter, sort, safe_delete, options).await
    }

    async fn count(
        &self,
        table: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64> {
        let w = compile_where(D, filter, safe_delete, 0);
        let sql = format!(
            "SELECT COUNT(*) AS c FROM {}{}",
            D.quote_identifier(table),
            w.clause
        );
        let rows = self.fetch_all(&sql, &w.params).await?;
        let c: i64 = rows
            .first()
            .map_or(Ok(0), |r| r.try_get(0))
            .map_err(|e| AppError::Database(format!("SQLite count error: {e}")))?;
        Ok(u64::try_from(c).unwrap_or(0))
    }

    async fn insert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        _unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        self.bulk(table, data, None).await
    }

    /// Conflict target is `unique[0]`; with no unique keys this is a plain insert (Node).
    async fn upsert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        self.bulk(table, data, unique.first().map(Vec::as_slice))
            .await
    }

    async fn remove_rows(
        &self,
        table: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<WriteSummary> {
        let w = compile_where(D, filter, safe_delete, 0);
        let sql = match safe_delete {
            Some(sd) => format!(
                "UPDATE {} SET {} = 1{}",
                D.quote_identifier(table),
                D.quote_identifier(sd),
                w.clause
            ),
            None => format!("DELETE FROM {}{}", D.quote_identifier(table), w.clause),
        };
        let rows = self.execute(&sql, &w.params).await?;
        Ok(WriteSummary {
            rows,
            message: format_remove_message(table, rows),
            id: None,
            record: None,
        })
    }

    async fn update_where(
        &self,
        table: &str,
        set: &RowMap,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64> {
        match compile_update(D, table, set, filter, safe_delete, None) {
            Some((sql, params)) => self.execute(&sql, &params).await,
            None => Ok(0),
        }
    }

    async fn query(&self, sql: &str, params: &[DbValue]) -> Result<Vec<RowMap>> {
        Ok(self
            .fetch_all(sql, params)
            .await?
            .iter()
            .map(row_to_rowmap)
            .collect())
    }

    fn escape_id(&self, name: &str) -> String {
        D.quote_identifier(name)
    }

    async fn begin_tx(&self) -> Result<Arc<dyn TransactionAdapter>> {
        Ok(Arc::new(self.begin().await?))
    }
}

#[async_trait]
impl TransactionAdapter for SqliteAdapter {
    async fn commit(&self) -> Result<()> {
        let Conn::Tx(t) = &self.conn else {
            return Ok(());
        };
        if let Some(tx) = t.lock().await.take() {
            tx.commit()
                .await
                .map_err(|e| AppError::Database(format!("SQLite transaction commit error: {e}")))?;
        }
        Ok(())
    }

    async fn rollback(&self) -> Result<()> {
        let Conn::Tx(t) = &self.conn else {
            return Ok(());
        };
        if let Some(tx) = t.lock().await.take() {
            tx.rollback().await.map_err(|e| {
                AppError::Database(format!("SQLite transaction rollback error: {e}"))
            })?;
        }
        Ok(())
    }

    fn as_adapter(self: Arc<Self>) -> Arc<dyn DatabaseAdapter> {
        self
    }
}

fn bind_all<'a>(
    mut q: sqlx::query::Query<'a, Sqlite, SqliteArguments<'a>>,
    params: &'a [DbValue],
) -> sqlx::query::Query<'a, Sqlite, SqliteArguments<'a>> {
    for v in params {
        q = match v {
            DbValue::Null => q.bind(None::<String>),
            DbValue::Bool(b) => q.bind(*b),
            DbValue::Int(i) => match int_or_text(*i) {
                Ok(n) => q.bind(n),
                Err(s) => q.bind(s),
            },
            DbValue::Float(f) => q.bind(*f),
            DbValue::Decimal(d) => q.bind(d.to_string()),
            DbValue::Text(s) => q.bind(s.as_str()),
            DbValue::Bytes(b) => q.bind(b.as_slice()),
            DbValue::Json(j) => q.bind(j.to_string()),
        };
    }
    q
}
