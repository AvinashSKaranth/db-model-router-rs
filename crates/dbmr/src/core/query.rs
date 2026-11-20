//! Read options for `find` / `find_one` / `list` (§20.4, §21.2).
//!
//! ```
//! use dbmr::{Query, core::filter::Operator};
//! let q = Query::new()
//!     .eq("status", "active")
//!     .and("age", Operator::Gte, 18)
//!     .sort("-created_at,name")
//!     .select(["id", "name"])
//!     .page(0)
//!     .size(50);
//! assert_eq!(q.size, Some(50));
//! ```

use crate::core::error::Result;
use crate::core::filter::{apply_search, data_to_filter, normalize_sort, FilterExpr, Operator};
use crate::core::types::{DbValue, SortKey};
use serde_json::Value;

/// Default and maximum page size for `list`.
pub const DEFAULT_PAGE_SIZE: u64 = 30;
pub const MAX_PAGE_SIZE: u64 = 200;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Query {
    pub filter: FilterExpr,
    pub sort: Vec<SortKey>,
    /// Column projection applied after decryption, before `after_find`.
    pub select_columns: Option<Vec<String>>,
    /// Free-text term fanned out over the model's `search_columns` (LIKE).
    pub search: Option<String>,
    /// `list` only. Default 0.
    pub page: Option<u64>,
    /// `list` only. Default 30, clamped to 1..=200.
    pub size: Option<u64>,
    /// `find` only: maximum rows. `list` treats it as an alias of `size`.
    pub limit: Option<u64>,
    /// Whether to compute the total count. `find` default: `true` unless `limit` is set;
    /// `list` default: `true`.
    pub count: Option<bool>,
    /// Relation names to attach (see `Model::has_many` / `Model::belongs_to`).
    pub include: Vec<String>,
}

/// Node `parseInt(x, 10)`: leading integer of a number or string.
fn parse_int(v: &Value) -> Option<i64> {
    match v {
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().map(|f| f.trunc() as i64)),
        Value::String(s) => {
            let t = s.trim_start();
            let end = t
                .char_indices()
                .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+')))
                .map(|(i, c)| i + c.len_utf8())
                .last()?;
            t[..end].parse().ok()
        }
        _ => None,
    }
}

/// JS truthiness for `Boolean(data.count)`.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

