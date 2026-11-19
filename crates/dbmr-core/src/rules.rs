//! Column rule strings (`"required|string|maxLength:100"`) compiled once per model and
//! evaluated with node-input-validator (niv) compatible semantics and English messages.
//!
//! Semantics (niv v4, bail mode):
//! - `required` is checked first; it fails on missing, `null`, or whitespace-only strings
//!   (`false` and `0` pass).
//! - A non-required attribute that is missing, `null` or `""` skips every other rule.
//! - Only the first failing rule per attribute is reported.
//! - Messages of all failing attributes are joined with a single space.
//!
//! Deliberate differences from niv (see `docs/deviations.md`): rule arguments are split at
//! the **first** `:` (niv truncates `regex:a:b` to `a`), and a `regex:` pattern is taken
//! verbatim (niv splits it on `,`).

use crate::error::{AppError, Result};
use crate::types::{DbValue, RowMap};
use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

#[allow(clippy::unwrap_used)]
static INT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[-+]?(0|[1-9]\d*)$").unwrap());
#[allow(clippy::unwrap_used)]
static NUMERIC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[+-]?([0-9]*[.])?[0-9]+$").unwrap());
#[allow(clippy::unwrap_used)]
static DECIMAL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[-+]?([0-9]+)?(\.[0-9]+)?$").unwrap());
#[allow(clippy::unwrap_used)]
static EMAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[A-Za-z0-9.!#$%&'*+/=?^_`{|}~-]+@[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9-]{0,61}[A-Za-z0-9])?)+$").unwrap()
});

/// Base types recognised in a rule string (first match wins).
pub const BASE_TYPES: &[&str] = &[
    "auto_increment",
    "string",
    "integer",
    "numeric",
    "boolean",
    "datetime",
    "object",
];

#[derive(Debug, Clone)]
pub enum Rule {
    Required,
    String,
    Integer,
    Numeric,
    Decimal,
    Boolean,
    Object,
    Array,
    Datetime,
    Email,
    MaxLength(usize),
    MinLength(usize),
    Max(f64),
    Min(f64),
    Regex(Regex),
    In(Vec<String>),
    NotIn(Vec<String>),
}

/// One column's compiled rules.
#[derive(Debug, Clone)]
pub struct ColumnRules {
    pub column: String,
    pub rules: Vec<Rule>,
    pub nullable: bool,
    pub encrypted: bool,
    /// First base type token (`string`, `integer`, ...), used by encryption serialization.
    pub base_type: Option<String>,
}

impl ColumnRules {
    #[must_use]
    pub fn is_required(&self) -> bool {
        matches!(self.rules.first(), Some(Rule::Required))
    }

    /// Copy without `required` (used by `patch`).
    #[must_use]
    pub fn without_required(&self) -> Self {
        let mut c = self.clone();
        c.rules.retain(|r| !matches!(r, Rule::Required));
        c
    }
}

