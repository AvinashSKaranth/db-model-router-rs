use crate::core::filter::{FilterExpr, Operator};
use crate::core::types::{DbValue, SortDirection, SortKey};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqlDialect {
    Sqlite,
    Postgres,
    Mysql,
    Mssql,
    Oracle,
}

impl SqlDialect {
    /// Quote an identifier. `schema.table` is quoted per part; NUL bytes are removed and
    /// the dialect's closing quote character is doubled (S2), so a column name can never
    /// break out of its quotes.
    #[must_use]
    pub fn quote_identifier(&self, ident: &str) -> String {
        ident
            .replace('\0', "")
            .split('.')
            .map(|part| {
                let part = part.trim();
                match self {
                    Self::Postgres | Self::Sqlite => format!("\"{}\"", part.replace('"', "\"\"")),
                    Self::Oracle => format!("\"{}\"", part.to_uppercase().replace('"', "\"\"")),
                    Self::Mysql => format!("`{}`", part.replace('`', "``")),
                    Self::Mssql => format!("[{}]", part.replace(']', "]]")),
                }
            })
            .collect::<Vec<_>>()
            .join(".")
    }
}

pub struct CompiledWhere {
    pub clause: String,
    pub params: Vec<DbValue>,
}

/// PostgreSQL column types (`format_type` output) keyed by column name; used to cast
/// text/null parameters so `"age" > '30'` and `NULL` into an `integer` column work.
pub type ColumnTypes = HashMap<String, String>;

/// Types that accept a TEXT parameter without a cast.
fn is_texty(ty: &str) -> bool {
    ty == "text"
        || ty == "name"
        || ty == "citext"
        || ty.starts_with("character")
        || ty.starts_with("varchar")
        || ty.starts_with("char")
}

/// PostgreSQL: a number bound to a typed (non-text, non-boolean) column is sent as text
/// and cast to the column type. sqlx caches a prepared statement by its SQL text with the
/// parameter types of the first call, so binding `9.5` (float8) and later `9` (int8) to
/// the same `price = $1` silently matched nothing. Text + cast gives every call the same
/// parameter type.
fn pg_number_as_text(ty: &str, value: &DbValue) -> bool {
    !is_texty(ty)
        && ty != "boolean"
        && matches!(
            value,
            DbValue::Int(_) | DbValue::Float(_) | DbValue::Decimal(_)
        )
}

/// Placeholder for parameter `idx`, wrapped in a cast for PostgreSQL when the column has a
/// known non-text type and the value is text, null or a number.
#[must_use]
pub fn placeholder(
    dialect: SqlDialect,
    idx: usize,
    column: &str,
    value: &DbValue,
    types: Option<&ColumnTypes>,
) -> String {
    let ph = format_placeholder(dialect, idx);
    if dialect != SqlDialect::Postgres {
        return ph;
    }
    match types.and_then(|t| t.get(column)) {
        Some(ty)
            if !is_texty(ty)
                && (matches!(value, DbValue::Text(_) | DbValue::Null | DbValue::Json(_))
                    || pg_number_as_text(ty, value)) =>
        {
            format!("CAST({ph} AS {ty})")
        }
        _ => ph,
    }
}

/// PostgreSQL: an integer bound to a boolean column becomes a boolean (Node coercion).
#[must_use]
pub fn coerce_value(
    dialect: SqlDialect,
    column: &str,
    value: &DbValue,
    types: Option<&ColumnTypes>,
) -> DbValue {
    if dialect == SqlDialect::Postgres {
        let ty = types.and_then(|t| t.get(column)).map(String::as_str);
        if let (Some("boolean"), DbValue::Int(i)) = (ty, value) {
            return DbValue::Bool(*i != 0);
        }
        if ty.is_some_and(|t| pg_number_as_text(t, value)) {
            return DbValue::Text(value.to_string());
        }
    }
    value.clone()
}

fn soft_delete_check(dialect: SqlDialect, col: &str, types: Option<&ColumnTypes>) -> String {
    let q = dialect.quote_identifier(col);
    let is_bool = types
        .and_then(|t| t.get(col))
        .is_some_and(|t| t == "boolean");
    if is_bool {
        format!("{q} = FALSE")
    } else {
        format!("{q} = 0")
    }
}

#[must_use]
pub fn compile_where(
    dialect: SqlDialect,
    filter: &FilterExpr,
    safe_delete: Option<&str>,
    param_offset: usize,
) -> CompiledWhere {
    compile_where_typed(dialect, filter, safe_delete, param_offset, None)
}

