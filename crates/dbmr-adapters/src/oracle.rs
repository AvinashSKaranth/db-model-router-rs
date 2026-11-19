use crate::compiler::{
    compile_order_by, compile_update, compile_where, format_bulk_saved_message,
    format_remove_message, Slots, SqlDialect,
};
use async_trait::async_trait;
use compact_str::CompactString;
use dbmr_core::adapter::{DatabaseAdapter, PoolConfig, TransactionAdapter};
use dbmr_core::error::{AppError, Result};
use dbmr_core::filter::FilterExpr;
use dbmr_core::types::{DbValue, FindResult, GetOptions, RowMap, SortKey, WriteSummary};
use oracle_rs::{
    BindParam, Config, Connection, LobData, LobValue, OracleType, Value as OracleValue,
};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use tokio::sync::{Mutex, OwnedMutexGuard};

struct Shared {
    slots: Slots<Connection>,
    config: Config,
    is_connected: AtomicBool,
    /// TIMESTAMP* column names (upper case) per table, for RFC 3339 write binds.
    ts_cols: RwLock<HashMap<String, Arc<HashSet<String>>>>,
}

/// A pooled connection held for a transaction's lifetime (oracle-rs never autocommits).
/// Dropped without commit/rollback, the connection is discarded (the server rolls the
/// session's work back) rather than returned to the pool mid-transaction.
struct TxConn {
    slot: OwnedMutexGuard<Option<Connection>>,
    done: bool,
}

impl Drop for TxConn {
    fn drop(&mut self) {
        if !self.done {
            *self.slot = None;
        }
    }
}

/// Oracle adapter over a small pool of oracle-rs connections. The same type serves as
/// the transaction-scoped adapter returned by [`DatabaseAdapter::begin_tx`].
#[derive(Clone)]
pub struct OracleAdapter {
    shared: Arc<Shared>,
    tx: Option<Arc<Mutex<Option<TxConn>>>>,
}

/// UTC session time zone, so a TIMESTAMP bound into a TIMESTAMP WITH TIME ZONE column
/// means UTC. (NLS format changes are not an option: oracle-rs fails to parse the
/// server's reply to `ALTER SESSION SET NLS_*`.)
const SESSION_SETUP: &str = "ALTER SESSION SET TIME_ZONE = '+00:00'";

async fn open(config: &Config, what: &str) -> Result<Connection> {
    let conn = Connection::connect_with_config(config.clone())
        .await
        .map_err(|e| AppError::Database(format!("Oracle {what} error: {e}")))?;
    conn.execute(SESSION_SETUP, &[])
        .await
        .map_err(|e| AppError::Database(format!("Oracle session setup error: {e}")))?;
    Ok(conn)
}

fn closed() -> AppError {
    AppError::Database("Transaction already closed".to_string())
}

/// A pool slot or the transaction's connection; derefs to [`Connection`]
/// (`get_conn` only hands out filled ones).
enum Pooled {
    Pool(OwnedMutexGuard<Option<Connection>>),
    Tx(OwnedMutexGuard<Option<TxConn>>),
}

impl std::ops::Deref for Pooled {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        let c = match self {
            Self::Pool(g) => g.as_ref(),
            Self::Tx(g) => g.as_ref().and_then(|t| t.slot.as_ref()),
        };
        match c {
            Some(c) => c,
            None => unreachable!("get_conn hands out filled connections only"),
        }
    }
}

impl Pooled {
    /// Error from a statement. A pooled (non-transaction) connection is discarded, so a
    /// session the server dropped is not handed out again; the next `acquire` reconnects.
    fn fail(&mut self, what: &str, e: impl std::fmt::Display) -> AppError {
        if let Self::Pool(g) = self {
            **g = None;
        }
        AppError::Database(format!("Oracle {what} error: {e}"))
    }
}

impl OracleAdapter {
    pub async fn connect(
        host: &str,
        port: u16,
        service_name: &str,
        username: &str,
        password: &str,
    ) -> Result<Arc<Self>> {
        Self::connect_with(
            host,
            port,
            service_name,
            username,
            password,
            PoolConfig::default(),
        )
        .await
    }