/// Compile one rule string.
///
/// # Errors
/// [`AppError::Validation`] for an unknown rule or a malformed argument.
pub fn compile(column: &str, rule: &str) -> Result<ColumnRules> {
    let mut out = ColumnRules {
        column: column.to_string(),
        rules: Vec::new(),
        nullable: false,
        encrypted: false,
        base_type: None,
    };
    for token in rule.split('|').map(str::trim).filter(|t| !t.is_empty()) {
        let (name, arg) = match token.split_once(':') {
            Some((n, a)) => (n, Some(a)),
            None => (token, None),
        };
        let bad = |what: &str| {
            AppError::Validation(format!("Seed in {name} rule for {column} must be {what}."))
        };
        if BASE_TYPES.contains(&name) && out.base_type.is_none() {
            out.base_type = Some(name.to_string());
        }
        let parsed =
            match name {
                "required" => {
                    out.rules.insert(0, Rule::Required);
                    continue;
                }
                "nullable" => {
                    out.nullable = true;
                    continue;
                }
                "encrypted" => {
                    out.encrypted = true;
                    continue;
                }
                // Schema-only tokens: no runtime check.
                "auto_increment" | "sometimes" => continue,
                "string" => Rule::String,
                "integer" => Rule::Integer,
                "numeric" => Rule::Numeric,
                "decimal" => Rule::Decimal,
                "boolean" => Rule::Boolean,
                "object" => Rule::Object,
                "array" => Rule::Array,
                "datetime" => Rule::Datetime,
                "email" => Rule::Email,
                "maxLength" => Rule::MaxLength(
                    arg.and_then(|a| a.trim().parse().ok())
                        .ok_or_else(|| bad("a number"))?,
                ),
                "minLength" => Rule::MinLength(
                    arg.and_then(|a| a.trim().parse().ok())
                        .ok_or_else(|| bad("a number"))?,
                ),
                "max" => Rule::Max(
                    arg.and_then(|a| a.trim().parse().ok())
                        .ok_or_else(|| bad("a number"))?,
                ),
                "min" => Rule::Min(
                    arg.and_then(|a| a.trim().parse().ok())
                        .ok_or_else(|| bad("a number"))?,
                ),
                "regex" => Rule::Regex(Regex::new(arg.ok_or_else(|| bad("a pattern"))?).map_err(
                    |e| AppError::Validation(format!("Invalid regex for {column}: {e}")),
                )?),
                "in" => Rule::In(arg.unwrap_or("").split(',').map(str::to_string).collect()),
                "notIn" => Rule::NotIn(arg.unwrap_or("").split(',').map(str::to_string).collect()),
                other => {
                    return Err(AppError::Validation(format!(
                        "Validation Rule: {other} does not exists."
                    )))
                }
            };
        out.rules.push(parsed);
    }
    if out.encrypted
        && !matches!(
            out.base_type.as_deref(),
            Some("string" | "integer" | "numeric" | "boolean" | "datetime")
        )
    {
        return Err(AppError::Validation(format!(
            "Field \"{column}\" is marked \"encrypted\" but type \"{}\" cannot be encrypted (supported: string, integer, numeric, boolean, datetime)",
            out.base_type.as_deref().unwrap_or("(none)")
        )));
    }
    Ok(out)
}

/// niv attribute display name: `first_name` / `firstName` -> `first name`; dotted paths verbatim.
#[must_use]
pub fn attribute_name(attr: &str) -> String {
    if attr.contains('.') {
        return attr.to_string();
    }
    let snake = attr.replace('_', " ");
    let mut out = String::with_capacity(snake.len() + 4);
    let mut prev_upper = false;
    for ch in snake.chars() {
        if ch.is_ascii_uppercase() {
            if !prev_upper {
                out.push(' ');
            }
            out.push(ch.to_ascii_lowercase());
            prev_upper = true;
        } else {
            out.push(ch);
            prev_upper = false;
        }
    }
    out.trim_start().to_string()
}

/// JS `String(value)`.
fn js_string(v: &DbValue) -> String {
    match v {
        DbValue::Json(Value::Object(_)) => "[object Object]".to_string(),
        DbValue::Json(Value::Array(a)) => a
            .iter()
            .map(|x| match x {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                o => o.to_string(),
            })
            .collect::<Vec<_>>()
            .join(","),
        DbValue::Bytes(b) => String::from_utf8_lossy(b).into_owned(),
        other => other.to_string(),
    }
}

/// niv `reallyEmpty`: missing, null, or a value whose string form is empty.
fn really_empty(v: Option<&DbValue>) -> bool {
    match v {
        None | Some(DbValue::Null) => true,
        Some(DbValue::Float(f)) if f.is_nan() => true,
        Some(other) => js_string(other).is_empty(),
    }
}

fn check(rule: &Rule, v: &DbValue) -> bool {
    match rule {
        Rule::Required => !js_string(v).trim().is_empty() && !v.is_null(),
        Rule::String => matches!(v, DbValue::Text(_)),
        Rule::Integer => INT_RE.is_match(&js_string(v)),
        Rule::Numeric => NUMERIC_RE.is_match(&js_string(v)),
        Rule::Decimal => {
            let s = js_string(v);
            !s.is_empty() && s != "." && DECIMAL_RE.is_match(&s)
        }
        Rule::Boolean => match v {
            DbValue::Bool(_) => true,
            DbValue::Int(i) => *i == 0 || *i == 1,
            DbValue::Text(s) => matches!(s.as_str(), "true" | "false" | "0" | "1"),
            _ => false,
        },
        Rule::Object => matches!(v, DbValue::Json(Value::Object(_))),
        Rule::Array => matches!(v, DbValue::Json(Value::Array(_))),
        Rule::Datetime => {
            let s = js_string(v);
            s.len() == 19 && chrono::NaiveDateTime::parse_from_str(&s, "%Y-%m-%d %H:%M:%S").is_ok()
        }
        Rule::Email => EMAIL_RE.is_match(&js_string(v)),
        Rule::MaxLength(n) => js_string(v).encode_utf16().count() <= *n,
        Rule::MinLength(n) => js_string(v).encode_utf16().count() >= *n,
        Rule::Max(n) => js_string(v).parse::<f64>().is_ok_and(|x| x <= *n),
        Rule::Min(n) => js_string(v).parse::<f64>().is_ok_and(|x| x >= *n),
        Rule::Regex(re) => re.is_match(&js_string(v)),
        Rule::In(opts) => opts.contains(&js_string(v)),
        Rule::NotIn(opts) => !opts.contains(&js_string(v)),
    }
}

