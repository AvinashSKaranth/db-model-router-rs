use crate::adapters::compiler::{
    compile_distinct, compile_order_by, compile_update, compile_where, format_bulk_saved_message,
    format_remove_message, SqlDialect,
};
use crate::core::adapter::{DatabaseAdapter, PoolConfig, TransactionAdapter};
use crate::core::error::{AppError, Result};
use crate::core::filter::FilterExpr;
use crate::core::types::{DbValue, FindResult, GetOptions, RowMap, SortKey, WriteSummary};
use async_trait::async_trait;
use chrono::SecondsFormat;
use compact_str::CompactString;
use sqlx::mysql::{
    MySqlArguments, MySqlConnectOptions, MySqlPool, MySqlPoolOptions, MySqlQueryResult, MySqlRow,
};
use sqlx::{Column, MySql, Row, TypeInfo};
use std::collections::{HashMap, HashSet};
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex;

const D: SqlDialect = SqlDialect::Mysql;

type Tx = Arc<Mutex<Option<sqlx::Transaction<'static, MySql>>>>;

#[derive(Clone)]
enum Conn {
    Pool(MySqlPool),
    Tx(Tx),
}

/// MySQL / MariaDB adapter. The same type serves as the transaction-scoped adapter
/// returned by [`DatabaseAdapter::begin_tx`].
#[derive(Clone)]
pub struct MySqlAdapter {
    conn: Conn,
    /// DATETIME/TIMESTAMP column names per table (cached), for RFC 3339 write conversion.
    datetimes: Arc<RwLock<HashMap<String, Arc<HashSet<String>>>>>,
}

impl MySqlAdapter {
    /// Connect to a MySQL / MariaDB database by connection string.
    ///
    /// # Errors
    /// Returns [`AppError::Database`] if connection fails.
    pub async fn connect(url: &str) -> Result<Arc<Self>> {
        Self::connect_with(url, PoolConfig::default()).await
    }

    /// [`Self::connect`] with explicit pool settings.
    ///
    /// # Errors
    /// Returns [`AppError::Database`] if connection fails.
    pub async fn connect_with(url: &str, cfg: PoolConfig) -> Result<Arc<Self>> {
        let opts = MySqlConnectOptions::from_str(url)
            .map_err(|e| AppError::Database(format!("Invalid MySQL URL: {e}")))?;

        let pool = MySqlPoolOptions::new()
            .max_connections(cfg.max_connections)
            .min_connections(cfg.min_connections)
            .acquire_timeout(cfg.acquire_timeout)
            .connect_with(opts)
            .await
            .map_err(|e| AppError::Database(format!("Failed to connect to MySQL: {e}")))?;

        Ok(Arc::new(Self::new(pool)))
    }