    /// [`Self::connect`] with explicit pool settings (`max_connections` connections,
    /// opened lazily after the first).
    pub async fn connect_with(
        host: &str,
        port: u16,
        service_name: &str,
        username: &str,
        password: &str,
        cfg: PoolConfig,
    ) -> Result<Arc<Self>> {
        let config = Config::new(host, port, service_name, username, password);
        let first = open(&config, "connection").await?;
        Ok(Arc::new(Self {
            shared: Arc::new(Shared {
                slots: Slots::new(&cfg, first),
                config,
                is_connected: AtomicBool::new(true),
                ts_cols: RwLock::default(),
            }),
            tx: None,
        }))
    }

    /// A healthy pooled connection (reconnecting a broken or empty slot).
    async fn acquire(&self) -> Result<OwnedMutexGuard<Option<Connection>>> {
        let mut guard = self.shared.slots.acquire().await?;
        // A dropped session is flagged `is_closed` while `state` can still read Ready.
        let broken = match &*guard {
            Some(c) => c.is_closed() || c.state().await != oracle_rs::ConnectionState::Ready,
            None => true,
        };
        if broken {
            *guard = Some(open(&self.shared.config, "reconnect").await?);
        }
        Ok(guard)
    }

    async fn get_conn(&self) -> Result<Pooled> {
        match &self.tx {
            Some(tx) => {
                let g = tx.clone().lock_owned().await;
                if g.as_ref().and_then(|t| t.slot.as_ref()).is_none() {
                    return Err(closed());
                }
                Ok(Pooled::Tx(g))
            }
            None => Ok(Pooled::Pool(self.acquire().await?)),
        }
    }

    /// Outside a transaction every write commits immediately.
    async fn autocommit(&self, conn: &Connection) -> Result<()> {
        if self.tx.is_none() {
            conn.commit()
                .await
                .map_err(|e| AppError::Database(format!("Oracle commit error: {e}")))?;
        }
        Ok(())
    }

    /// Run a DML statement, commit (outside a tx), return the affected-row count.
    async fn execute(&self, sql: &str, params: &[DbValue]) -> Result<u64> {
        self.execute_binds(sql, params.iter().map(dbvalue_to_oracle).collect())
            .await
    }

    async fn execute_binds(&self, sql: &str, oracle_params: Vec<OracleValue>) -> Result<u64> {
        let mut conn = self.get_conn().await?;
        let res = conn
            .execute(sql, &oracle_params)
            .await
            .map_err(|e| conn.fail("execute", e))?;
        self.autocommit(&conn).await?;
        Ok(res.rows_affected)
    }

    /// TIMESTAMP* columns of `table` (cached; unknown table → empty).
    // ponytail: USER_TAB_COLUMNS by bare table name; tables in other schemas get no
    // timestamp binds (RFC 3339 text then fails on their TIMESTAMP columns).
    async fn ts_columns(&self, table: &str) -> Result<Arc<HashSet<String>>> {
        if let Some(t) = self
            .shared
            .ts_cols
            .read()
            .ok()
            .and_then(|m| m.get(table).cloned())
        {
            return Ok(t);
        }
        let name = table.rsplit('.').next().unwrap_or(table).trim_matches('"');
        let rows = self
            .query(
                "SELECT COLUMN_NAME AS c FROM USER_TAB_COLUMNS \
                 WHERE TABLE_NAME = :1 AND DATA_TYPE LIKE 'TIMESTAMP%'",
                &[DbValue::Text(name.to_uppercase())],
            )
            .await?;
        let set: Arc<HashSet<String>> = Arc::new(
            rows.iter()
                .filter_map(|r| r.get("c").and_then(DbValue::as_str).map(str::to_string))
                .collect(),
        );
        if let Ok(mut m) = self.shared.ts_cols.write() {
            m.insert(table.to_string(), set.clone());
        }
        Ok(set)
    }

    /// Binds for a write: `vals[i]` belongs to column `cols[i]` (extra values, e.g. WHERE
    /// params, bind as-is). RFC 3339 text for a TIMESTAMP column binds as a timestamp;
    /// text columns keep it verbatim.
    async fn write_binds(
        &self,
        table: &str,
        cols: &[&str],
        vals: &[DbValue],
    ) -> Result<Vec<OracleValue>> {
        let ts = self.ts_columns(table).await?;
        Ok(vals
            .iter()
            .enumerate()
            .map(|(i, v)| {
                cols.get(i)
                    .filter(|c| ts.contains(&c.to_uppercase()))
                    .and_then(|_| rfc3339_timestamp(v))
                    .unwrap_or_else(|| dbvalue_to_oracle(v))
            })
            .collect())
    }