fn message(rule: &Rule, attr: &str) -> String {
    let a = attribute_name(attr);
    match rule {
        Rule::Required => format!("The {a} field is mandatory."),
        Rule::String => format!("The {a} must be a string."),
        Rule::Integer => format!("The {a} must be an integer."),
        Rule::Numeric => format!("The {a} must be a number."),
        Rule::Decimal => format!("The {a} must be a valid decimal value."),
        Rule::Boolean => format!("The {a} field must be boolean."),
        Rule::Object => format!("The {a} must be an object."),
        Rule::Array => format!("The {a} must be an array."),
        Rule::Datetime => format!("The {a} must be a valid datetime(YYYY-MM-DD HH:mm:ss)."),
        Rule::Email => format!("The {a} must be a valid email address."),
        Rule::MaxLength(n) => format!("The {a} can not be greater than {n}."),
        Rule::MinLength(n) => format!("The {a} can not be less than {n}."),
        Rule::Max(n) => format!("The {a} can not be greater than {n}."),
        Rule::Min(n) => format!("The {a} must be at least {n}."),
        Rule::Regex(_) => format!("The {a} format is invalid."),
        Rule::In(_) | Rule::NotIn(_) => format!("The selected {a} is invalid."),
    }
}

/// Validate one attribute; returns its first error message.
#[must_use]
pub fn validate_value(rules: &ColumnRules, attr: &str, value: Option<&DbValue>) -> Option<String> {
    if rules.nullable && matches!(value, Some(DbValue::Null)) {
        return None;
    }
    let empty = really_empty(value);
    if !rules.is_required() && empty {
        return None;
    }
    for rule in &rules.rules {
        let ok = match (rule, value) {
            (Rule::Required, _) => !empty && value.is_some_and(|v| check(rule, v)),
            (_, Some(v)) => check(rule, v),
            (_, None) => false,
        };
        if !ok {
            return Some(message(rule, attr));
        }
    }
    None
}

/// Validate `row` against `rules`; `prefix` is prepended to attribute names (bulk: `data.3.`).
pub fn collect_errors<'a>(
    rules: impl IntoIterator<Item = &'a ColumnRules>,
    row: &RowMap,
    prefix: &str,
    errors: &mut Vec<String>,
) {
    for r in rules {
        let attr = if prefix.is_empty() {
            r.column.clone()
        } else {
            format!("{prefix}{}", r.column)
        };
        if let Some(msg) = validate_value(r, &attr, row.get(r.column.as_str())) {
            errors.push(msg);
        }
    }
}

/// Turn collected messages into the 422 error (space-joined, niv `getErrorMessage`).
///
/// # Errors
/// [`AppError::UnprocessableEntity`] when `errors` is non-empty.
pub fn finish(errors: &[String]) -> Result<()> {
    if errors.is_empty() {
        Ok(())
    } else {
        Err(AppError::UnprocessableEntity(errors.join(" ")))
    }
}

