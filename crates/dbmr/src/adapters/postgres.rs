use crate::adapters::compiler::{
    coerce_value, compile_distinct, compile_order_by, compile_update, compile_where_typed,
    format_remove_message, int_or_text, placeholder, summary, ColumnTypes, SqlDialect,
};
use crate::core::adapter::{DatabaseAdapter, PoolConfig, TransactionAdapter};
use crate::core::error::{AppError, Result};
use crate::core::filter::FilterExpr;
use crate::core::types::{DbValue, FindResult, GetOptions, RowMap, SortKey, WriteSummary};
use async_trait::async_trait;
use chrono::SecondsFormat;
use compact_str::CompactString;
use sqlx::postgres::{PgArguments, PgConnectOptions, PgPool, PgPoolOptions, PgRow};
use sqlx::{Column, Postgres, Row, TypeInfo};
use std::collections::HashMap;
use std::fmt::Write;
use std::str::FromStr;
use std::sync::{Arc, RwLock};
use tokio::sync::Mutex;

const D: SqlDialect = SqlDialect::Postgres;
/// PostgreSQL bind-parameter limit per statement.
const MAX_PARAMS: usize = 65_535;

type Tx = Arc<Mutex<Option<sqlx::Transaction<'static, Postgres>>>>;

#[derive(Clone)]
enum Conn {
    Pool(PgPool),
    Tx(Tx),
}

/// PostgreSQL / CockroachDB adapter. Column types are read once per table from
/// `pg_attribute` and used to cast text/null parameters (Node `pg` coercion parity).
#[derive(Clone)]
pub struct PostgresAdapter {
    conn: Conn,
    types: Arc<RwLock<HashMap<String, Arc<ColumnTypes>>>>,
}

