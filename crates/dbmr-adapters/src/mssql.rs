use crate::compiler::{
    compile_order_by, compile_update, compile_where, format_bulk_saved_message,
    format_remove_message, Slots, SqlDialect,
};
use async_trait::async_trait;
use chrono::SecondsFormat;
use compact_str::CompactString;
use dbmr_core::adapter::{DatabaseAdapter, PoolConfig, TransactionAdapter};
use dbmr_core::error::{AppError, Result};
use dbmr_core::filter::FilterExpr;
use dbmr_core::types::{DbValue, FindResult, GetOptions, RowMap, SortKey, WriteSummary};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tiberius::{AuthMethod, Client, ColumnData, Config, FromSql, Row, ToSql};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, OwnedMutexGuard};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

type MssqlClient = Client<Compat<TcpStream>>;
type Slot = OwnedMutexGuard<Option<MssqlClient>>;

struct Shared {
    config: Config,
    slots: Slots<MssqlClient>,
    connected: AtomicBool,
}

/// A pooled connection held for a transaction's lifetime. Dropped without
/// commit/rollback, the connection is discarded (closing it rolls the server side back)
/// rather than returned to the pool mid-transaction.
struct TxConn {
    slot: Slot,
    done: bool,
}

impl Drop for TxConn {
    fn drop(&mut self) {
        if !self.done {
            *self.slot = None;
        }
    }
}

/// SQL Server adapter over a small pool of tiberius connections. The same type serves as
/// the transaction-scoped adapter returned by [`DatabaseAdapter::begin_tx`].
#[derive(Clone)]
pub struct MssqlAdapter {
    shared: Arc<Shared>,
    tx: Option<Arc<Mutex<Option<TxConn>>>>,
}

enum Out {
    Rows(Vec<RowMap>),
    Affected(u64),
}

async fn open(config: &Config) -> Result<MssqlClient> {
    let addr = config.get_addr();
    let tcp = TcpStream::connect(&addr)
        .await
        .map_err(|e| AppError::Database(format!("Failed to connect to MSSQL {addr}: {e}")))?;
    tcp.set_nodelay(true)
        .map_err(|e| AppError::Database(format!("Failed to set nodelay: {e}")))?;
    Client::connect(config.clone(), tcp.compat_write())
        .await
        .map_err(|e| AppError::Database(format!("MSSQL handshake failed: {e}")))
}

fn query_err(e: &tiberius::error::Error) -> AppError {
    AppError::Database(format!("MSSQL query error: {e}"))
}

fn closed() -> AppError {
    AppError::Database("Transaction already closed".to_string())
}

/// `query` or `execute` (affected-row count) on one connection.
async fn op(
    client: &mut MssqlClient,
    sql: &str,
    params: &[DbValue],
    affected: bool,
) -> std::result::Result<Out, tiberius::error::Error> {
    let boxed: Vec<Box<dyn ToSql + Sync>> = params.iter().map(to_sql_param).collect();
    let refs: Vec<&dyn ToSql> = boxed.iter().map(|b| &**b as &dyn ToSql).collect();
    if affected {
        Ok(Out::Affected(client.execute(sql, &refs[..]).await?.total()))
    } else {
        let rows = client
            .query(sql, &refs[..])
            .await?
            .into_first_result()
            .await?;
        Ok(Out::Rows(rows.iter().map(row_to_rowmap).collect()))
    }
}

/// A plain batch (not `sp_executesql`): BEGIN/COMMIT must not run inside an RPC call.
async fn batch(client: &mut MssqlClient, sql: &str) -> Result<()> {
    client
        .simple_query(sql)
        .await
        .map_err(|e| query_err(&e))?
        .into_results()
        .await
        .map_err(|e| query_err(&e))?;
    Ok(())
}

/// Core's `Transaction` speaks ANSI savepoints; T-SQL spells them `SAVE`/`ROLLBACK TRAN`
/// and has no release. `None` = no-op.
fn tsql_savepoint(sql: &str) -> Option<String> {
    if let Some(n) = sql.strip_prefix("ROLLBACK TO SAVEPOINT ") {
        Some(format!("ROLLBACK TRANSACTION {n}"))
    } else if let Some(n) = sql.strip_prefix("SAVEPOINT ") {
        Some(format!("SAVE TRANSACTION {n}"))
    } else if sql.starts_with("RELEASE SAVEPOINT ") {
        None
    } else {
        Some(sql.to_string())
    }
}