/// Convert `v` to the column's declared type, so the API returns the same JSON type on
/// every adapter. Applied to writes after validation (`"9"` → `9`, `"true"`/`"0"` →
/// boolean) and to reads (`0`/`1` → boolean, numbers in text columns → text, integral
/// floats → integers). Values that do not convert are left unchanged.
///
/// ponytail: non-integral `numeric`/`decimal` text is written as f64 (MongoDB stores a
/// `Decimal` as text, which breaks range filters); use an integer column of cents when
/// exact money values matter.
pub fn normalize(col: &ColumnRules, v: &mut DbValue) {
    use rust_decimal::prelude::ToPrimitive;
    const EXACT: f64 = 9_007_199_254_740_992.0; // 2^53
    let integral = |f: f64| f.fract() == 0.0 && f.abs() < EXACT;
    // `auto_increment` alone (no type rule) is an integer column.
    let auto_int = [Rule::Integer];
    let declared = if col.base_type.as_deref() == Some("auto_increment") {
        &auto_int[..]
    } else {
        &col.rules[..]
    };
    let Some(ty) = declared.iter().find(|r| {
        matches!(
            r,
            Rule::String
                | Rule::Email
                | Rule::Integer
                | Rule::Numeric
                | Rule::Decimal
                | Rule::Boolean
        )
    }) else {
        return;
    };
    #[allow(clippy::cast_possible_truncation)]
    let new = match (ty, &*v) {
        (
            Rule::String | Rule::Email,
            DbValue::Int(_) | DbValue::Float(_) | DbValue::Decimal(_) | DbValue::Bool(_),
        ) => DbValue::Text(v.to_string()),
        (Rule::Integer, DbValue::Text(t)) => match t.trim().parse::<i128>() {
            Ok(i) => DbValue::Int(i),
            Err(_) => return,
        },
        (Rule::Integer, DbValue::Float(f)) if integral(*f) => DbValue::Int(*f as i128),
        (Rule::Integer, DbValue::Decimal(d)) if d.fract().is_zero() => match d.to_i128() {
            Some(i) => DbValue::Int(i),
            None => return,
        },
        (Rule::Numeric | Rule::Decimal, DbValue::Text(t)) => {
            let t = t.trim();
            if let Ok(i) = t.parse::<i128>() {
                DbValue::Int(i)
            } else if let Ok(f) = t.parse::<f64>() {
                if integral(f) {
                    DbValue::Int(f as i128)
                } else {
                    DbValue::Float(f)
                }
            } else {
                return;
            }
        }
        (Rule::Numeric | Rule::Decimal, DbValue::Float(f)) if integral(*f) => {
            DbValue::Int(*f as i128)
        }
        (Rule::Boolean, DbValue::Int(i)) if *i == 0 || *i == 1 => DbValue::Bool(*i == 1),
        (Rule::Boolean, DbValue::Decimal(d)) if d.is_zero() || *d == rust_decimal::Decimal::ONE => {
            DbValue::Bool(!d.is_zero())
        }
        (Rule::Boolean, DbValue::Text(t)) => match t.trim() {
            "true" | "1" => DbValue::Bool(true),
            "false" | "0" => DbValue::Bool(false),
            _ => return,
        },
        _ => return,
    };
    *v = new;
}

/// [`normalize`] every declared column present in `row`.
pub fn normalize_row(rules: &[ColumnRules], row: &mut RowMap) {
    for col in rules {
        if let Some(v) = row.get_mut(col.column.as_str()) {
            normalize(col, v);
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[allow(clippy::needless_pass_by_value)]
    fn err(rule: &str, v: Option<DbValue>) -> Option<String> {
        validate_value(
            &compile("first_name", rule).unwrap(),
            "first_name",
            v.as_ref(),
        )
    }

    #[test]
    fn niv_semantics() {
        assert_eq!(
            err("required|string", None).unwrap(),
            "The first name field is mandatory."
        );
        assert_eq!(
            err("required", Some("  ".into())).unwrap(),
            "The first name field is mandatory."
        );
        assert!(err("required|boolean", Some(false.into())).is_none());
        assert!(err("required|integer", Some(DbValue::Int(0))).is_none());
        assert!(err("string|maxLength:3", None).is_none());
        assert!(err("string|maxLength:3", Some("".into())).is_none());
        assert_eq!(
            err("string|maxLength:3", Some("abcd".into())).unwrap(),
            "The first name can not be greater than 3."
        );
        assert_eq!(
            err("integer", Some("1.5".into())).unwrap(),
            "The first name must be an integer."
        );
        assert!(err("integer", Some("42".into())).is_none());
        assert!(err("integer", Some(DbValue::Float(1.0))).is_none());
        assert!(err("boolean", Some("1".into())).is_none());
        assert!(err("boolean", Some("yes".into())).is_some());
        assert!(err("datetime", Some("2024-01-02 03:04:05".into())).is_none());
        assert!(err("datetime", Some("2024-01-02T03:04:05".into())).is_some());
        assert!(err("regex:^a:b$", Some("a:b".into())).is_none());
        assert_eq!(
            err("in:a,b", Some("c".into())).unwrap(),
            "The selected first name is invalid."
        );
        assert!(err("email", Some("x@y.io".into())).is_none());
        assert!(err("nullable|string", Some(DbValue::Null)).is_none());
        assert!(compile("x", "bogus").is_err());
        assert!(compile("x", "encrypted|object").is_err());
        assert_eq!(attribute_name("firstName"), "first name");
        assert_eq!(attribute_name("data.0.first_name"), "data.0.first_name");
    }
}
