use crate::adapters::compiler::{format_bulk_saved_message, format_remove_message};
use crate::core::adapter::DatabaseAdapter;
use crate::core::error::{AppError, Result};
use crate::core::filter::{Condition, FilterExpr, Operator};
use crate::core::types::{
    DbValue, FindResult, GetOptions, RowMap, SortDirection, SortKey, WriteSummary,
};
use async_trait::async_trait;
use compact_str::CompactString;
use redis::AsyncCommands;
use std::cmp::Ordering;
use std::sync::Arc;

#[derive(Clone)]
pub struct RedisAdapter {
    client: redis::Client,
}

impl RedisAdapter {
    pub fn connect(url: &str) -> Result<Arc<Self>> {
        let client = redis::Client::open(url)
            .map_err(|e| AppError::Database(format!("Invalid Redis URL: {e}")))?;
        Ok(Arc::new(Self { client }))
    }

    pub fn new(client: redis::Client) -> Self {
        Self { client }
    }

    async fn get_conn(&self) -> Result<redis::aio::MultiplexedConnection> {
        self.client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| AppError::Database(format!("Redis connection failed: {e}")))
    }
}

/// Hash fields are type-tagged (`s:007`, `i:7`, `f:1.5`, `d:9.00`, `b:true`, `j:{..}`;
/// `""` is NULL) so values read back with the type they were written with: text that
/// looks like a number stays text.
fn to_hash_value(v: &DbValue) -> String {
    match v {
        DbValue::Null => String::new(),
        DbValue::Bool(b) => format!("b:{b}"),
        DbValue::Int(i) => format!("i:{i}"),
        DbValue::Float(f) => format!("f:{f}"),
        DbValue::Decimal(d) => format!("d:{d}"),
        DbValue::Text(t) => format!("s:{t}"),
        DbValue::Bytes(b) => format!("s:{}", String::from_utf8_lossy(b)),
        DbValue::Json(j) => format!("j:{j}"),
    }
}

fn from_hash_value(v: &str) -> DbValue {
    let Some((tag, body)) = v.split_once(':') else {
        return if v.is_empty() {
            DbValue::Null
        } else {
            DbValue::Text(v.to_string())
        };
    };
    let text = || DbValue::Text(body.to_string());
    match tag {
        "s" => text(),
        "b" => DbValue::Bool(body == "true"),
        "i" => body.parse().map_or_else(|_| text(), DbValue::Int),
        "f" => body.parse().map_or_else(|_| text(), DbValue::Float),
        "d" => body.parse().map_or_else(|_| text(), DbValue::Decimal),
        "j" => serde_json::from_str(body).map_or_else(|_| text(), DbValue::Json),
        _ => DbValue::Text(v.to_string()),
    }
}

fn rowmap_to_hash(row: &RowMap) -> Vec<(String, String)> {
    row.iter()
        .map(|(k, v)| (k.to_string(), to_hash_value(v)))
        .collect()
}

/// Compare the stored fields that matched the filter before mutating one hash.
/// ARGV: n, n x (field, observed value or empty for absent), delete flag, field/value pairs.
const CAS_SCRIPT: &str = r"
if redis.call('EXISTS', KEYS[1]) == 0 then return 0 end
local n = tonumber(ARGV[1])
for i = 0, n - 1 do
  local v = redis.call('HGET', KEYS[1], ARGV[2 + 2 * i]) or ''
  if v ~= ARGV[3 + 2 * i] then return 0 end
end
if ARGV[2 + 2 * n] == '1' then return redis.call('DEL', KEYS[1]) end
redis.call('HSET', KEYS[1], unpack(ARGV, 3 + 2 * n))
return 1
";

const INSERT_SCRIPT: &str = r"
if redis.call('EXISTS', KEYS[1]) ~= 0 then return 0 end
redis.call('HSET', KEYS[1], unpack(ARGV))
return 1
";