impl Query {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Query with a prepared filter.
    #[must_use]
    pub fn filter(filter: FilterExpr) -> Self {
        Self {
            filter,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn eq(self, column: &str, value: impl Into<DbValue>) -> Self {
        self.and(column, Operator::Eq, value)
    }

    #[must_use]
    pub fn and(mut self, column: &str, op: Operator, value: impl Into<DbValue>) -> Self {
        self.filter = self.filter.and(column, op, value);
        self
    }

    /// `"-age,name"`, `"+name"`, or a JSON array string.
    #[must_use]
    pub fn sort(mut self, spec: &str) -> Self {
        self.sort = normalize_sort(&Value::String(spec.to_string()));
        self
    }

    #[must_use]
    pub fn select<I: IntoIterator<Item = S>, S: Into<String>>(mut self, cols: I) -> Self {
        self.select_columns = Some(cols.into_iter().map(Into::into).collect());
        self
    }

    #[must_use]
    pub fn search(mut self, term: impl Into<String>) -> Self {
        self.search = Some(term.into());
        self
    }

    #[must_use]
    pub fn page(mut self, page: u64) -> Self {
        self.page = Some(page);
        self
    }

    #[must_use]
    pub fn size(mut self, size: u64) -> Self {
        self.size = Some(size);
        self
    }

    #[must_use]
    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }

    /// Attach relations by name, e.g. `.include(["comments"])`.
    #[must_use]
    pub fn include<I: IntoIterator<Item = S>, S: Into<String>>(mut self, names: I) -> Self {
        self.include = names.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn with_count(mut self, count: bool) -> Self {
        self.count = Some(count);
        self
    }

    /// Parse an HTTP-style payload: reserved keys (`select_columns`, `search`, `sort`,
    /// `page`, `size`, `limit`, `count`, `output_content_type`) are extracted with Node's
    /// coercion rules and the remaining keys become the filter (§22).
    ///
    /// Returns the query and `output_content_type` (if any).
    ///
    /// # Errors
    /// `422 "Invalid filter Inputs"` (or an invalid operator) from the filter parser.
    pub fn from_json(payload: &Value, primary_key: &str) -> Result<(Self, Option<String>)> {
        let mut q = Self::default();
        let mut output = None;
        let mut data = payload.clone();
        if let Value::Object(map) = &mut data {
            if let Some(v) = map.remove("select_columns") {
                q.select_columns = match v {
                    Value::String(s) if !s.is_empty() => {
                        Some(s.split(',').map(|c| c.trim().to_string()).collect())
                    }
                    Value::Array(a) => Some(
                        a.iter()
                            .filter_map(|c| c.as_str().map(str::to_string))
                            .collect(),
                    ),
                    _ => None,
                };
            }
            if let Some(v) = map.remove("output_content_type") {
                output = v.as_str().filter(|s| !s.is_empty()).map(str::to_string);
            }
            if let Some(v) = map.remove("search") {
                q.search = match v {
                    Value::Null => None,
                    Value::String(s) => Some(s),
                    other => Some(other.to_string()),
                };
            }
            if let Some(v) = map.remove("include") {
                q.include = match v {
                    Value::String(s) => s
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                        .collect(),
                    Value::Array(a) => a
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect(),
                    _ => Vec::new(),
                };
            }
            if let Some(v) = map.remove("sort") {
                q.sort = normalize_sort(&v);
            }
            if let Some(v) = map.remove("page") {
                q.page = Some(parse_int(&v).filter(|p| *p >= 0).map_or(0, |p| p as u64));
            }
            let size = map.remove("size");
            let limit = map.remove("limit");
            if let Some(v) = size.as_ref().or(limit.as_ref()) {
                let n = parse_int(v)
                    .filter(|n| *n >= 1)
                    .map_or(DEFAULT_PAGE_SIZE, |n| n as u64);
                q.size = Some(n.min(MAX_PAGE_SIZE));
            }
            if let Some(v) = &limit {
                q.limit = parse_int(v).filter(|n| *n >= 1).map(|n| n as u64);
            }
            if let Some(v) = map.remove("count") {
                q.count = Some(truthy(&v));
            }
        }
        q.filter = data_to_filter(&data, primary_key)?;
        Ok((q, output))
    }

    /// Filter with `search` expanded over `search_columns`.
    #[must_use]
    pub(crate) fn effective_filter(&self, search_columns: &[String]) -> FilterExpr {
        apply_search(self.filter.clone(), search_columns, self.search.as_deref())
    }

    /// `(page, size)` with Node defaults and clamping.
    #[must_use]
    pub fn pagination(&self) -> (u64, u64) {
        let size = self
            .size
            .or(self.limit)
            .unwrap_or(DEFAULT_PAGE_SIZE)
            .clamp(1, MAX_PAGE_SIZE);
        (self.page.unwrap_or(0), size)
    }
}

/// Keep only `cols` (in `cols` order); missing keys are absent.
#[must_use]
pub fn apply_select(
    row: crate::core::types::RowMap,
    cols: &[String],
) -> crate::core::types::RowMap {
    let mut row = row;
    cols.iter()
        .filter_map(|c| row.shift_remove_entry(c.as_str()))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn reserved_params_follow_node_coercion() {
        let (q, out) = Query::from_json(
            &json!({"page":"-3","size":"999","sort":"-age","select_columns":"id, name","search":"  bob ","output_content_type":"csv","name":"x"}),
            "id",
        )
        .unwrap();
        assert_eq!(q.pagination(), (0, 200));
        assert_eq!(q.select_columns.unwrap(), vec!["id", "name"]);
        assert_eq!(q.search.as_deref(), Some("  bob "));
        assert_eq!(out.as_deref(), Some("csv"));
        assert_eq!(q.filter.groups[0].len(), 1);
        let (q, _) = Query::from_json(&json!({"size":"abc","limit":"5"}), "id").unwrap();
        assert_eq!(q.pagination(), (0, 30));
        assert_eq!(q.limit, Some(5));
        let (q, _) = Query::from_json(&json!({"size":"12px"}), "id").unwrap();
        assert_eq!(q.pagination(), (0, 12));
    }
}