#[must_use]
pub fn compile_where_typed(
    dialect: SqlDialect,
    filter: &FilterExpr,
    safe_delete: Option<&str>,
    param_offset: usize,
    types: Option<&ColumnTypes>,
) -> CompiledWhere {
    let mut params = Vec::new();
    let mut param_idx = param_offset + 1;
    let mut group_sql_parts = Vec::new();
    let push = |col: &str, v: &DbValue, params: &mut Vec<DbValue>, idx: &mut usize| {
        let ph = placeholder(dialect, *idx, col, v, types);
        *idx += 1;
        params.push(coerce_value(dialect, col, v, types));
        ph
    };

    for group in filter.groups.iter().filter(|g| !g.is_empty()) {
        let mut cond_sql_parts = Vec::new();

        for cond in group {
            let name = cond.column.as_str();
            let col = dialect.quote_identifier(name);

            match (&cond.operator, &cond.value) {
                (Operator::IsNull, _) | (Operator::Eq, DbValue::Null) => {
                    cond_sql_parts.push(format!("{col} IS NULL"));
                }
                (Operator::IsNotNull, _) | (Operator::NotEq, DbValue::Null) => {
                    cond_sql_parts.push(format!("{col} IS NOT NULL"));
                }
                (Operator::In | Operator::NotIn, v) => {
                    // Scalars are treated as a one-element list (S8: never dropped).
                    let items: Vec<DbValue> = match v {
                        DbValue::Json(serde_json::Value::Array(a)) => {
                            a.iter().cloned().map(DbValue::from).collect()
                        }
                        other => vec![other.clone()],
                    };
                    let is_in = cond.operator == Operator::In;
                    if items.is_empty() {
                        cond_sql_parts.push(if is_in { "1=0" } else { "1=1" }.to_string());
                    } else {
                        let phs: Vec<String> = items
                            .iter()
                            .map(|item| push(name, item, &mut params, &mut param_idx))
                            .collect();
                        let op = if is_in { "IN" } else { "NOT IN" };
                        cond_sql_parts.push(format!("{col} {op} ({})", phs.join(", ")));
                    }
                }
                (Operator::Like | Operator::NotLike, v) => {
                    // Node: the term is always wrapped as %term% (contains match).
                    let term = match v {
                        DbValue::Null => String::new(),
                        other => other.to_string(),
                    };
                    let negate = cond.operator == Operator::NotLike;
                    let (lhs, op) = match dialect {
                        SqlDialect::Postgres => (
                            format!("CAST({col} AS TEXT)"),
                            if negate { "NOT ILIKE" } else { "ILIKE" },
                        ),
                        // Oracle LIKE is case-sensitive; match the other dialects.
                        SqlDialect::Oracle => (
                            format!("UPPER({col})"),
                            if negate { "NOT LIKE" } else { "LIKE" },
                        ),
                        _ => (col.clone(), if negate { "NOT LIKE" } else { "LIKE" }),
                    };
                    let term = if dialect == SqlDialect::Oracle {
                        term.to_uppercase()
                    } else {
                        term
                    };
                    let ph = format_placeholder(dialect, param_idx);
                    param_idx += 1;
                    params.push(DbValue::Text(format!("%{term}%")));
                    cond_sql_parts.push(format!("{lhs} {op} {ph}"));
                }
                (op, v) => {
                    let ph = push(name, v, &mut params, &mut param_idx);
                    let op = if *op == Operator::NotEq && dialect == SqlDialect::Mssql {
                        "<>".to_string()
                    } else {
                        op.to_string()
                    };
                    cond_sql_parts.push(format!("{col} {op} {ph}"));
                }
            }
        }

        if let Some(sd) = safe_delete {
            cond_sql_parts.push(soft_delete_check(dialect, sd, types));
        }
        group_sql_parts.push(format!("({})", cond_sql_parts.join(" AND ")));
    }

    if group_sql_parts.is_empty() {
        let clause = safe_delete.map_or_else(String::new, |sd| {
            format!(" WHERE {}", soft_delete_check(dialect, sd, types))
        });
        return CompiledWhere { clause, params };
    }

    CompiledWhere {
        clause: format!(" WHERE {}", group_sql_parts.join(" OR ")),
        params,
    }
}

