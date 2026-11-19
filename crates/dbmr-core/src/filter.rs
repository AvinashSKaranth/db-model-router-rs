use crate::error::{AppError, Result};
use crate::json::json_safe_parse;
use crate::types::{DbValue, SortDirection, SortKey};
use compact_str::CompactString;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Operator {
    Eq,
    NotEq,
    Gt,
    Lt,
    Gte,
    Lte,
    Like,
    NotLike,
    In,
    NotIn,
    IsNull,
    IsNotNull,
}

impl std::fmt::Display for Operator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Eq => write!(f, "="),
            Self::NotEq => write!(f, "!="),
            Self::Gt => write!(f, ">"),
            Self::Lt => write!(f, "<"),
            Self::Gte => write!(f, ">="),
            Self::Lte => write!(f, "<="),
            Self::Like => write!(f, "like"),
            Self::NotLike => write!(f, "not like"),
            Self::In => write!(f, "in"),
            Self::NotIn => write!(f, "not in"),
            Self::IsNull => write!(f, "is null"),
            Self::IsNotNull => write!(f, "is not null"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Condition {
    pub column: CompactString,
    pub operator: Operator,
    pub value: DbValue,
}

/// Filter structure: OR-groups containing AND-conditions.
/// `[ [ (A AND B) ] OR [ (C AND D) ] ]`
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct FilterExpr {
    pub groups: Vec<Vec<Condition>>,
}

impl FilterExpr {
    #[must_use]
    pub fn empty() -> Self {
        Self { groups: Vec::new() }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty() || self.groups.iter().all(Vec::is_empty)
    }

    /// Single-condition filter: `FilterExpr::eq("id", 5)`.
    #[must_use]
    pub fn eq(column: &str, value: impl Into<DbValue>) -> Self {
        Self::default().and(column, Operator::Eq, value)
    }

    /// AND a condition onto every OR-group (creating one group if none).
    #[must_use]
    pub fn and(mut self, column: &str, op: Operator, value: impl Into<DbValue>) -> Self {
        let c = condition(column, op, value.into());
        if self.groups.is_empty() {
            self.groups.push(Vec::new());
        }
        for g in &mut self.groups {
            g.push(c.clone());
        }
        self
    }

    /// AND every condition of `other` (one AND-group, e.g. a scope) onto every OR-group.
    #[must_use]
    pub fn and_expr(mut self, other: &FilterExpr) -> Self {
        for c in other.groups.iter().flatten() {
            self = self.and(&c.column, c.operator.clone(), c.value.clone());
        }
        self
    }

    /// OR another filter's groups onto this one.
    #[must_use]
    pub fn or(mut self, other: FilterExpr) -> Self {
        self.groups.retain(|g| !g.is_empty());
        self.groups
            .extend(other.groups.into_iter().filter(|g| !g.is_empty()));
        self
    }

    /// Parse a JSON criteria value with the HTTP/Node rules (§22): `{"age": ">30"}`,
    /// `[1,2]` (pk IN), `{"filter": [[["age", ">", 30]]]}`.
    ///
    /// # Errors
    /// [`AppError::UnprocessableEntity`] (`"Invalid filter Inputs"`).
    pub fn from_json(data: &Value, primary_key: &str) -> Result<Self> {
        data_to_filter(data, primary_key)
    }
}

/// Parses query string operators according to exact precedence (§22.4).
#[must_use]
pub fn parse_filter_value(val: &Value) -> (Operator, DbValue) {
    let Value::String(s) = val else {
        if val.is_null() {
            return (Operator::IsNull, DbValue::Null);
        }
        return (Operator::Eq, DbValue::from(val.clone()));
    };

    // Node does not trim filter values.
    let trimmed = s.as_str();
    let lower = trimmed.to_ascii_lowercase();
    let list = |inner: &str| -> DbValue {
        DbValue::Json(Value::Array(
            inner
                .split(',')
                .map(|v| Value::String(v.trim().to_string()))
                .collect(),
        ))
    };

    // 1. !in(a,b,...) (case-insensitive)
    if lower.starts_with("!in(") && trimmed.ends_with(')') && trimmed.len() > 5 {
        return (Operator::NotIn, list(&trimmed[4..trimmed.len() - 1]));
    }

    // 2. in(a,b,...)
    if lower.starts_with("in(") && trimmed.ends_with(')') && trimmed.len() > 4 {
        return (Operator::In, list(&trimmed[3..trimmed.len() - 1]));
    }

    // 3. ! and remainder contains % -> not like
    if trimmed.starts_with('!') && trimmed[1..].contains('%') {
        return (Operator::NotLike, DbValue::Text(trimmed[1..].to_string()));
    }

    // 4. contains % -> like
    if trimmed.contains('%') {
        return (Operator::Like, DbValue::Text(trimmed.to_string()));
    }

    // 5. >=
    if let Some(rest) = trimmed.strip_prefix(">=") {
        return (Operator::Gte, DbValue::Text(rest.to_string()));
    }

    // 6. <=
    if let Some(rest) = trimmed.strip_prefix("<=") {
        return (Operator::Lte, DbValue::Text(rest.to_string()));
    }

    // 7. >
    if let Some(rest) = trimmed.strip_prefix('>') {
        return (Operator::Gt, DbValue::Text(rest.to_string()));
    }

    // 8. <
    if let Some(rest) = trimmed.strip_prefix('<') {
        return (Operator::Lt, DbValue::Text(rest.to_string()));
    }

    // 9. ! -> !=
    if let Some(rest) = trimmed.strip_prefix('!') {
        return (Operator::NotEq, DbValue::Text(rest.to_string()));
    }

    // 10. Default =
    (Operator::Eq, DbValue::Text(trimmed.to_string()))
}

/// Converts a plain JSON object into a single AND group.
#[must_use]
pub fn object_to_filter(obj: &serde_json::Map<String, Value>) -> Vec<Condition> {
    let mut conditions = Vec::new();
    for (k, v) in obj {
        let (op, db_val) = parse_filter_value(v);
        conditions.push(condition(k, op, db_val));
    }
    conditions
}

/// Converts incoming user query/body payload into normalized FilterExpr.
///
/// # Errors
/// Returns [`AppError::UnprocessableEntity`] if filter payload is invalid.
pub fn data_to_filter(data: &Value, primary_key: &str) -> Result<FilterExpr> {
    if data.is_null() || data.as_str().is_some_and(str::is_empty) {
        return Err(AppError::UnprocessableEntity(
            "Invalid filter Inputs".to_string(),
        ));
    }

    let parsed = json_safe_parse(data);

    if let Some(s) = parsed.as_str() {
        return Ok(FilterExpr {
            groups: vec![vec![Condition {
                column: CompactString::new(primary_key),
                operator: Operator::Eq,
                value: DbValue::Text(s.to_string()),
            }]],
        });
    }

    if parsed.is_number() {
        return Ok(FilterExpr {
            groups: vec![vec![Condition {
                column: CompactString::new(primary_key),
                operator: Operator::Eq,
                value: DbValue::from(parsed.clone()),
            }]],
        });
    }

    if let Some(arr) = parsed.as_array() {
        if arr.is_empty() {
            return Ok(FilterExpr {
                groups: vec![vec![]],
            });
        }

        // Array of primitives -> IN (...)
        if arr.iter().all(|x| x.is_string() || x.is_number()) {
            return Ok(FilterExpr {
                groups: vec![vec![Condition {
                    column: CompactString::new(primary_key),
                    operator: Operator::In,
                    value: DbValue::Json(Value::Array(arr.clone())),
                }]],
            });
        }

        // Array of objects
        if arr.iter().all(Value::is_object) {
            let all_pk_only = arr.iter().all(|item| {
                item.as_object()
                    .is_some_and(|m| m.len() == 1 && m.contains_key(primary_key))
            });
            if all_pk_only {
                let ids: Vec<Value> = arr
                    .iter()
                    .filter_map(|item| item.as_object().and_then(|m| m.get(primary_key)).cloned())
                    .collect();
                return Ok(FilterExpr {
                    groups: vec![vec![Condition {
                        column: CompactString::new(primary_key),
                        operator: Operator::In,
                        value: DbValue::Json(Value::Array(ids)),
                    }]],
                });
            }

            // OR of groups
            let groups: Vec<Vec<Condition>> = arr
                .iter()
                .filter_map(Value::as_object)
                .map(object_to_filter)
                .collect();
            return Ok(FilterExpr { groups });
        }

        return Err(AppError::UnprocessableEntity(
            "Invalid filter Inputs".to_string(),
        ));
    }

    if let Some(map) = parsed.as_object() {
        if let Some(data_field) = map.get("data").and_then(Value::as_array) {
            // Keys next to `data` (e.g. the path id) still apply to every group.
            let mut f = data_to_filter(&Value::Array(data_field.clone()), primary_key)?;
            let mut rest = map.clone();
            rest.remove("data");
            for c in object_to_filter(&rest) {
                f = f.and(&c.column, c.operator, c.value);
            }
            return Ok(f);
        }

        if let Some(filter_field) = map.get("filter").and_then(Value::as_array) {
            let mut filter_expr = parse_raw_filter_array(filter_field)?;
            let mut rest = map.clone();
            rest.remove("filter");

            if !rest.is_empty() {
                let extra_conditions = object_to_filter(&rest);
                if filter_expr.groups.is_empty() {
                    filter_expr.groups.push(extra_conditions);
                } else {
                    for group in &mut filter_expr.groups {
                        group.extend(extra_conditions.clone());
                    }
                }
            }
            return Ok(filter_expr);
        }

        if map.is_empty() {
            return Ok(FilterExpr {
                groups: vec![vec![]],
            });
        }

        return Ok(FilterExpr {
            groups: vec![object_to_filter(map)],
        });
    }

    Err(AppError::UnprocessableEntity(
        "Invalid filter Inputs".to_string(),
    ))
}

/// Parse an operator token of the raw `[[["col","op",value]]]` form (case-insensitive).
///
/// # Errors
/// [`AppError::UnprocessableEntity`] for an unknown operator.
pub fn parse_operator(op: &str) -> Result<Operator> {
    Ok(match op.trim().to_ascii_lowercase().as_str() {
        "=" | "==" | "eq" => Operator::Eq,
        "!=" | "<>" | "ne" => Operator::NotEq,
        ">" | "gt" => Operator::Gt,
        "<" | "lt" => Operator::Lt,
        ">=" | "gte" => Operator::Gte,
        "<=" | "lte" => Operator::Lte,
        "like" => Operator::Like,
        "not like" => Operator::NotLike,
        "in" => Operator::In,
        "not in" => Operator::NotIn,
        "is null" => Operator::IsNull,
        "is not null" => Operator::IsNotNull,
        other => {
            return Err(AppError::UnprocessableEntity(format!(
                "Invalid filter operator: {other}"
            )))
        }
    })
}

/// Build a condition, normalising null comparisons (D10) and scalar `IN` values.
#[must_use]
pub fn condition(column: &str, operator: Operator, value: DbValue) -> Condition {
    let (operator, value) = match (operator, value) {
        (Operator::Eq, DbValue::Null) => (Operator::IsNull, DbValue::Null),
        (Operator::NotEq, DbValue::Null) => (Operator::IsNotNull, DbValue::Null),
        (op @ (Operator::In | Operator::NotIn), v) => match v {
            DbValue::Json(Value::Array(_)) => (op, v),
            DbValue::Text(s) if s.trim_start().starts_with('[') => {
                match serde_json::from_str::<Value>(&s) {
                    Ok(arr @ Value::Array(_)) => (op, DbValue::Json(arr)),
                    _ => (op, DbValue::Json(Value::Array(vec![Value::String(s)]))),
                }
            }
            other => (op, DbValue::Json(Value::Array(vec![other.to_json()]))),
        },
        (op, v) => (op, v),
    };
    Condition {
        column: CompactString::new(column),
        operator,
        value,
    }
}

fn parse_raw_filter_array(arr: &[Value]) -> Result<FilterExpr> {
    let invalid = || AppError::UnprocessableEntity("Invalid filter Inputs".to_string());
    let mut groups = Vec::new();
    for g in arr {
        let conds = g.as_array().ok_or_else(invalid)?;
        let mut group = Vec::new();
        for c in conds {
            let parts = c.as_array().ok_or_else(invalid)?;
            match parts.as_slice() {
                // `[]` placeholder inside `[[[]]]` = match all
                [] => {}
                [col, op, val] => {
                    let col = col.as_str().filter(|c| !c.is_empty()).ok_or_else(invalid)?;
                    let op = parse_operator(op.as_str().ok_or_else(invalid)?)?;
                    group.push(condition(col, op, DbValue::from(val.clone())));
                }
                [col, op] => {
                    let col = col.as_str().filter(|c| !c.is_empty()).ok_or_else(invalid)?;
                    let op = parse_operator(op.as_str().ok_or_else(invalid)?)?;
                    if !matches!(op, Operator::IsNull | Operator::IsNotNull) {
                        return Err(invalid());
                    }
                    group.push(condition(col, op, DbValue::Null));
                }
                _ => return Err(invalid()),
            }
        }
        groups.push(group);
    }
    Ok(FilterExpr { groups })
}

/// Normalizes sort string or array into a list of SortKey structs.
#[must_use]
pub fn normalize_sort(sort_val: &Value) -> Vec<SortKey> {
    let mut keys = Vec::new();
    if let Value::String(s) = sort_val {
        if let Ok(arr @ Value::Array(_)) = serde_json::from_str::<Value>(s) {
            return normalize_sort(&arr);
        }
    }
    let tokens: Vec<String> = match sort_val {
        Value::String(s) => s
            .split(',')
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
            .collect(),
        Value::Array(arr) => arr
            .iter()
            .filter_map(|v| v.as_str().map(ToString::to_string))
            .collect(),
        _ => Vec::new(),
    };

    for token in tokens {
        if let Some(stripped) = token.strip_prefix('-') {
            keys.push(SortKey {
                column: CompactString::new(stripped.trim()),
                direction: SortDirection::Desc,
            });
        } else if let Some(stripped) = token.strip_prefix('+') {
            keys.push(SortKey {
                column: CompactString::new(stripped.trim()),
                direction: SortDirection::Asc,
            });
        } else {
            keys.push(SortKey {
                column: CompactString::new(token.trim()),
                direction: SortDirection::Asc,
            });
        }
    }

    keys
}

/// Fans out free-text search across configured search columns.
#[must_use]
pub fn apply_search(
    filter: FilterExpr,
    search_columns: &[String],
    term: Option<&str>,
) -> FilterExpr {
    let Some(t) = term.map(str::trim).filter(|s| !s.is_empty()) else {
        return filter;
    };
    if search_columns.is_empty() {
        return filter;
    }

    let search_conditions: Vec<Condition> = search_columns
        .iter()
        .map(|col| Condition {
            column: CompactString::new(col),
            operator: Operator::Like,
            value: DbValue::Text(t.to_string()),
        })
        .collect();

    let base_groups = if filter.is_empty() {
        vec![vec![]]
    } else {
        filter.groups
    };

    let mut merged = Vec::new();
    for base_group in base_groups {
        for search_cond in &search_conditions {
            let mut new_group = base_group.clone();
            new_group.push(search_cond.clone());
            merged.push(new_group);
        }
    }

    FilterExpr { groups: merged }
}