fn guarded_command(
    key: &str,
    row: &RowMap,
    filter: &FilterExpr,
    soft_delete: Option<&str>,
    delete: bool,
) -> Option<redis::Cmd> {
    let group = filter
        .groups
        .iter()
        .find(|g| g.iter().all(|c| matches_condition(row, c)));
    if group.is_none() && !filter.is_empty() {
        return None;
    }
    let fields: std::collections::BTreeSet<&str> = group
        .into_iter()
        .flatten()
        .map(|c| c.column.as_str())
        .chain(soft_delete)
        .collect();
    let mut cmd = redis::cmd("EVAL");
    cmd.arg(CAS_SCRIPT).arg(1).arg(key).arg(fields.len());
    for col in fields {
        cmd.arg(col)
            .arg(row.get(col).map_or_else(String::new, to_hash_value));
    }
    cmd.arg(u8::from(delete));
    Some(cmd)
}

fn hash_to_rowmap(pairs: &std::collections::HashMap<String, String>) -> RowMap {
    pairs
        .iter()
        .map(|(k, v)| (CompactString::new(k), from_hash_value(v)))
        .collect()
}

/// Orders same-kind values; Int and Float compare numerically. Other pairs are unordered.
fn compare(a: &DbValue, b: &DbValue) -> Option<Ordering> {
    match (a, b) {
        (DbValue::Int(x), DbValue::Int(y)) => Some(x.cmp(y)),
        (DbValue::Text(x), DbValue::Text(y)) => Some(x.cmp(y)),
        (DbValue::Float(x), DbValue::Float(y)) => x.partial_cmp(y),
        (DbValue::Int(x), DbValue::Float(y)) => (*x as f64).partial_cmp(y),
        (DbValue::Float(x), DbValue::Int(y)) => x.partial_cmp(&(*y as f64)),
        _ => None,
    }
}

/// Redis stores text, so a numeric-looking string reads back as a number: compare
/// number vs text by their string form.
fn loose_eq(a: &DbValue, b: &DbValue) -> bool {
    let num_str = |v: &DbValue| match v {
        DbValue::Int(i) => Some(i.to_string()),
        DbValue::Float(f) => Some(f.to_string()),
        _ => None,
    };
    match (a, b) {
        (DbValue::Text(t), n) | (n, DbValue::Text(t)) if num_str(n).is_some() => {
            num_str(n).as_deref() == Some(t.as_str())
        }
        _ => a == b || compare(a, b).is_some_and(Ordering::is_eq),
    }
}

fn matches_condition(row: &RowMap, cond: &Condition) -> bool {
    let Some(val) = row.get(&cond.column) else {
        return cond.operator == Operator::IsNull;
    };

    match cond.operator {
        Operator::Eq => loose_eq(val, &cond.value),
        Operator::NotEq => !loose_eq(val, &cond.value),
        Operator::Gt => compare(val, &cond.value).is_some_and(Ordering::is_gt),
        Operator::Lt => compare(val, &cond.value).is_some_and(Ordering::is_lt),
        Operator::Gte => compare(val, &cond.value).is_some_and(Ordering::is_ge),
        Operator::Lte => compare(val, &cond.value).is_some_and(Ordering::is_le),
        Operator::Like => {
            if let (Some(s), Some(pat)) = (val.as_str(), cond.value.as_str()) {
                let clean_pat = pat.trim_matches('%').to_lowercase();
                s.to_lowercase().contains(&clean_pat)
            } else {
                false
            }
        }
        Operator::NotLike => {
            if let (Some(s), Some(pat)) = (val.as_str(), cond.value.as_str()) {
                let clean_pat = pat.trim_matches('%').to_lowercase();
                !s.to_lowercase().contains(&clean_pat)
            } else {
                true
            }
        }
        Operator::In => {
            if let DbValue::Json(serde_json::Value::Array(arr)) = &cond.value {
                arr.iter()
                    .any(|item| loose_eq(val, &DbValue::from(item.clone())))
            } else {
                false
            }
        }
        Operator::NotIn => !matches_condition(
            row,
            &Condition {
                column: cond.column.clone(),
                operator: Operator::In,
                value: cond.value.clone(),
            },
        ),
        Operator::IsNull => val.is_null(),
        Operator::IsNotNull => !val.is_null(),
    }
}