/// `UPDATE <table> SET a = p1, b = p2<where>` for `update_where`: SET values bind first,
/// then the filter's; the soft-delete predicate is the same one reads use. `None` when
/// `set` is empty.
#[must_use]
pub fn compile_update(
    dialect: SqlDialect,
    table: &str,
    set: &crate::core::types::RowMap,
    filter: &FilterExpr,
    safe_delete: Option<&str>,
    types: Option<&ColumnTypes>,
) -> Option<(String, Vec<DbValue>)> {
    if set.is_empty() {
        return None;
    }
    let mut params = Vec::with_capacity(set.len());
    let assigns: Vec<String> = set
        .iter()
        .map(|(k, v)| {
            params.push(coerce_value(dialect, k, v, types));
            let ph = placeholder(dialect, params.len(), k, v, types);
            format!("{} = {ph}", dialect.quote_identifier(k))
        })
        .collect();
    let w = compile_where_typed(dialect, filter, safe_delete, params.len(), types);
    params.extend(w.params);
    Some((
        format!(
            "UPDATE {} SET {}{}",
            dialect.quote_identifier(table),
            assigns.join(", "),
            w.clause
        ),
        params,
    ))
}

/// Fixed-size connection slots for drivers without a pool (MSSQL, Oracle). Slots start
/// empty and are connected lazily by the caller; a broken connection is reset to `None`.
// ponytail: try_lock scan, then wait on a round-robin slot (even if another frees first);
// ceiling is max_connections in-flight queries. Use bb8/deadpool if fairness matters.
#[cfg(any(feature = "mssql", feature = "oracle"))]
pub struct Slots<T> {
    conns: Vec<std::sync::Arc<tokio::sync::Mutex<Option<T>>>>,
    next: std::sync::atomic::AtomicUsize,
    timeout: std::time::Duration,
}

#[cfg(any(feature = "mssql", feature = "oracle"))]
impl<T> Slots<T> {
    /// `first` (the connection that validated the settings) fills slot 0.
    #[must_use]
    pub fn new(cfg: &crate::core::adapter::PoolConfig, first: T) -> Self {
        let n = cfg.max_connections.max(1) as usize;
        let mut conns = Vec::with_capacity(n);
        conns.push(std::sync::Arc::new(tokio::sync::Mutex::new(Some(first))));
        conns.resize_with(n, || std::sync::Arc::new(tokio::sync::Mutex::new(None)));
        Self {
            conns,
            next: std::sync::atomic::AtomicUsize::new(0),
            timeout: cfg.acquire_timeout,
        }
    }

    /// A free slot, or wait (up to `acquire_timeout`) for a busy one.
    pub async fn acquire(
        &self,
    ) -> crate::core::error::Result<tokio::sync::OwnedMutexGuard<Option<T>>> {
        for s in &self.conns {
            if let Ok(g) = s.clone().try_lock_owned() {
                return Ok(g);
            }
        }
        let i = self.next.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % self.conns.len();
        tokio::time::timeout(self.timeout, self.conns[i].clone().lock_owned())
            .await
            .map_err(|_| {
                crate::core::error::AppError::Database(
                    "Timed out acquiring a database connection".into(),
                )
            })
    }
}

#[must_use]
pub fn format_placeholder(dialect: SqlDialect, idx: usize) -> String {
    match dialect {
        SqlDialect::Postgres => format!("${idx}"),
        SqlDialect::Oracle => format!(":{idx}"),
        SqlDialect::Sqlite | SqlDialect::Mysql => "?".to_string(),
        SqlDialect::Mssql => format!("@P{idx}"),
    }
}

#[must_use]
pub fn compile_order_by(dialect: SqlDialect, sort: &[SortKey]) -> String {
    if sort.is_empty() {
        return String::new();
    }
    let parts: Vec<String> = sort
        .iter()
        .filter(|k| !k.column.is_empty())
        .map(|k| {
            let col = dialect.quote_identifier(k.column.as_str());
            let dir = match k.direction {
                SortDirection::Asc => "ASC",
                SortDirection::Desc => "DESC",
            };
            format!("{col} {dir}")
        })
        .collect();
    if parts.is_empty() {
        return String::new();
    }
    format!(" ORDER BY {}", parts.join(", "))
}

/// Node parity helper: replace '_' with ' ', capitalize each word boundary.
#[must_use]
pub fn namify(text: &str) -> String {
    let replaced = text.replace('_', " ");
    let mut result = String::with_capacity(replaced.len());
    let mut capitalize_next = true;
    for ch in replaced.chars() {
        if ch.is_alphanumeric() {
            if capitalize_next {
                result.extend(ch.to_uppercase());
                capitalize_next = false;
            } else {
                result.push(ch);
            }
        } else {
            result.push(ch);
            capitalize_next = true;
        }
    }
    result
}

/// Exact remove string contract (§27, §37.2): `"N <table>[s] removed"`
#[must_use]
pub fn format_remove_message(table: &str, count: u64) -> String {
    if count > 1 {
        format!("{count} {table}s removed")
    } else {
        format!("{count} {table} removed")
    }
}