    #[must_use]
    pub fn new(pool: MySqlPool) -> Self {
        Self {
            conn: Conn::Pool(pool),
            datetimes: Arc::default(),
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
            .map_err(|e| AppError::Database(format!("MySQL begin transaction error: {e}")))?;
        Ok(Self {
            conn: Conn::Tx(Arc::new(Mutex::new(Some(tx)))),
            datetimes: self.datetimes.clone(),
        })
    }

    async fn fetch_all(&self, sql: &str, params: &[DbValue], what: &str) -> Result<Vec<MySqlRow>> {
        let q = bind_all(sqlx::query(sql), params);
        let res = match &self.conn {
            Conn::Pool(p) => q.fetch_all(p).await,
            Conn::Tx(t) => {
                let mut g = t.lock().await;
                let tx = g.as_mut().ok_or_else(closed)?;
                q.fetch_all(&mut **tx).await
            }
        };
        res.map_err(|e| AppError::Database(format!("MySQL {what} error: {e}")))
    }

    /// `rows_affected` counts rows *matched*, not changed: sqlx sets CLIENT_FOUND_ROWS.
    async fn execute(&self, sql: &str, params: &[DbValue], what: &str) -> Result<MySqlQueryResult> {
        let q = bind_all(sqlx::query(sql), params);
        let res = match &self.conn {
            Conn::Pool(p) => q.execute(p).await,
            Conn::Tx(t) => {
                let mut g = t.lock().await;
                let tx = g.as_mut().ok_or_else(closed)?;
                q.execute(&mut **tx).await
            }
        };
        res.map_err(|e| AppError::Database(format!("MySQL {what} error: {e}")))
    }

    /// DATETIME/TIMESTAMP columns of `table` (cached; unknown table → empty).
    async fn datetime_columns(&self, table: &str) -> Result<Arc<HashSet<String>>> {
        if let Some(t) = self
            .datetimes
            .read()
            .ok()
            .and_then(|m| m.get(table).cloned())
        {
            return Ok(t);
        }
        let rows = self
            .fetch_all(
                "SELECT CAST(column_name AS CHAR) AS c FROM information_schema.columns \
                 WHERE table_schema = DATABASE() AND table_name = ? \
                 AND data_type IN ('datetime', 'timestamp')",
                &[DbValue::Text(table.to_string())],
                "query",
            )
            .await?;
        let set: Arc<HashSet<String>> =
            Arc::new(rows.iter().filter_map(|r| r.try_get("c").ok()).collect());
        if let Ok(mut m) = self.datetimes.write() {
            m.insert(table.to_string(), set.clone());
        }
        Ok(set)
    }

    /// MySQL/MariaDB reject RFC 3339 `...Z` text for DATETIME: rewrite it to
    /// `YYYY-MM-DD HH:MM:SS.ffffff` (UTC) for datetime columns only. Other columns keep
    /// the value verbatim.
    async fn for_write(&self, table: &str, row: &RowMap) -> Result<RowMap> {
        let dt = self.datetime_columns(table).await?;
        Ok(row
            .iter()
            .map(|(k, v)| {
                let v = match v {
                    DbValue::Text(s) if dt.contains(k.as_str()) => {
                        chrono::DateTime::parse_from_rfc3339(s).map_or_else(
                            |_| v.clone(),
                            |d| {
                                DbValue::Text(
                                    d.naive_utc().format("%Y-%m-%d %H:%M:%S%.6f").to_string(),
                                )
                            },
                        )
                    }
                    _ => v.clone(),
                };
                (k.clone(), v)
            })
            .collect())
    }

    /// Insert (or `ON DUPLICATE KEY UPDATE`) each row; the caller provides atomicity.
    async fn write_rows(&self, table: &str, data: &[RowMap], upsert: bool) -> Result<WriteSummary> {
        let mut last_id = None;
        for row in data {
            let row = self.for_write(table, row).await?;
            let cols: Vec<String> = row.keys().map(|k| D.quote_identifier(k)).collect();
            let mut sql = format!(
                "INSERT INTO {} ({}) VALUES ({})",
                D.quote_identifier(table),
                cols.join(", "),
                vec!["?"; row.len()].join(", ")
            );
            if upsert {
                let set: Vec<String> = cols.iter().map(|q| format!("{q} = VALUES({q})")).collect();
                sql.push_str(" ON DUPLICATE KEY UPDATE ");
                sql.push_str(&set.join(", "));
            }
            let values: Vec<DbValue> = row.values().cloned().collect();
            let what = if upsert { "upsert" } else { "insert" };
            let id = self.execute(&sql, &values, what).await?.last_insert_id();
            if id > 0 {
                last_id = Some(DbValue::Int(i128::from(id)));
            }
        }
        let count = data.len() as u64;
        Ok(WriteSummary {
            rows: count,
            message: format_bulk_saved_message(table, count),
            id: if count == 1 { last_id } else { None },
            record: None,
        })
    }

    /// Multi-row writes on a pool run inside an internal transaction (all or nothing).
    async fn bulk(&self, table: &str, data: &[RowMap], upsert: bool) -> Result<WriteSummary> {
        if data.len() < 2 || matches!(self.conn, Conn::Tx(_)) {
            return self.write_rows(table, data, upsert).await;
        }
        let tx = self.begin().await?;
        match tx.write_rows(table, data, upsert).await {
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

fn row_to_rowmap(row: &MySqlRow) -> RowMap {
    let mut map = RowMap::new();
    for col in row.columns() {
        let name = col.name();
        let type_name = col.type_info().name();

        let val = match type_name {
            "BOOLEAN" => row
                .try_get::<bool, _>(name)
                .map_or(DbValue::Null, DbValue::Bool),
            "TINYINT" | "SMALLINT" | "MEDIUMINT" | "INT" | "BIGINT" => row
                .try_get::<i64, _>(name)
                .map_or(DbValue::Null, |i| DbValue::Int(i128::from(i))),
            "TINYINT UNSIGNED" | "SMALLINT UNSIGNED" | "MEDIUMINT UNSIGNED" | "INT UNSIGNED"
            | "BIGINT UNSIGNED" => row
                .try_get::<u64, _>(name)
                .map_or(DbValue::Null, |i| DbValue::Int(i128::from(i))),
            "FLOAT" => row
                .try_get::<f32, _>(name)
                .map_or(DbValue::Null, |f| DbValue::Float(f64::from(f))),
            "DOUBLE" => row
                .try_get::<f64, _>(name)
                .map_or(DbValue::Null, DbValue::Float),
            "DECIMAL" => row
                .try_get::<rust_decimal::Decimal, _>(name)
                .map_or(DbValue::Null, DbValue::Decimal),
            "BLOB" | "TINYBLOB" | "MEDIUMBLOB" | "LONGBLOB" | "BINARY" | "VARBINARY" => row
                .try_get::<Vec<u8>, _>(name)
                .map_or(DbValue::Null, DbValue::Bytes),
            // Same ISO-8601 UTC text as the PostgreSQL adapter.
            "DATETIME" | "TIMESTAMP" => row
                .try_get::<chrono::NaiveDateTime, _>(name)
                .map_or(DbValue::Null, |v| {
                    DbValue::Text(v.and_utc().to_rfc3339_opts(SecondsFormat::Millis, true))
                }),
            "DATE" => row
                .try_get::<chrono::NaiveDate, _>(name)
                .map_or(DbValue::Null, |v| DbValue::Text(v.to_string())),
            "TIME" => row
                .try_get::<chrono::NaiveTime, _>(name)
                .map_or(DbValue::Null, |v| DbValue::Text(v.to_string())),
            // sqlx refuses `String` for the JSON type (the value read as NULL); the wire
            // payload is UTF-8 JSON text, so decode it unchecked.
            "JSON" => row
                .try_get_unchecked::<String, _>(name)
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .map_or(DbValue::Null, DbValue::Json),
            _ => row
                .try_get::<String, _>(name)
                .map_or(DbValue::Null, DbValue::Text),
        };
        map.insert(CompactString::new(name), val);
    }
    map
}

#[async_trait]
impl DatabaseAdapter for MySqlAdapter {
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
        let rows = self.fetch_all(&sql, &w.params, "query").await?;
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
            "SELECT COUNT(*) FROM {}{}",
            D.quote_identifier(table),
            w.clause
        );
        let rows = self.fetch_all(&sql, &w.params, "count").await?;
        let c: i64 = rows
            .first()
            .map_or(Ok(0), |r| r.try_get(0))
            .map_err(|e| AppError::Database(format!("MySQL count error: {e}")))?;
        Ok(u64::try_from(c).unwrap_or(0))
    }

    async fn insert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        _unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        self.bulk(table, data, false).await
    }

    async fn upsert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        _unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        self.bulk(table, data, true).await
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
        let rows = self
            .execute(&sql, &w.params, "remove")
            .await?
            .rows_affected();
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
        if set.is_empty() {
            return Ok(0);
        }
        let set = self.for_write(table, set).await?;
        match compile_update(D, table, &set, filter, safe_delete, None) {
            Some((sql, params)) => Ok(self.execute(&sql, &params, "update").await?.rows_affected()),
            None => Ok(0),
        }
    }