impl MssqlAdapter {
    /// Connect with an ADO (`server=tcp:host,1433;...`) or JDBC connection string.
    /// The server certificate is verified unless the string sets
    /// `TrustServerCertificate=true` (dev only).
    pub async fn connect(url: &str) -> Result<Arc<Self>> {
        Self::connect_with(url, PoolConfig::default()).await
    }

    /// [`Self::connect`] with explicit pool settings (`max_connections` connections,
    /// opened lazily after the first).
    pub async fn connect_with(url: &str, cfg: PoolConfig) -> Result<Arc<Self>> {
        let config = Config::from_jdbc_string(url)
            .or_else(|_| Config::from_ado_string(url))
            .map_err(|e| AppError::Database(format!("Invalid MSSQL connection string: {e}")))?;
        Self::from_config(config, &cfg).await
    }

    /// Connect by parts. `trust_cert = true` skips certificate verification (dev only).
    pub async fn connect_host(
        host: &str,
        port: u16,
        user: &str,
        pass: &str,
        database: &str,
        trust_cert: bool,
    ) -> Result<Arc<Self>> {
        let mut config = Config::new();
        config.host(host);
        config.port(port);
        config.authentication(AuthMethod::sql_server(user, pass));
        config.database(database);
        if trust_cert {
            config.trust_cert();
        }
        Self::from_config(config, &PoolConfig::default()).await
    }

    async fn from_config(config: Config, cfg: &PoolConfig) -> Result<Arc<Self>> {
        let first = open(&config).await?;
        Ok(Arc::new(Self {
            shared: Arc::new(Shared {
                config,
                slots: Slots::new(cfg, first),
                connected: AtomicBool::new(true),
            }),
            tx: None,
        }))
    }

    /// A pooled connection, (re)opened if its slot is empty.
    async fn acquire(&self) -> Result<Slot> {
        let mut slot = self.shared.slots.acquire().await?;
        if slot.is_none() {
            *slot = Some(open(&self.shared.config).await?);
        }
        Ok(slot)
    }

    async fn run(&self, sql: &str, params: &[DbValue], affected: bool) -> Result<Out> {
        if let Some(tx) = &self.tx {
            let mut g = tx.lock().await;
            let client = g
                .as_mut()
                .and_then(|c| c.slot.as_mut())
                .ok_or_else(closed)?;
            return op(client, sql, params, affected)
                .await
                .map_err(|e| query_err(&e));
        }
        let mut slot = self.acquire().await?;
        let client = slot.as_mut().ok_or_else(closed)?;
        let res = op(client, sql, params, affected).await;
        if let Err(tiberius::error::Error::Io { .. }) = &res {
            *slot = None; // broken: reconnect on next use
        }
        res.map_err(|e| query_err(&e))
    }

    async fn execute(&self, sql: &str, params: &[DbValue]) -> Result<u64> {
        match self.run(sql, params, true).await? {
            Out::Affected(n) => Ok(n),
            Out::Rows(_) => Ok(0),
        }
    }

    /// End the transaction with `sql`; the connection goes back to the pool on success.
    async fn finish(&self, sql: &str) -> Result<()> {
        let Some(tx) = &self.tx else { return Ok(()) };
        let Some(mut conn) = tx.lock().await.take() else {
            return Ok(());
        };
        let client = conn.slot.as_mut().ok_or_else(closed)?;
        batch(client, sql).await?;
        conn.done = true;
        Ok(())
    }
}

fn to_sql_param<'a>(val: &'a DbValue) -> Box<dyn ToSql + Sync + 'a> {
    match val {
        DbValue::Null => Box::new(Option::<&str>::None),
        DbValue::Bool(b) => Box::new(*b),
        #[allow(clippy::cast_possible_truncation)]
        DbValue::Int(i) => Box::new(*i as i64),
        DbValue::Float(f) => Box::new(*f),
        DbValue::Decimal(d) => Box::new(d.to_string()),
        DbValue::Text(s) => Box::new(s.as_str()),
        DbValue::Bytes(b) => Box::new(b.as_slice()),
        DbValue::Json(j) => Box::new(j.to_string()),
    }
}