fn matches_filter(row: &RowMap, filter: &FilterExpr, safe_delete: Option<&str>) -> bool {
    if let Some(sd) = safe_delete {
        if let Some(v) = row.get(sd) {
            if matches!(v, DbValue::Bool(true) | DbValue::Int(1)) {
                return false;
            }
        }
    }

    if filter.is_empty() {
        return true;
    }

    // OR-groups of AND-conditions
    filter
        .groups
        .iter()
        .any(|group| group.iter().all(|cond| matches_condition(row, cond)))
}

impl RedisAdapter {
    async fn write_rows(
        &self,
        table: &str,
        data: &[RowMap],
        insert_only: bool,
    ) -> Result<WriteSummary> {
        if data.is_empty() {
            return Ok(WriteSummary {
                rows: 0,
                message: format_bulk_saved_message(table, 0),
                id: None,
                record: None,
            });
        }

        let mut conn = self.get_conn().await?;
        let mut last_id = None;
        let mut last_record = None;

        for row in data {
            let mut row = row.clone();
            let id = match row.get("id") {
                Some(id) if !id.is_null() => id.clone(),
                // Generated ids come from a per-table counter (outside the `{table}:*`
                // row key space), so bulk inserts never collide.
                _ => {
                    let n: i64 = conn
                        .incr(format!("__seq:{table}"), 1)
                        .await
                        .map_err(|e| AppError::Database(format!("Redis incr error: {e}")))?;
                    let id = DbValue::Int(n.into());
                    row.insert("id".into(), id.clone());
                    id
                }
            };
            let key = format!("{table}:{id}");
            let pairs = rowmap_to_hash(&row);
            if insert_only {
                let mut cmd = redis::cmd("EVAL");
                cmd.arg(INSERT_SCRIPT).arg(1).arg(&key);
                for (field, value) in pairs {
                    cmd.arg(field).arg(value);
                }
                let inserted: i64 = cmd
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| AppError::Database(format!("Redis hset error: {e}")))?;
                if inserted == 0 {
                    return Err(AppError::Conflict("Primary key already exists.".into()));
                }
            } else {
                let () = conn
                    .hset_multiple(&key, &pairs)
                    .await
                    .map_err(|e| AppError::Database(format!("Redis hset error: {e}")))?;
            }

            last_id = Some(id);
            last_record = Some(row);
        }

        let count = data.len() as u64;
        Ok(WriteSummary {
            rows: count,
            message: format_bulk_saved_message(table, count),
            id: if count == 1 { last_id } else { None },
            record: if count == 1 { last_record } else { None },
        })
    }
}

#[async_trait]
impl DatabaseAdapter for RedisAdapter {
    async fn disconnect(&self) -> Result<()> {
        Ok(())
    }

    async fn is_connected(&self) -> bool {
        let Ok(mut conn) = self.get_conn().await else {
            return false;
        };
        redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .is_ok()
    }

    async fn get(
        &self,
        table: &str,
        filter: &FilterExpr,
        sort: &[SortKey],
        safe_delete: Option<&str>,
        options: GetOptions,
    ) -> Result<FindResult> {
        let mut res = self
            .list(table, filter, sort, safe_delete, 0, u64::MAX)
            .await?;
        let skip = usize::try_from(options.offset.unwrap_or(0)).unwrap_or(usize::MAX);
        let take = usize::try_from(options.limit.unwrap_or(u64::MAX)).unwrap_or(usize::MAX);
        res.data = res.data.into_iter().skip(skip).take(take).collect();
        if !options.count {
            res.count = res.data.len() as u64;
        }
        Ok(res)
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
        let mut conn = self.get_conn().await?;
        // ponytail: KEYS + full scan per query, O(table); fine for dev/cache-sized tables,
        // switch to SCAN + secondary index sets if Redis holds large tables.
        let pattern = format!("{table}:*");
        let keys: Vec<String> = conn
            .keys(&pattern)
            .await
            .map_err(|e| AppError::Database(format!("Redis keys error: {e}")))?;

        let mut rows = Vec::new();
        for key in &keys {
            let pairs: std::collections::HashMap<String, String> = conn
                .hgetall(key)
                .await
                .map_err(|e| AppError::Database(format!("Redis hgetall error: {e}")))?;
            if !pairs.is_empty() {
                let row = hash_to_rowmap(&pairs);
                if matches_filter(&row, filter, safe_delete) {
                    rows.push(row);
                }
            }
        }

        // Apply sorting
        if let Some(k) = sort.first() {
            rows.sort_by(|a, b| {
                let va = a.get(&k.column).cloned().unwrap_or(DbValue::Null);
                let vb = b.get(&k.column).cloned().unwrap_or(DbValue::Null);
                let ord = crate::adapters::compiler::sort_order(&va, &vb);
                if k.direction == SortDirection::Desc {
                    ord.reverse()
                } else {
                    ord
                }
            });
        }

        let total = rows.len() as u64;
        let offset = usize::try_from(page.saturating_mul(size)).unwrap_or(usize::MAX);
        let limit = size as usize;

        let paged: Vec<RowMap> = rows.into_iter().skip(offset).take(limit).collect();
        Ok(FindResult {
            data: paged,
            count: total,
        })
    }