    /// End the transaction; the connection goes back to the pool on success.
    async fn finish(&self, commit: bool) -> Result<()> {
        let Some(tx) = &self.tx else { return Ok(()) };
        let Some(mut t) = tx.lock().await.take() else {
            return Ok(());
        };
        let conn = t.slot.as_ref().ok_or_else(closed)?;
        let res = if commit {
            conn.commit().await
        } else {
            conn.rollback().await
        };
        res.map_err(|e| AppError::Database(format!("Oracle transaction end error: {e}")))?;
        t.done = true;
        Ok(())
    }
}

/// RFC 3339 text → UTC `TIMESTAMP` bind value.
fn rfc3339_timestamp(v: &DbValue) -> Option<OracleValue> {
    use chrono::{Datelike, Timelike};
    let DbValue::Text(s) = v else { return None };
    let d = chrono::DateTime::parse_from_rfc3339(s).ok()?.naive_utc();
    Some(OracleValue::Timestamp(
        oracle_rs::types::OracleTimestamp::new(
            d.year(),
            d.month() as u8,
            d.day() as u8,
            d.hour() as u8,
            d.minute() as u8,
            d.second() as u8,
            d.nanosecond() / 1000,
        ),
    ))
}

fn dbvalue_to_oracle(val: &DbValue) -> OracleValue {
    match val {
        DbValue::Null => OracleValue::Null,
        // oracle-rs never completes a BOOLEAN bind; 1/0 works for NUMBER(1) flags and
        // for Oracle 23ai BOOLEAN columns alike.
        DbValue::Bool(b) => OracleValue::Integer(i64::from(*b)),
        #[allow(clippy::cast_possible_truncation)]
        DbValue::Int(i) => OracleValue::Integer(*i as i64),
        DbValue::Float(f) => OracleValue::Float(*f),
        DbValue::Decimal(d) => OracleValue::String(d.to_string()),
        DbValue::Text(s) => OracleValue::String(s.clone()),
        DbValue::Bytes(b) => OracleValue::Bytes(b.clone()),
        DbValue::Json(j) => OracleValue::Json(j.clone()),
    }
}

fn oracle_to_dbvalue(val: &OracleValue) -> DbValue {
    match val {
        OracleValue::Null => DbValue::Null,
        OracleValue::Boolean(b) => DbValue::Bool(*b),
        OracleValue::Integer(i) => DbValue::Int(i128::from(*i)),
        OracleValue::Float(f) => DbValue::Float(*f),
        OracleValue::String(s) => DbValue::Text(s.clone()),
        OracleValue::Bytes(b) => DbValue::Bytes(b.clone()),
        OracleValue::Json(j) => DbValue::Json(j.clone()),
        OracleValue::Number(n) => number_to_dbvalue(n.as_str()),
        OracleValue::Date(d) => iso_utc(d.year, d.month, d.day, (d.hour, d.minute, d.second), 0),
        // TIMESTAMP WITH TIME ZONE arrives already in UTC.
        OracleValue::Timestamp(t) => iso_utc(
            t.year,
            t.month,
            t.day,
            (t.hour, t.minute, t.second),
            t.microsecond,
        ),
        other => DbValue::Text(format!("{other:?}")),
    }
}

/// NUMBER digits (oracle-rs returns query NUMBERs as text) → `Int`, else `Decimal`.
fn number_to_dbvalue(s: &str) -> DbValue {
    if let Ok(i) = s.parse::<i128>() {
        DbValue::Int(i)
    } else if let Ok(d) = s.parse::<rust_decimal::Decimal>() {
        DbValue::Decimal(d)
    } else {
        DbValue::Text(s.to_string())
    }
}

/// Same ISO-8601 UTC text as the PostgreSQL adapter.
fn iso_utc(year: i32, month: u8, day: u8, (h, m, sec): (u8, u8, u8), micros: u32) -> DbValue {
    chrono::NaiveDate::from_ymd_opt(year, month.into(), day.into())
        .and_then(|d| d.and_hms_micro_opt(h.into(), m.into(), sec.into(), micros))
        .map_or(DbValue::Null, |dt| {
            DbValue::Text(
                dt.and_utc()
                    .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            )
        })
}