/// Exact saved string contract (§27, P4-09): `"1 <namified> is saved"`, `"N <namified>s are saved"`
#[must_use]
pub fn format_bulk_saved_message(table: &str, count: u64) -> String {
    let named = namify(table);
    if count == 1 {
        format!("1 {named} is saved")
    } else {
        format!("{count} {named}s are saved")
    }
}

/// Write summary; `record`/`id` only for a single row.
#[must_use]
pub fn summary(
    table: &str,
    count: u64,
    last: Option<crate::core::types::RowMap>,
) -> crate::core::types::WriteSummary {
    let record = if count == 1 { last } else { None };
    crate::core::types::WriteSummary {
        rows: count,
        message: format_bulk_saved_message(table, count),
        id: record.as_ref().and_then(|r| r.get("id").cloned()),
        record,
    }
}

/// i128 that fits i64 binds as an integer; larger values bind as their decimal text.
#[must_use]
pub fn int_or_text(i: i128) -> Result<i64, String> {
    i64::try_from(i).map_err(|_| i.to_string())
}

/// Total order for client-side sorting (Redis, DynamoDB): by type first
/// (null < bool < number < text < bytes < json), then by value; numbers compare
/// numerically across Int/Float/Decimal. `sort_by` may panic on an inconsistent order,
/// so mixed-type columns must not fall back to "equal" or string comparison.
// ponytail: Int vs Float goes through f64, so ints beyond 2^53 tie with a nearby float.
#[cfg(any(feature = "redis", feature = "dynamodb"))]
#[must_use]
pub fn sort_order(a: &DbValue, b: &DbValue) -> std::cmp::Ordering {
    fn rank(v: &DbValue) -> u8 {
        match v {
            DbValue::Null => 0,
            DbValue::Bool(_) => 1,
            DbValue::Int(_) | DbValue::Float(_) | DbValue::Decimal(_) => 2,
            DbValue::Text(_) => 3,
            DbValue::Bytes(_) => 4,
            DbValue::Json(_) => 5,
        }
    }
    fn num(v: &DbValue) -> f64 {
        match v {
            #[allow(clippy::cast_precision_loss)]
            DbValue::Int(i) => *i as f64,
            DbValue::Float(f) => *f,
            DbValue::Decimal(d) => d.to_string().parse().unwrap_or(f64::NAN),
            _ => f64::NAN,
        }
    }
    match (a, b) {
        (DbValue::Int(x), DbValue::Int(y)) => x.cmp(y),
        (DbValue::Bool(x), DbValue::Bool(y)) => x.cmp(y),
        (DbValue::Text(x), DbValue::Text(y)) => x.cmp(y),
        (DbValue::Bytes(x), DbValue::Bytes(y)) => x.cmp(y),
        (DbValue::Json(x), DbValue::Json(y)) => x.to_string().cmp(&y.to_string()),
        _ if rank(a) == 2 && rank(b) == 2 => num(a).total_cmp(&num(b)),
        _ => rank(a).cmp(&rank(b)),
    }
}