    async fn count(
        &self,
        table: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64> {
        let res = self
            .list(table, filter, &[], safe_delete, 0, u64::MAX)
            .await?;
        Ok(res.count)
    }

    async fn insert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        _unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        self.write_rows(table, data, true).await
    }

    async fn upsert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        let mut rows = Vec::with_capacity(data.len());
        for row in data {
            rows.push(crate::adapters::compiler::merge_existing(self, table, row, unique).await?);
        }
        self.write_rows(table, &rows, false).await
    }

    async fn remove_rows(
        &self,
        table: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<WriteSummary> {
        let res = self
            .list(table, filter, &[], safe_delete, 0, u64::MAX)
            .await?;
        let mut conn = self.get_conn().await?;
        let mut deleted = 0;

        for row in &res.data {
            if let Some(id) = row.get("id") {
                let key = format!("{table}:{id}");
                let Some(mut cmd) =
                    guarded_command(&key, row, filter, safe_delete, safe_delete.is_none())
                else {
                    continue;
                };
                if let Some(sd) = safe_delete {
                    cmd.arg(sd).arg(to_hash_value(&DbValue::Bool(true)));
                }
                let changes: i64 = cmd
                    .query_async(&mut conn)
                    .await
                    .map_err(|e| AppError::Database(format!("Redis delete error: {e}")))?;
                if changes > 0 {
                    deleted += 1;
                }
            }
        }

        Ok(WriteSummary {
            rows: deleted,
            message: format_remove_message(table, deleted),
            id: None,
            record: None,
        })
    }

    /// Compare the fields of the matched group and the soft-delete flag atomically with
    /// the update, including fields matched by list-valued scopes.
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
        let res = self
            .list(table, filter, &[], safe_delete, 0, u64::MAX)
            .await?;
        let mut conn = self.get_conn().await?;
        let mut changed = 0;
        for row in &res.data {
            let Some(id) = row.get("id") else { continue };
            let Some(mut cmd) =
                guarded_command(&format!("{table}:{id}"), row, filter, safe_delete, false)
            else {
                continue;
            };
            for (k, v) in rowmap_to_hash(set) {
                cmd.arg(k).arg(v);
            }
            let n: i64 = cmd
                .query_async(&mut conn)
                .await
                .map_err(|e| AppError::Database(format!("Redis update error: {e}")))?;
            changed += u64::from(n == 1);
        }
        Ok(changed)
    }

    async fn ping(&self) -> Result<()> {
        let mut conn = self.get_conn().await?;
        redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
            .map(|_| ())
            .map_err(|e| AppError::Database(format!("Redis ping error: {e}")))
    }

    async fn query(&self, _sql: &str, _params: &[DbValue]) -> Result<Vec<RowMap>> {
        Err(AppError::Database(
            "Raw SQL queries not supported on Redis adapter".to_string(),
        ))
    }

    fn escape_id(&self, name: &str) -> String {
        name.to_string()
    }
}