    async fn query(&self, sql: &str, params: &[DbValue]) -> Result<Vec<RowMap>> {
        // MySQL (not MariaDB) refuses savepoint statements as prepared statements (1295);
        // core's `Transaction` sends them without params, so use the text protocol.
        let upper = sql.trim_start().to_ascii_uppercase();
        if let (true, Conn::Tx(t)) = (
            params.is_empty()
                && ["SAVEPOINT", "RELEASE SAVEPOINT", "ROLLBACK TO"]
                    .iter()
                    .any(|p| upper.starts_with(p)),
            &self.conn,
        ) {
            let mut g = t.lock().await;
            let tx = g.as_mut().ok_or_else(closed)?;
            // `&str` carries no arguments, so sqlx sends it as a plain COM_QUERY.
            let conn: &mut sqlx::MySqlConnection = tx;
            sqlx::Executor::execute(conn, sql)
                .await
                .map_err(|e| AppError::Database(format!("MySQL raw query error: {e}")))?;
            return Ok(Vec::new());
        }
        Ok(self
            .fetch_all(sql, params, "raw query")
            .await?
            .iter()
            .map(row_to_rowmap)
            .collect())
    }

    fn escape_id(&self, name: &str) -> String {
        D.quote_identifier(name)
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
        let empty = FilterExpr::empty();
        let f = filter.unwrap_or(&empty);
        let (sql, params) = compile_distinct(D, table, column, f, safe_delete, search, limit, None);
        let rows = self.query(&sql, &params).await?;
        Ok(rows
            .into_iter()
            .filter_map(|mut r| {
                r.shift_remove("val")
                    .or_else(|| r.into_iter().next().map(|(_, v)| v))
            })
            .filter(|v| !v.is_null())
            .collect())
    }