fn row_to_rowmap(row: &Row) -> RowMap {
    let mut map = RowMap::new();
    for (col, data) in row.cells() {
        let val = match data {
            ColumnData::U8(Some(i)) => DbValue::Int(i128::from(*i)),
            ColumnData::I16(Some(i)) => DbValue::Int(i128::from(*i)),
            ColumnData::I32(Some(i)) => DbValue::Int(i128::from(*i)),
            ColumnData::I64(Some(i)) => DbValue::Int(i128::from(*i)),
            ColumnData::F32(Some(f)) => DbValue::Float(f64::from(*f)),
            ColumnData::F64(Some(f)) => DbValue::Float(*f),
            ColumnData::Bit(Some(b)) => DbValue::Bool(*b),
            ColumnData::String(Some(s)) => DbValue::Text(s.to_string()),
            ColumnData::Guid(Some(g)) => DbValue::Text(g.to_string()),
            ColumnData::Binary(Some(b)) => DbValue::Bytes(b.to_vec()),
            ColumnData::Xml(Some(x)) => DbValue::Text(x.to_string()),
            // Scale > 28 does not fit `Decimal`; keep the exact digits as text.
            ColumnData::Numeric(Some(n)) => {
                rust_decimal::Decimal::try_from_i128_with_scale(n.value(), u32::from(n.scale()))
                    .map_or_else(|_| DbValue::Text(n.to_string()), DbValue::Decimal)
            }
            // Same ISO-8601 UTC text as the PostgreSQL adapter.
            ColumnData::DateTime(Some(_))
            | ColumnData::SmallDateTime(Some(_))
            | ColumnData::DateTime2(Some(_)) => chrono::NaiveDateTime::from_sql(data)
                .ok()
                .flatten()
                .map_or(DbValue::Null, |v| {
                    DbValue::Text(v.and_utc().to_rfc3339_opts(SecondsFormat::Millis, true))
                }),
            ColumnData::DateTimeOffset(Some(_)) => chrono::DateTime::<chrono::Utc>::from_sql(data)
                .ok()
                .flatten()
                .map_or(DbValue::Null, |v| {
                    DbValue::Text(v.to_rfc3339_opts(SecondsFormat::Millis, true))
                }),
            ColumnData::Date(Some(_)) => chrono::NaiveDate::from_sql(data)
                .ok()
                .flatten()
                .map_or(DbValue::Null, |v| DbValue::Text(v.to_string())),
            ColumnData::Time(Some(_)) => chrono::NaiveTime::from_sql(data)
                .ok()
                .flatten()
                .map_or(DbValue::Null, |v| DbValue::Text(v.to_string())),
            _ => DbValue::Null,
        };
        map.insert(CompactString::new(col.name()), val);
    }
    map
}

#[async_trait]
impl DatabaseAdapter for MssqlAdapter {
    async fn disconnect(&self) -> Result<()> {
        self.shared.connected.store(false, Ordering::Relaxed);
        Ok(())
    }