fn query_result_to_rowmaps(res: &oracle_rs::QueryResult) -> Vec<RowMap> {
    let col_names: Vec<String> = res.columns.iter().map(|c| c.name.to_lowercase()).collect();
    res.rows
        .iter()
        .map(|row| {
            let mut map = RowMap::new();
            for (idx, name) in col_names.iter().enumerate() {
                let val = match (row.get(idx), &res.columns[idx].oracle_type) {
                    (Some(OracleValue::String(s)), OracleType::Number) => number_to_dbvalue(s),
                    (Some(v), _) => oracle_to_dbvalue(v),
                    (None, _) => DbValue::Null,
                };
                map.insert(CompactString::new(name), val);
            }
            map
        })
        .collect()
}

#[async_trait]
impl DatabaseAdapter for OracleAdapter {
    async fn disconnect(&self) -> Result<()> {
        self.shared.is_connected.store(false, Ordering::Relaxed);
        Ok(())
    }

    async fn is_connected(&self) -> bool {
        match &self.tx {
            Some(tx) => tx.lock().await.is_some(),
            None => self.shared.is_connected.load(Ordering::Relaxed),
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
        let dialect = SqlDialect::Oracle;
        let where_comp = compile_where(dialect, filter, safe_delete, 0);

        let q_str = match (options.limit, options.offset) {
            (Some(l), Some(o)) => {
                let order_clause = if sort.is_empty() {
                    " ORDER BY NULL".to_string()
                } else {
                    compile_order_by(dialect, sort)
                };
                format!(
                    "SELECT * FROM {}{}{} OFFSET {o} ROWS FETCH NEXT {l} ROWS ONLY",
                    dialect.quote_identifier(table),
                    where_comp.clause,
                    order_clause
                )
            }
            (Some(l), None) => {
                let order_clause = compile_order_by(dialect, sort);
                format!(
                    "SELECT * FROM {}{}{} FETCH FIRST {l} ROWS ONLY",
                    dialect.quote_identifier(table),
                    where_comp.clause,
                    order_clause
                )
            }
            (None, _) => {
                let order_clause = compile_order_by(dialect, sort);
                format!(
                    "SELECT * FROM {}{}{}",
                    dialect.quote_identifier(table),
                    where_comp.clause,
                    order_clause
                )
            }
        };

        let rows = self.query(&q_str, &where_comp.params).await?;
        let count = rows.len() as u64;
        Ok(FindResult { data: rows, count })
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
        let total = self.count(table, filter, safe_delete).await?;
        let options = GetOptions {
            limit: Some(size),
            offset: Some(page * size),
            count: false,
        };
        let mut res = self.get(table, filter, sort, safe_delete, options).await?;
        res.count = total;
        Ok(res)
    }

    async fn count(
        &self,
        table: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64> {
        let dialect = SqlDialect::Oracle;
        let where_comp = compile_where(dialect, filter, safe_delete, 0);
        let q_str = format!(
            "SELECT COUNT(*) AS \"COUNT\" FROM {}{}",
            dialect.quote_identifier(table),
            where_comp.clause
        );

        let rows = self.query(&q_str, &where_comp.params).await?;
        if let Some(first) = rows.first() {
            if let Some(val) = first.get("count").or_else(|| first.values().next()) {
                match val {
                    DbValue::Int(c) => {
                        #[allow(clippy::cast_sign_loss)]
                        return Ok(*c as u64);
                    }
                    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                    DbValue::Float(f) => return Ok(*f as u64),
                    DbValue::Text(s) => {
                        if let Ok(i) = s.parse::<u64>() {
                            return Ok(i);
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(0)
    }

    async fn insert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        _unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        if data.is_empty() {
            return Ok(WriteSummary {
                rows: 0,
                message: format_bulk_saved_message(table, 0),
                id: None,
                record: None,
            });
        }

        let dialect = SqlDialect::Oracle;
        let mut record = None;

        for row in data {
            let cols: Vec<String> = row
                .keys()
                .map(|k| dialect.quote_identifier(k.as_str()))
                .collect();
            let mut params = Vec::new();
            let mut placeholders = Vec::new();

            for (idx, val) in row.values().enumerate() {
                placeholders.push(format!(":{}", idx + 1));
                params.push(val.clone());
            }

            let q_str = format!(
                "INSERT INTO {} ({}) VALUES ({})",
                dialect.quote_identifier(table),
                cols.join(", "),
                placeholders.join(", ")
            );

            let keys: Vec<&str> = row.keys().map(CompactString::as_str).collect();
            let values = self.write_binds(table, &keys, &params).await?;
            if data.len() > 1 {
                self.execute_binds(&q_str, values).await?;
                continue;
            }
            // Single row: capture the ROWID so the stored record (with its generated key,
            // whatever the PK column is called) can be returned.
            let mut binds: Vec<BindParam> = values.into_iter().map(BindParam::input).collect();
            binds.push(BindParam::output(OracleType::Varchar, 64));
            let plsql = format!("BEGIN {q_str} RETURNING ROWID INTO :{}; END;", binds.len());
            let mut conn = self.get_conn().await?;
            let res = conn
                .execute_plsql(&plsql, &binds)
                .await
                .map_err(|e| conn.fail("execute", e))?;
            self.autocommit(&conn).await?;
            drop(conn);
            if let Some(OracleValue::String(rowid)) = res.out_values.first() {
                let sel = format!(
                    "SELECT * FROM {} WHERE ROWID = CHARTOROWID(:1)",
                    dialect.quote_identifier(table)
                );
                record = self
                    .query(&sel, &[DbValue::Text(rowid.clone())])
                    .await?
                    .into_iter()
                    .next();
            }
        }

        let count = data.len() as u64;
        Ok(WriteSummary {
            rows: count,
            message: format_bulk_saved_message(table, count),
            id: record.as_ref().and_then(|r| r.get("id").cloned()),
            record,
        })
    }

    async fn upsert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        if data.is_empty() {
            return Ok(WriteSummary {
                rows: 0,
                message: format_bulk_saved_message(table, 0),
                id: None,
                record: None,
            });
        }

        let unique_cols = unique
            .first()
            .cloned()
            .unwrap_or_else(|| vec!["id".to_string()]);
        let dialect = SqlDialect::Oracle;

        for row in data {
            // S7: all target columns AND-ed; a partial key is a plain insert.
            let key: Option<Vec<_>> = unique_cols
                .iter()
                .map(|k| {
                    row.get(k.as_str()).map(|v| dbmr_core::filter::Condition {
                        column: CompactString::new(k),
                        operator: dbmr_core::filter::Operator::Eq,
                        value: v.clone(),
                    })
                })
                .collect();
            let filter_expr = FilterExpr {
                groups: key.into_iter().collect(),
            };

            let existing = if filter_expr.groups.is_empty() {
                0
            } else {
                self.count(table, &filter_expr, None).await?
            };
            if existing > 0 {
                // Update
                let non_key_pairs: Vec<(&CompactString, &DbValue)> = row
                    .iter()
                    .filter(|(k, _)| !unique_cols.contains(&k.to_string()))
                    .collect();

                if !non_key_pairs.is_empty() {
                    let mut set_parts = Vec::new();
                    let mut params = Vec::new();

                    for (idx, (k, val)) in non_key_pairs.iter().enumerate() {
                        set_parts.push(format!(
                            "{} = :{}",
                            dialect.quote_identifier(k.as_str()),
                            idx + 1
                        ));
                        params.push((*val).clone());
                    }

                    let where_comp = compile_where(dialect, &filter_expr, None, params.len());
                    for p in where_comp.params {
                        params.push(p);
                    }

                    let q_str = format!(
                        "UPDATE {} SET {}{}",
                        dialect.quote_identifier(table),
                        set_parts.join(", "),
                        where_comp.clause
                    );
                    let keys: Vec<&str> = non_key_pairs.iter().map(|(k, _)| k.as_str()).collect();
                    let binds = self.write_binds(table, &keys, &params).await?;
                    self.execute_binds(&q_str, binds).await?;
                }
            } else {
                // Insert
                self.insert_rows(table, std::slice::from_ref(row), unique)
                    .await?;
            }
        }

        let count = data.len() as u64;
        Ok(WriteSummary {
            rows: count,
            message: format_bulk_saved_message(table, count),
            id: None,
            record: None,
        })
    }

    async fn remove_rows(
        &self,
        table: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<WriteSummary> {
        let dialect = SqlDialect::Oracle;
        let where_comp = compile_where(dialect, filter, safe_delete, 0);

        let sql = match safe_delete {
            Some(sd) => format!(
                "UPDATE {} SET {} = 1{}",
                dialect.quote_identifier(table),
                dialect.quote_identifier(sd),
                where_comp.clause
            ),
            None => format!(
                "DELETE FROM {}{}",
                dialect.quote_identifier(table),
                where_comp.clause
            ),
        };
        let rows = self.execute(&sql, &where_comp.params).await?;
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
        let Some((sql, params)) =
            compile_update(SqlDialect::Oracle, table, set, filter, safe_delete, None)
        else {
            return Ok(0);
        };
        let keys: Vec<&str> = set.keys().map(CompactString::as_str).collect();
        let binds = self.write_binds(table, &keys, &params).await?;
        self.execute_binds(&sql, binds).await
    }

    async fn ping(&self) -> Result<()> {
        self.query("SELECT 1 FROM DUAL", &[]).await.map(|_| ())
    }

    async fn query(&self, sql: &str, params: &[DbValue]) -> Result<Vec<RowMap>> {
        // Oracle has no RELEASE SAVEPOINT; core's `Transaction` sends it on savepoint commit.
        if sql.trim_start().starts_with("RELEASE SAVEPOINT ") {
            return Ok(Vec::new());
        }
        let mut conn = self.get_conn().await?;
        let oracle_params: Vec<OracleValue> = params.iter().map(dbvalue_to_oracle).collect();

        let trimmed = sql.trim_start();
        let upper = trimmed.to_uppercase();
        if upper.starts_with("SELECT") || upper.starts_with("WITH") {
            let mut res = conn
                .query(sql, &oracle_params)
                .await
                .map_err(|e| conn.fail("query", e))?;
            // CLOB/BLOB columns arrive as locators; read them while the connection is held.
            for row in &mut res.rows {
                let mut values =
                    std::mem::replace(row, oracle_rs::Row::new(Vec::new())).into_values();
                for v in &mut values {
                    if let OracleValue::Lob(LobValue::Locator(loc)) = v {
                        *v = match conn
                            .read_lob(loc)
                            .await
                            .map_err(|e| conn.fail("LOB read", e))?
                        {
                            LobData::String(s) => OracleValue::String(s),
                            LobData::Bytes(b) => OracleValue::Bytes(b.to_vec()),
                        };
                    }
                }
                *row = oracle_rs::Row::new(values);
            }

            Ok(query_result_to_rowmaps(&res))
        } else {
            conn.execute(sql, &oracle_params)
                .await
                .map_err(|e| conn.fail("execute", e))?;
            if !upper.starts_with("CREATE")
                && !upper.starts_with("DROP")
                && !upper.starts_with("ALTER")
                && !upper.starts_with("BEGIN")
            {
                self.autocommit(&conn).await?;
            }
            Ok(Vec::new())
        }
    }

    fn escape_id(&self, name: &str) -> String {
        SqlDialect::Oracle.quote_identifier(name)
    }

    async fn begin_tx(&self) -> Result<Arc<dyn TransactionAdapter>> {
        if self.tx.is_some() {
            return Err(AppError::Internal(
                "Transaction already open; use Transaction::savepoint for nesting.".into(),
            ));
        }
        let slot = self.acquire().await?;
        Ok(Arc::new(Self {
            shared: self.shared.clone(),
            tx: Some(Arc::new(Mutex::new(Some(TxConn { slot, done: false })))),
        }))
    }
}

#[async_trait]
impl TransactionAdapter for OracleAdapter {
    async fn commit(&self) -> Result<()> {
        self.finish(true).await
    }

    async fn rollback(&self) -> Result<()> {
        self.finish(false).await
    }

    fn as_adapter(self: Arc<Self>) -> Arc<dyn DatabaseAdapter> {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Variants oracle-rs 0.1.7 only produces for binds/PL-SQL, not for fetched rows.
    #[test]
    fn maps_every_oracle_value_kind() {
        assert_eq!(
            oracle_to_dbvalue(&OracleValue::Boolean(true)),
            DbValue::Bool(true)
        );
        assert_eq!(
            oracle_to_dbvalue(&OracleValue::Integer(-3)),
            DbValue::Int(-3)
        );
        assert_eq!(
            oracle_to_dbvalue(&OracleValue::Float(0.5)),
            DbValue::Float(0.5)
        );
        let n = oracle_rs::types::OracleNumber::new("12.50");
        assert_eq!(
            oracle_to_dbvalue(&OracleValue::Number(n)),
            DbValue::Decimal(rust_decimal::Decimal::new(1250, 2))
        );
        assert_eq!(
            oracle_to_dbvalue(&OracleValue::Lob(LobValue::Empty)),
            DbValue::Text("Lob(Empty)".into())
        );
    }
}