/// Key-value upsert (Redis, DynamoDB): when `row` supplies every column of a unique key and
/// a stored row matches all of them, return the stored row overlaid with `row` (so its key
/// is kept); otherwise `row` unchanged, which the caller inserts.
///
/// # Errors
/// Database errors from the lookup.
#[cfg(any(feature = "redis", feature = "dynamodb"))]
pub async fn merge_existing(
    db: &dyn crate::core::adapter::DatabaseAdapter,
    table: &str,
    row: &crate::core::types::RowMap,
    unique: &[Vec<String>],
) -> crate::core::error::Result<crate::core::types::RowMap> {
    for key in unique {
        let mut cols = key.iter();
        let Some(first) = cols.next() else { continue };
        let val = |c: &str| row.get(c).filter(|v| !v.is_null()).cloned();
        let Some(v) = val(first) else { continue };
        let mut filter = FilterExpr::eq(first, v);
        let mut complete = true;
        for c in cols {
            match val(c) {
                Some(v) => filter = filter.and(c, Operator::Eq, v),
                None => complete = false,
            }
        }
        if !complete {
            continue;
        }
        let opts = crate::core::types::GetOptions {
            limit: Some(1),
            ..Default::default()
        };
        if let Some(mut existing) = db.get(table, &filter, &[], None, opts).await?.data.pop() {
            for (k, v) in row {
                existing.insert(k.clone(), v.clone());
            }
            return Ok(existing);
        }
    }
    Ok(row.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::filter::condition;

    #[cfg(any(feature = "redis", feature = "dynamodb"))]
    #[test]
    fn sort_order_is_total_across_types() {
        use std::cmp::Ordering;
        let vals = [
            DbValue::Text("9".into()),
            DbValue::Int(10),
            DbValue::Null,
            DbValue::Float(9.5),
            DbValue::Int(9),
            DbValue::Bool(true),
            DbValue::Text("10".into()),
            DbValue::Json(serde_json::json!({"a": 1})),
        ];
        // Antisymmetric and transitive over every triple: the property sort_by relies on.
        for a in &vals {
            for b in &vals {
                assert_eq!(sort_order(a, b), sort_order(b, a).reverse());
                for c in &vals {
                    if sort_order(a, b) != Ordering::Greater
                        && sort_order(b, c) != Ordering::Greater
                    {
                        assert_ne!(sort_order(a, c), Ordering::Greater, "{a:?} {b:?} {c:?}");
                    }
                }
            }
        }
        let mut v = vals.to_vec();
        v.sort_by(sort_order);
        assert_eq!(v[0], DbValue::Null);
        assert_eq!(
            &v[2..5],
            &[DbValue::Int(9), DbValue::Float(9.5), DbValue::Int(10)]
        );
    }

    #[test]
    fn quoting_cannot_be_escaped() {
        assert_eq!(SqlDialect::Postgres.quote_identifier("a\"b"), "\"a\"\"b\"");
        assert_eq!(SqlDialect::Mysql.quote_identifier("a`b"), "`a``b`");
        assert_eq!(SqlDialect::Mssql.quote_identifier("a]b"), "[a]]b]");
        assert_eq!(SqlDialect::Sqlite.quote_identifier("s.t"), "\"s\".\"t\"");
    }

    #[test]
    fn where_semantics() {
        let f = FilterExpr {
            groups: vec![vec![
                condition("a", Operator::In, DbValue::from(5)),
                condition("b", Operator::NotEq, DbValue::Null),
                condition("c", Operator::Like, DbValue::from("jo")),
            ]],
        };
        let w = compile_where(SqlDialect::Sqlite, &f, Some("del"), 0);
        assert_eq!(
            w.clause,
            " WHERE (\"a\" IN (?) AND \"b\" IS NOT NULL AND \"c\" LIKE ? AND \"del\" = 0)"
        );
        assert_eq!(w.params, vec![DbValue::Int(5), DbValue::from("%jo%")]);

        let mut types = ColumnTypes::new();
        types.insert("a".into(), "integer".into());
        let w = compile_where_typed(
            SqlDialect::Postgres,
            &FilterExpr::eq("a", "30"),
            None,
            0,
            Some(&types),
        );
        assert_eq!(w.clause, " WHERE (\"a\" = CAST($1 AS integer))");
    }

    #[test]
    fn update_binds_set_then_filter() {
        let mut set = crate::core::types::RowMap::new();
        set.insert("name".into(), DbValue::from("x"));
        set.insert("version".into(), DbValue::Int(2));
        let f = FilterExpr::eq("id", 1).and("version", Operator::Eq, 1);
        let Some((sql, params)) =
            compile_update(SqlDialect::Mssql, "t", &set, &f, Some("del"), None)
        else {
            panic!("non-empty set compiles")
        };
        assert_eq!(
            sql,
            "UPDATE [t] SET [name] = @P1, [version] = @P2 WHERE ([id] = @P3 AND [version] = @P4 AND [del] = 0)"
        );
        assert_eq!(
            params,
            vec![
                DbValue::from("x"),
                DbValue::Int(2),
                DbValue::Int(1),
                DbValue::Int(1)
            ]
        );
        let empty = crate::core::types::RowMap::new();
        assert!(compile_update(SqlDialect::Sqlite, "t", &empty, &f, None, None).is_none());
    }

    #[test]
    fn list_operators_null_like_and_blank_sort() {
        let f = FilterExpr {
            groups: vec![vec![
                // A raw scalar (not normalised by `condition`) is a one-element list.
                crate::core::filter::Condition {
                    column: "a".into(),
                    operator: Operator::NotIn,
                    value: DbValue::Int(1),
                },
                condition("b", Operator::NotLike, DbValue::Null),
            ]],
        };
        let w = compile_where(SqlDialect::Mysql, &f, None, 0);
        assert_eq!(w.clause, " WHERE (`a` NOT IN (?) AND `b` NOT LIKE ?)");
        assert_eq!(w.params, vec![DbValue::Int(1), DbValue::from("%%")]);
        let blank = [SortKey {
            column: "".into(),
            direction: SortDirection::Asc,
        }];
        assert_eq!(compile_order_by(SqlDialect::Mssql, &blank), "");
    }
}
