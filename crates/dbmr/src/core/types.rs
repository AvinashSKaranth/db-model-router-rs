use compact_str::CompactString;
use indexmap::IndexMap;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

/// Internal row representation: keyed by compact string with strongly-typed values.
pub type RowMap = IndexMap<CompactString, DbValue>;

/// Internal typed database values without JSON tree intermediate allocations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DbValue {
    Null,
    Bool(bool),
    Int(i128),
    Float(f64),
    Decimal(Decimal),
    Text(String),
    Bytes(Vec<u8>),
    Json(serde_json::Value),
}

impl DbValue {
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s.as_str()),
            _ => None,
        }
    }
}

impl std::fmt::Display for DbValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Null => write!(f, "null"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(i) => write!(f, "{i}"),
            Self::Float(fl) => write!(f, "{fl}"),
            Self::Decimal(d) => write!(f, "{d}"),
            Self::Text(s) => write!(f, "{s}"),
            Self::Bytes(b) => write!(f, "{b:?}"),
            Self::Json(j) => write!(f, "{j}"),
        }
    }
}

impl From<String> for DbValue {
    fn from(s: String) -> Self {
        Self::Text(s)
    }
}

impl From<&str> for DbValue {
    fn from(s: &str) -> Self {
        Self::Text(s.to_string())
    }
}

impl From<i64> for DbValue {
    fn from(i: i64) -> Self {
        Self::Int(i128::from(i))
    }
}

impl From<i32> for DbValue {
    fn from(i: i32) -> Self {
        Self::Int(i128::from(i))
    }
}

impl From<u32> for DbValue {
    fn from(i: u32) -> Self {
        Self::Int(i128::from(i))
    }
}

impl From<u64> for DbValue {
    fn from(i: u64) -> Self {
        Self::Int(i128::from(i))
    }
}

impl<T: Into<DbValue>> From<Option<T>> for DbValue {
    fn from(v: Option<T>) -> Self {
        v.map_or(Self::Null, Into::into)
    }
}

impl From<i128> for DbValue {
    fn from(i: i128) -> Self {
        Self::Int(i)
    }
}

impl From<f64> for DbValue {
    fn from(f: f64) -> Self {
        Self::Float(f)
    }
}

impl From<bool> for DbValue {
    fn from(b: bool) -> Self {
        Self::Bool(b)
    }
}

impl From<Decimal> for DbValue {
    fn from(d: Decimal) -> Self {
        Self::Decimal(d)
    }
}

impl From<serde_json::Value> for DbValue {
    fn from(v: serde_json::Value) -> Self {
        match v {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(b) => Self::Bool(b),
            serde_json::Value::Number(n) => {
                let lit = n.to_string();
                if let Some(i) = n.as_i128() {
                    Self::Int(i)
                } else if !lit.contains(['.', 'e', 'E']) {
                    // Integer beyond i128: keep every digit.
                    Self::Text(lit)
                } else {
                    let f = n.as_f64().unwrap_or(f64::NAN);
                    match lit.parse::<Decimal>() {
                        // Prefer Decimal when f64 would drop digits (money, big fractions).
                        Ok(d) if f.to_string() != lit => Self::Decimal(d),
                        _ if f.is_finite() => Self::Float(f),
                        _ => Self::Text(lit),
                    }
                }
            }
            serde_json::Value::String(s) => Self::Text(s),
            other => Self::Json(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortDirection {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SortKey {
    pub column: CompactString,
    pub direction: SortDirection,
}

#[derive(Debug, Clone, Default)]
pub struct GetOptions {
    pub count: bool,
    pub limit: Option<u64>,
    pub offset: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FindResult {
    pub data: Vec<RowMap>,
    pub count: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WriteSummary {
    pub rows: u64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<DbValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record: Option<RowMap>,
}

impl DbValue {
    /// Lossless JSON form: integers and decimals keep every digit (`arbitrary_precision`),
    /// bytes become base64, non-finite floats become `null`.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value;
        let num = |s: String| {
            s.parse::<serde_json::Number>()
                .map_or(Value::String(s), Value::Number)
        };
        match self {
            Self::Null => Value::Null,
            Self::Bool(b) => Value::Bool(*b),
            Self::Int(i) => num(i.to_string()),
            Self::Decimal(d) => num(d.normalize().to_string()),
            Self::Float(f) => serde_json::Number::from_f64(*f).map_or(Value::Null, Value::Number),
            Self::Text(s) => Value::String(s.clone()),
            Self::Bytes(b) => {
                use base64::Engine;
                Value::String(base64::engine::general_purpose::STANDARD.encode(b))
            }
            Self::Json(j) => j.clone(),
        }
    }
}

/// Row to JSON object (column order preserved).
#[must_use]
pub fn row_to_json(row: &RowMap) -> serde_json::Value {
    serde_json::Value::Object(
        row.iter()
            .map(|(k, v)| (k.to_string(), v.to_json()))
            .collect(),
    )
}

/// JSON object to row; non-objects yield an empty row.
#[must_use]
pub fn json_to_row(v: &serde_json::Value) -> RowMap {
    v.as_object()
        .map(|m| {
            m.iter()
                .map(|(k, v)| (CompactString::new(k), DbValue::from(v.clone())))
                .collect()
        })
        .unwrap_or_default()
}

/// Build a row from `(column, value)` pairs: `row([("name", "Ann".into())])`.
#[must_use]
pub fn row<const N: usize>(pairs: [(&str, DbValue); N]) -> RowMap {
    pairs
        .into_iter()
        .map(|(k, v)| (CompactString::new(k), v))
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn big_numbers_round_trip() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"a":170141183460469231731687303715884105727,"b":12345678901234567.89,"c":1.5}"#,
        )
        .unwrap();
        let r = json_to_row(&v);
        assert!(matches!(r["a"], DbValue::Int(_)));
        assert!(matches!(r["b"], DbValue::Decimal(_)));
        assert!(matches!(r["c"], DbValue::Float(_)));
        let back = row_to_json(&r);
        assert_eq!(
            back["a"].to_string(),
            "170141183460469231731687303715884105727"
        );
        assert_eq!(back["b"].to_string(), "12345678901234567.89");
    }
}