    async fn begin_tx(&self) -> Result<Arc<dyn TransactionAdapter>> {
        Ok(Arc::new(self.begin().await?))
    }
}

#[async_trait]
impl TransactionAdapter for MySqlAdapter {
    async fn commit(&self) -> Result<()> {
        let Conn::Tx(t) = &self.conn else {
            return Ok(());
        };
        if let Some(tx) = t.lock().await.take() {
            tx.commit()
                .await
                .map_err(|e| AppError::Database(format!("MySQL transaction commit error: {e}")))?;
        }
        Ok(())
    }

    async fn rollback(&self) -> Result<()> {
        let Conn::Tx(t) = &self.conn else {
            return Ok(());
        };
        if let Some(tx) = t.lock().await.take() {
            tx.rollback().await.map_err(|e| {
                AppError::Database(format!("MySQL transaction rollback error: {e}"))
            })?;
        }
        Ok(())
    }

    fn as_adapter(self: Arc<Self>) -> Arc<dyn DatabaseAdapter> {
        self
    }
}

fn bind_all<'a>(
    mut q: sqlx::query::Query<'a, MySql, MySqlArguments>,
    params: &'a [DbValue],
) -> sqlx::query::Query<'a, MySql, MySqlArguments> {
    for v in params {
        q = match v {
            DbValue::Null => q.bind(None::<String>),
            DbValue::Bool(b) => q.bind(*b),
            #[allow(clippy::cast_possible_truncation)]
            DbValue::Int(i) => q.bind(*i as i64),
            DbValue::Float(f) => q.bind(*f),
            DbValue::Decimal(d) => q.bind(*d),
            DbValue::Text(s) => q.bind(s.as_str()),
            DbValue::Bytes(b) => q.bind(b.as_slice()),
            DbValue::Json(j) => q.bind(j.to_string()),
        };
    }
    q
}