impl PostgresAdapter {
    /// Connect to a PostgreSQL database by connection string.
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
        let opts = PgConnectOptions::from_str(url)
            .map_err(|e| AppError::Database(format!("Invalid PostgreSQL URL: {e}")))?;
        let pool = PgPoolOptions::new()
            .max_connections(cfg.max_connections)
            .min_connections(cfg.min_connections)
            .acquire_timeout(cfg.acquire_timeout)
            .connect_with(opts)
            .await
            .map_err(|e| AppError::Database(format!("Failed to connect to PostgreSQL: {e}")))?;
        Ok(Arc::new(Self::new(pool)))
    }

    #[must_use]
    pub fn new(pool: PgPool) -> Self {
        Self {
            conn: Conn::Pool(pool),
            types: Arc::default(),
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
            .map_err(|e| AppError::Database(format!("PostgreSQL begin transaction error: {e}")))?;
        Ok(Self {
            conn: Conn::Tx(Arc::new(Mutex::new(Some(tx)))),
            types: self.types.clone(),
        })
    }

    async fn fetch_all(&self, sql: &str, params: &[DbValue]) -> Result<Vec<PgRow>> {
        let q = bind_all(sqlx::query(sql), params);
        let res = match &self.conn {
            Conn::Pool(p) => q.fetch_all(p).await,
            Conn::Tx(t) => {
                let mut g = t.lock().await;
                let tx = g.as_mut().ok_or_else(closed)?;
                q.fetch_all(&mut **tx).await
            }
        };
        res.map_err(|e| AppError::Database(format!("PostgreSQL query error: {e}")))
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
            .map_err(|e| AppError::Database(format!("PostgreSQL query error: {e}")))
    }

    /// Column name → `format_type()` for `table` (cached; unknown table → empty map).
    async fn column_types(&self, table: &str) -> Result<Arc<ColumnTypes>> {
        if let Some(t) = self.types.read().ok().and_then(|m| m.get(table).cloned()) {
            return Ok(t);
        }
        let rows = self
            .fetch_all(
                "SELECT a.attname::text AS name, format_type(a.atttypid, a.atttypmod) AS ty \
                 FROM pg_attribute a WHERE a.attrelid = to_regclass($1) \
                 AND a.attnum > 0 AND NOT a.attisdropped",
                &[DbValue::Text(D.quote_identifier(table))],
            )
            .await?;
        let map: ColumnTypes = rows
            .iter()
            .filter_map(|r| Some((r.try_get("name").ok()?, r.try_get("ty").ok()?)))
            .collect();
        let map = Arc::new(map);
        if let Ok(mut m) = self.types.write() {
            m.insert(table.to_string(), map.clone());
        }
        Ok(map)
    }

    /// One INSERT for `rows` (all with `keys`), optionally upserting on `target`.
    fn insert_sql(
        table: &str,
        keys: &[&str],
        rows: &[RowMap],
        target: Option<&[String]>,
        types: &ColumnTypes,
        returning: bool,
    ) -> (String, Vec<DbValue>) {
        let mut params = Vec::with_capacity(keys.len() * rows.len());
        let mut tuples = Vec::with_capacity(rows.len());
        for row in rows {
            let phs: Vec<String> = keys
                .iter()
                .map(|k| {
                    let v = row.get(*k).unwrap_or(&DbValue::Null);
                    params.push(coerce_value(D, k, v, Some(types)));
                    placeholder(D, params.len(), k, v, Some(types))
                })
                .collect();
            tuples.push(format!("({})", phs.join(", ")));
        }
        let cols: Vec<String> = keys.iter().map(|k| D.quote_identifier(k)).collect();
        let mut sql = format!(
            "INSERT INTO {} ({}) VALUES {}",
            D.quote_identifier(table),
            cols.join(", "),
            tuples.join(", ")
        );
        if let Some(target) = target {
            let conflict: Vec<String> = target.iter().map(|c| D.quote_identifier(c)).collect();
            let set: Vec<String> = keys
                .iter()
                .filter(|k| !target.iter().any(|t| t == *k))
                .map(|k| {
                    let q = D.quote_identifier(k);
                    format!("{q} = EXCLUDED.{q}")
                })
                .collect();
            let _ = write!(sql, " ON CONFLICT ({})", conflict.join(", "));
            if set.is_empty() {
                sql.push_str(" DO NOTHING");
            } else {
                let _ = write!(sql, " DO UPDATE SET {}", set.join(", "));
            }
        }
        if returning {
            sql.push_str(" RETURNING *");
        }
        (sql, params)
    }

    /// Rows with identical key sets go in multi-row chunks; otherwise row by row.
    async fn write_rows(
        &self,
        table: &str,
        data: &[RowMap],
        target: Option<&[String]>,
    ) -> Result<WriteSummary> {
        let types = self.column_types(table).await?;
        if data.len() == 1 {
            let keys: Vec<&str> = data[0].keys().map(CompactString::as_str).collect();
            let (sql, params) = Self::insert_sql(table, &keys, data, target, &types, true);
            let last = self
                .fetch_all(&sql, &params)
                .await?
                .first()
                .map(row_to_rowmap);
            return Ok(summary(table, 1, last));
        }
        let same_keys = data.windows(2).all(|w| w[0].keys().eq(w[1].keys()));
        if same_keys && !data.is_empty() {
            let keys: Vec<&str> = data[0].keys().map(CompactString::as_str).collect();
            for chunk in data.chunks((MAX_PARAMS / keys.len().max(1)).max(1)) {
                let (sql, params) = Self::insert_sql(table, &keys, chunk, target, &types, false);
                self.execute(&sql, &params).await?;
            }
        } else {
            for row in data {
                let keys: Vec<&str> = row.keys().map(CompactString::as_str).collect();
                let (sql, params) = Self::insert_sql(
                    table,
                    &keys,
                    std::slice::from_ref(row),
                    target,
                    &types,
                    false,
                );
                self.execute(&sql, &params).await?;
            }
        }
        Ok(summary(table, data.len() as u64, None))
    }

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

fn iso(dt: chrono::DateTime<chrono::Utc>) -> DbValue {
    DbValue::Text(dt.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn row_to_rowmap(row: &PgRow) -> RowMap {
    let columns = row.columns();
    let mut map = RowMap::with_capacity(columns.len());
    for (i, col) in columns.iter().enumerate() {
        macro_rules! get {
            ($t:ty, $f:expr) => {
                row.try_get::<Option<$t>, _>(i)
                    .ok()
                    .flatten()
                    .map_or(DbValue::Null, $f)
            };
        }
        let val = match col.type_info().name() {
            "BOOL" => get!(bool, DbValue::Bool),
            "INT2" => get!(i16, |v| DbValue::Int(i128::from(v))),
            "INT4" => get!(i32, |v| DbValue::Int(i128::from(v))),
            "INT8" => get!(i64, |v| DbValue::Int(i128::from(v))),
            "FLOAT4" => get!(f32, |v| DbValue::Float(f64::from(v))),
            "FLOAT8" => get!(f64, DbValue::Float),
            "NUMERIC" => get!(rust_decimal::Decimal, DbValue::Decimal),
            "BYTEA" => get!(Vec<u8>, DbValue::Bytes),
            "JSON" | "JSONB" => get!(serde_json::Value, DbValue::Json),
            "TIMESTAMPTZ" => get!(chrono::DateTime<chrono::Utc>, iso),
            "TIMESTAMP" => get!(chrono::NaiveDateTime, |v| iso(v.and_utc())),
            "DATE" => get!(chrono::NaiveDate, |v| DbValue::Text(v.to_string())),
            "TIME" => get!(chrono::NaiveTime, |v| DbValue::Text(v.to_string())),
            "UUID" => get!(sqlx::types::Uuid, |v| DbValue::Text(v.to_string())),
            // ponytail: other exotic types (inet, interval, arrays) decode only if text-compatible; else null.
            _ => get!(String, DbValue::Text),
        };
        map.insert(CompactString::new(col.name()), val);
    }
    map
}

#[async_trait]
impl DatabaseAdapter for PostgresAdapter {
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
        let types = self.column_types(table).await?;
        let w = compile_where_typed(D, filter, safe_delete, 0, Some(&types));
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
        let types = self.column_types(table).await?;
        let w = compile_where_typed(D, filter, safe_delete, 0, Some(&types));
        let sql = format!(
            "SELECT COUNT(*) AS c FROM {}{}",
            D.quote_identifier(table),
            w.clause
        );
        let rows = self.fetch_all(&sql, &w.params).await?;
        let c: i64 = rows
            .first()
            .map_or(Ok(0), |r| r.try_get(0))
            .map_err(|e| AppError::Database(format!("PostgreSQL count error: {e}")))?;
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
        let types = self.column_types(table).await?;
        let w = compile_where_typed(D, filter, safe_delete, 0, Some(&types));
        let sql = match safe_delete {
            Some(sd) => {
                let flag = if types.get(sd).is_some_and(|t| t == "boolean") {
                    "TRUE"
                } else {
                    "1"
                };
                format!(
                    "UPDATE {} SET {} = {flag}{}",
                    D.quote_identifier(table),
                    D.quote_identifier(sd),
                    w.clause
                )
            }
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

    /// Text bound to a timestamp column gets the same `CAST` as everywhere else, so
    /// RFC 3339 text (`2026-11-16T10:20:30.123Z`) lands in TIMESTAMP/TIMESTAMPTZ.
    async fn update_where(
        &self,
        table: &str,
        set: &RowMap,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64> {
        let types = self.column_types(table).await?;
        match compile_update(D, table, set, filter, safe_delete, Some(&types)) {
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

    async fn distinct_values(
        &self,
        table: &str,
        column: &str,
        safe_delete: Option<&str>,
        filter: Option<&FilterExpr>,
        search: Option<&str>,
        limit: usize,
    ) -> Result<Vec<DbValue>> {
        let types = self.column_types(table).await?;
        let empty = FilterExpr::empty();
        let f = filter.unwrap_or(&empty);
        let (sql, params) = compile_distinct(
            D,
            table,
            column,
            f,
            safe_delete,
            search,
            limit,
            Some(&types),
        );
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
impl TransactionAdapter for PostgresAdapter {
    async fn commit(&self) -> Result<()> {
        let Conn::Tx(t) = &self.conn else {
            return Ok(());
        };
        if let Some(tx) = t.lock().await.take() {
            tx.commit().await.map_err(|e| {
                AppError::Database(format!("PostgreSQL transaction commit error: {e}"))
            })?;
        }
        Ok(())
    }

    async fn rollback(&self) -> Result<()> {
        let Conn::Tx(t) = &self.conn else {
            return Ok(());
        };
        if let Some(tx) = t.lock().await.take() {
            tx.rollback().await.map_err(|e| {
                AppError::Database(format!("PostgreSQL transaction rollback error: {e}"))
            })?;
        }
        Ok(())
    }

    fn as_adapter(self: Arc<Self>) -> Arc<dyn DatabaseAdapter> {
        self
    }
}

fn bind_all<'a>(
    mut q: sqlx::query::Query<'a, Postgres, PgArguments>,
    params: &'a [DbValue],
) -> sqlx::query::Query<'a, Postgres, PgArguments> {
    for v in params {
        q = match v {
            DbValue::Null => q.bind(None::<String>),
            DbValue::Bool(b) => q.bind(*b),
            DbValue::Int(i) => match int_or_text(*i) {
                Ok(n) => q.bind(n),
                Err(s) => q.bind(s),
            },
            DbValue::Float(f) => q.bind(*f),
            DbValue::Decimal(d) => q.bind(*d),
            DbValue::Text(s) => q.bind(s.as_str()),
            DbValue::Bytes(b) => q.bind(b.as_slice()),
            DbValue::Json(j) => q.bind(j),
        };
    }
    q
}