    async fn is_connected(&self) -> bool {
        match &self.tx {
            Some(tx) => tx.lock().await.is_some(),
            None => self.shared.connected.load(Ordering::Relaxed),
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
        let dialect = SqlDialect::Mssql;
        let where_comp = compile_where(dialect, filter, safe_delete, 0);

        let q_str = match (options.limit, options.offset) {
            (Some(l), Some(o)) => {
                let order_clause = if sort.is_empty() {
                    " ORDER BY (SELECT NULL)".to_string()
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
                    "SELECT TOP ({l}) * FROM {}{}{}",
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
        let dialect = SqlDialect::Mssql;
        let where_comp = compile_where(dialect, filter, safe_delete, 0);
        let q_str = format!(
            "SELECT COUNT(*) AS [count] FROM {}{}",
            dialect.quote_identifier(table),
            where_comp.clause
        );

        let rows = self.query(&q_str, &where_comp.params).await?;
        if let Some(first) = rows.first() {
            if let Some(DbValue::Int(c)) = first.get("count") {
                #[allow(clippy::cast_sign_loss)]
                return Ok(*c as u64);
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

        let dialect = SqlDialect::Mssql;
        let mut last_id = None;

        for row in data {
            let cols: Vec<String> = row
                .keys()
                .map(|k| dialect.quote_identifier(k.as_str()))
                .collect();
            let mut params = Vec::new();
            let mut placeholders = Vec::new();

            for (idx, val) in row.values().enumerate() {
                placeholders.push(format!("@p{}", idx + 1));
                params.push(val.clone());
            }

            let q_str = format!(
                "INSERT INTO {} ({}) VALUES ({}); SELECT CAST(SCOPE_IDENTITY() AS BIGINT) AS [id];",
                dialect.quote_identifier(table),
                cols.join(", "),
                placeholders.join(", ")
            );

            let res = self.query(&q_str, &params).await?;
            if let Some(first) = res.first() {
                if let Some(DbValue::Int(id)) = first.get("id") {
                    if *id > 0 {
                        last_id = Some(DbValue::Int(*id));
                    }
                }
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
        let dialect = SqlDialect::Mssql;
        // IDENTITY columns can never be updated (error 8102), so leave them out of SET.
        let identity: Vec<String> = self
            .query(
                "SELECT name FROM sys.identity_columns WHERE object_id = OBJECT_ID(@p1)",
                &[DbValue::Text(table.to_string())],
            )
            .await?
            .iter()
            .filter_map(|r| {
                r.get("name")
                    .and_then(DbValue::as_str)
                    .map(str::to_lowercase)
            })
            .collect();

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
                    .filter(|(k, _)| {
                        !unique_cols.contains(&k.to_string())
                            && !identity.contains(&k.as_str().to_lowercase())
                    })
                    .collect();

                if !non_key_pairs.is_empty() {
                    let mut set_parts = Vec::new();
                    let mut params = Vec::new();

                    for (idx, (k, val)) in non_key_pairs.iter().enumerate() {
                        set_parts.push(format!(
                            "{} = @p{}",
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
                    self.query(&q_str, &params).await?;
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
        let dialect = SqlDialect::Mssql;
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

    /// DATETIME2/DATETIME columns accept RFC 3339 `...Z` text natively.
    async fn update_where(
        &self,
        table: &str,
        set: &RowMap,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64> {
        match compile_update(SqlDialect::Mssql, table, set, filter, safe_delete, None) {
            Some((sql, params)) => self.execute(&sql, &params).await,
            None => Ok(0),
        }
    }

    async fn query(&self, sql: &str, params: &[DbValue]) -> Result<Vec<RowMap>> {
        let sql = match (&self.tx, tsql_savepoint(sql.trim())) {
            (None, _) => sql.to_string(),
            (Some(_), Some(s)) => s,
            (Some(_), None) => return Ok(Vec::new()),
        };
        match self.run(&sql, params, false).await? {
            Out::Rows(rows) => Ok(rows),
            Out::Affected(_) => Ok(Vec::new()),
        }
    }

    fn escape_id(&self, name: &str) -> String {
        SqlDialect::Mssql.quote_identifier(name)
    }

    async fn begin_tx(&self) -> Result<Arc<dyn TransactionAdapter>> {
        if self.tx.is_some() {
            return Err(AppError::Internal(
                "Transaction already open; use Transaction::savepoint for nesting.".into(),
            ));
        }
        let mut slot = self.acquire().await?;
        let client = slot.as_mut().ok_or_else(closed)?;
        batch(client, "BEGIN TRANSACTION").await?;
        Ok(Arc::new(Self {
            shared: self.shared.clone(),
            tx: Some(Arc::new(Mutex::new(Some(TxConn { slot, done: false })))),
        }))
    }
}

#[async_trait]
impl TransactionAdapter for MssqlAdapter {
    async fn commit(&self) -> Result<()> {
        self.finish("COMMIT TRANSACTION").await
    }

    async fn rollback(&self) -> Result<()> {
        self.finish("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION").await
    }

    fn as_adapter(self: Arc<Self>) -> Arc<dyn DatabaseAdapter> {
        self
    }
}
