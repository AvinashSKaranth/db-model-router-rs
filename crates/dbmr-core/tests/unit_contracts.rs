//! Unit-level behaviour of errors, JSON/path helpers, values, queries, filters and rules.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::float_cmp)]

use dbmr_core::error::AppError;
use dbmr_core::filter::{
    apply_search, condition, data_to_filter, normalize_sort, parse_filter_value, parse_operator,
    FilterExpr, Operator,
};
use dbmr_core::json::{json_safe_parse, json_stringify};
use dbmr_core::query::{apply_select, Query, DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};
use dbmr_core::rules::{
    attribute_name, collect_errors, compile, finish, validate_value, ColumnRules,
};
use dbmr_core::types::{json_to_row, row, row_to_json, DbValue, SortDirection};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::str::FromStr;

// ------------------------------------------------------------------ error

#[test]
fn app_error_maps_each_variant_to_status_and_bare_message() {
    let cases = [
        (AppError::BadRequest("a".into()), 400, "Bad request: a"),
        (AppError::NotFound("b".into()), 404, "Not found: b"),
        (
            AppError::UnprocessableEntity("c".into()),
            422,
            "Unprocessable entity: c",
        ),
        (AppError::Validation("d".into()), 422, "Validation error: d"),
        (AppError::Internal("e".into()), 500, "Internal error: e"),
        (AppError::Database("f".into()), 500, "Database error: f"),
        (AppError::Encryption("g".into()), 500, "Encryption error: g"),
    ];
    for (err, status, display) in cases {
        assert_eq!(err.status_code(), status);
        assert_eq!(err.to_string(), display);
        assert_eq!(err.message(), &display[display.len() - 1..]);
    }
}

// ------------------------------------------------------------------ json

#[test]
fn json_safe_parse_recurses_into_arrays_and_objects_and_keeps_invalid_json_text() {
    let v = json!({"a": "[1,2]", "b": ["{\"x\":1}", "plain"], "c": 5, "d": "{not json}"});
    assert_eq!(
        json_safe_parse(&v),
        json!({"a": [1, 2], "b": [{"x": 1}, "plain"], "c": 5, "d": "{not json}"})
    );
    // Nested JSON strings inside parsed JSON are parsed too.
    assert_eq!(
        json_safe_parse(&json!(r#"{"inner":"[true]"}"#)),
        json!({"inner": [true]})
    );
}

#[test]
fn json_stringify_turns_nested_values_into_strings_inside_maps_only() {
    let v = json!({"obj": {"k": 1}, "arr": [1, 2], "n": 3});
    assert_eq!(
        json_stringify(&v),
        json!({"obj": "{\"k\":1}", "arr": "[1,2]", "n": 3})
    );
    assert_eq!(
        json_stringify(&json!([{"a": [1]}, 7])),
        json!([{"a": "[1]"}, 7])
    );
    assert_eq!(json_stringify(&json!("s")), json!("s"));
}

// ------------------------------------------------------------------ types

#[test]
fn db_value_accessors_and_display() {
    assert!(DbValue::Null.is_null());
    assert!(!DbValue::Int(0).is_null());
    assert_eq!(DbValue::from("x").as_str(), Some("x"));
    assert_eq!(DbValue::Int(1).as_str(), None);

    assert_eq!(DbValue::Null.to_string(), "null");
    assert_eq!(DbValue::Bool(true).to_string(), "true");
    assert_eq!(DbValue::Int(-3).to_string(), "-3");
    assert_eq!(DbValue::Float(1.5).to_string(), "1.5");
    assert_eq!(
        DbValue::Decimal(Decimal::from_str("2.50").unwrap()).to_string(),
        "2.50"
    );
    assert_eq!(DbValue::Text("t".into()).to_string(), "t");
    assert_eq!(DbValue::Bytes(vec![1, 2]).to_string(), "[1, 2]");
    assert_eq!(DbValue::Json(json!({"a": 1})).to_string(), r#"{"a":1}"#);
}

#[test]
fn db_value_from_conversions() {
    assert_eq!(DbValue::from(String::from("s")), DbValue::Text("s".into()));
    assert_eq!(DbValue::from(5i64), DbValue::Int(5));
    assert_eq!(DbValue::from(5i32), DbValue::Int(5));
    assert_eq!(DbValue::from(5u32), DbValue::Int(5));
    assert_eq!(DbValue::from(u64::MAX), DbValue::Int(i128::from(u64::MAX)));
    assert_eq!(DbValue::from(7i128), DbValue::Int(7));
    assert_eq!(DbValue::from(2.5f64), DbValue::Float(2.5));
    assert_eq!(DbValue::from(false), DbValue::Bool(false));
    assert_eq!(DbValue::from(Some(3i64)), DbValue::Int(3));
    assert_eq!(DbValue::from(None::<i64>), DbValue::Null);
    let d = Decimal::from_str("1.25").unwrap();
    assert_eq!(DbValue::from(d), DbValue::Decimal(d));
}

#[test]
fn db_value_from_json_preserves_every_number_shape() {
    assert_eq!(DbValue::from(Value::Null), DbValue::Null);
    assert_eq!(DbValue::from(json!(true)), DbValue::Bool(true));
    assert_eq!(DbValue::from(json!("s")), DbValue::Text("s".into()));
    assert_eq!(DbValue::from(json!([1])), DbValue::Json(json!([1])));
    assert_eq!(
        DbValue::from(json!({"a": 1})),
        DbValue::Json(json!({"a": 1}))
    );
    // Integer beyond i128 keeps every digit as text.
    let huge: Value = serde_json::from_str("1701411834604692317316873037158841057270").unwrap();
    assert_eq!(
        DbValue::from(huge),
        DbValue::Text("1701411834604692317316873037158841057270".into())
    );
    // A float that overflows f64 and cannot be a Decimal stays text.
    let inf: Value = serde_json::from_str("1e400").unwrap();
    assert_eq!(DbValue::from(inf), DbValue::Text("1e+400".into()));
}

#[test]
fn db_value_to_json_is_lossless() {
    assert_eq!(DbValue::Null.to_json(), Value::Null);
    assert_eq!(DbValue::Bool(true).to_json(), json!(true));
    assert_eq!(DbValue::Float(f64::NAN).to_json(), Value::Null);
    assert_eq!(DbValue::Float(0.5).to_json(), json!(0.5));
    assert_eq!(DbValue::Bytes(b"hi".to_vec()).to_json(), json!("aGk="));
    assert_eq!(DbValue::Json(json!([1])).to_json(), json!([1]));
    assert_eq!(DbValue::Text("x".into()).to_json(), json!("x"));
    assert_eq!(
        DbValue::Decimal(Decimal::from_str("1.500").unwrap())
            .to_json()
            .to_string(),
        "1.5"
    );
}

#[test]
fn row_helpers_build_and_convert_rows() {
    let r = row([("a", 1i64.into()), ("b", "x".into())]);
    assert_eq!(row_to_json(&r), json!({"a": 1, "b": "x"}));
    assert!(json_to_row(&json!([1, 2])).is_empty());
    assert_eq!(json_to_row(&json!({"a": 1})), row([("a", 1i64.into())]));
}

// ------------------------------------------------------------------ query

#[test]
fn query_builder_methods_set_their_fields() {
    let q = Query::filter(FilterExpr::eq("id", 1i64))
        .eq("name", "ann")
        .and("age", Operator::Gt, 18i64)
        .sort("-age,+name")
        .select(["id", "name"])
        .search("ann")
        .page(2)
        .size(10)
        .limit(5)
        .with_count(false);
    assert_eq!(q.filter.groups[0].len(), 3);
    assert_eq!(q.sort[0].direction, SortDirection::Desc);
    assert_eq!(q.sort[1].column.as_str(), "name");
    assert_eq!(
        q.select_columns.as_deref(),
        Some(&["id".to_string(), "name".to_string()][..])
    );
    assert_eq!(q.search.as_deref(), Some("ann"));
    assert_eq!(q.pagination(), (2, 10));
    assert_eq!(q.limit, Some(5));
    assert_eq!(q.count, Some(false));
    assert_eq!(Query::new().limit(7).pagination(), (0, 7));
    assert_eq!(Query::new().size(0).pagination(), (0, 1));
    assert_eq!(Query::new().pagination(), (0, DEFAULT_PAGE_SIZE));
    assert_eq!(Query::new().size(10_000).pagination().1, MAX_PAGE_SIZE);
}

#[test]
fn query_from_json_coerces_reserved_keys_like_node() {
    let (q, out) = Query::from_json(
        &json!({"select_columns": ["id", 5, "name"], "output_content_type": "", "search": null,
                "page": 3, "limit": 2.9, "count": 0}),
        "id",
    )
    .unwrap();
    assert_eq!(q.select_columns.unwrap(), vec!["id", "name"]);
    assert_eq!(out, None);
    assert_eq!(q.search, None);
    assert_eq!(q.page, Some(3));
    assert_eq!(q.size, Some(2));
    assert_eq!(q.limit, Some(2));
    assert_eq!(q.count, Some(false));
    // Empty object payload = match-all filter.
    assert!(q.filter.is_empty());

    let (q, _) = Query::from_json(
        &json!({"select_columns": 5, "search": 42, "page": "+4", "size": "-", "count": "yes"}),
        "id",
    )
    .unwrap();
    assert_eq!(q.select_columns, None);
    assert_eq!(q.search.as_deref(), Some("42"));
    assert_eq!(q.page, Some(4));
    assert_eq!(q.size, Some(DEFAULT_PAGE_SIZE));
    assert_eq!(q.count, Some(true));

    let truthy = |v: Value| {
        Query::from_json(&json!({ "count": v }), "id")
            .unwrap()
            .0
            .count
            .unwrap()
    };
    assert!(!truthy(Value::Null));
    assert!(truthy(json!(true)));
    assert!(!truthy(json!("")));
    assert!(truthy(json!(1)));
    assert!(truthy(json!([])));

    // page that is not a number falls back to 0; limit < 1 is dropped.
    let (q, _) = Query::from_json(&json!({"page": [], "limit": 0}), "id").unwrap();
    assert_eq!(q.page, Some(0));
    assert_eq!(q.limit, None);

    // Non-object payload is a PK filter.
    let (q, _) = Query::from_json(&json!("7"), "id").unwrap();
    assert_eq!(q.filter, FilterExpr::eq("id", "7"));

    let err = Query::from_json(&Value::Null, "id").unwrap_err();
    assert_eq!(err.message(), "Invalid filter Inputs");
}

#[test]
fn apply_select_keeps_requested_columns_in_requested_order() {
    let r = row([("a", 1i64.into()), ("b", 2i64.into()), ("c", 3i64.into())]);
    let out = apply_select(r, &["c".into(), "missing".into(), "a".into()]);
    let keys: Vec<&str> = out.keys().map(compact_str::CompactString::as_str).collect();
    assert_eq!(keys, ["c", "a"]);
}

// ------------------------------------------------------------------ filter

#[test]
fn operator_display_matches_sql_tokens() {
    let all = [
        (Operator::Eq, "="),
        (Operator::NotEq, "!="),
        (Operator::Gt, ">"),
        (Operator::Lt, "<"),
        (Operator::Gte, ">="),
        (Operator::Lte, "<="),
        (Operator::Like, "like"),
        (Operator::NotLike, "not like"),
        (Operator::In, "in"),
        (Operator::NotIn, "not in"),
        (Operator::IsNull, "is null"),
        (Operator::IsNotNull, "is not null"),
    ];
    for (op, s) in all {
        assert_eq!(op.to_string(), s);
        // Display output round-trips through the raw-filter operator parser.
        assert_eq!(parse_operator(s).unwrap(), op);
    }
}

#[test]
fn parse_operator_accepts_aliases_and_rejects_unknown() {
    for (tok, op) in [
        ("==", Operator::Eq),
        ("EQ", Operator::Eq),
        ("<>", Operator::NotEq),
        ("ne", Operator::NotEq),
        ("gt", Operator::Gt),
        ("lt", Operator::Lt),
        (" gte ", Operator::Gte),
        ("lte", Operator::Lte),
    ] {
        assert_eq!(parse_operator(tok).unwrap(), op);
    }
    let err = parse_operator("~~").unwrap_err();
    assert_eq!(err.status_code(), 422);
    assert_eq!(err.message(), "Invalid filter operator: ~~");
}

#[test]
fn filter_expr_combinators() {
    assert!(FilterExpr::empty().is_empty());
    assert!(FilterExpr {
        groups: vec![vec![]]
    }
    .is_empty());
    let a = FilterExpr::eq("a", 1i64);
    assert!(!a.is_empty());
    let both = FilterExpr {
        groups: vec![vec![]],
    }
    .or(a.clone())
    .or(FilterExpr::eq("b", 2i64));
    assert_eq!(both.groups.len(), 2);
    // `and` applies to every OR-group.
    let anded = both.and("c", Operator::Lt, 3i64);
    assert!(anded.groups.iter().all(|g| g.len() == 2));
    assert_eq!(
        FilterExpr::from_json(&json!({"a": 1}), "id").unwrap(),
        FilterExpr::eq("a", 1i64)
    );
}

#[test]
fn parse_filter_value_non_string_and_comparison_prefixes() {
    assert_eq!(
        parse_filter_value(&Value::Null),
        (Operator::IsNull, DbValue::Null)
    );
    assert_eq!(
        parse_filter_value(&json!(5)),
        (Operator::Eq, DbValue::Int(5))
    );
    assert_eq!(
        parse_filter_value(&json!("<=5")),
        (Operator::Lte, DbValue::Text("5".into()))
    );
    assert_eq!(
        parse_filter_value(&json!(">5")),
        (Operator::Gt, DbValue::Text("5".into()))
    );
    assert_eq!(
        parse_filter_value(&json!("<5")),
        (Operator::Lt, DbValue::Text("5".into()))
    );
    assert_eq!(
        parse_filter_value(&json!("!x")),
        (Operator::NotEq, DbValue::Text("x".into()))
    );
    assert_eq!(
        parse_filter_value(&json!("IN(a)")),
        (Operator::In, DbValue::Json(json!(["a"])))
    );
    // Too short to be an `in(...)` list: plain equality.
    assert_eq!(
        parse_filter_value(&json!("in()")),
        (Operator::Eq, DbValue::Text("in()".into()))
    );
}

#[test]
fn data_to_filter_rejects_empty_and_unsupported_inputs() {
    for bad in [Value::Null, json!(""), json!(true), json!([1, {"a": 1}])] {
        let err = data_to_filter(&bad, "id").unwrap_err();
        assert_eq!(err.status_code(), 422, "{bad}");
        assert_eq!(err.message(), "Invalid filter Inputs");
    }
}

#[test]
fn data_to_filter_scalar_and_array_forms() {
    assert_eq!(
        data_to_filter(&json!("abc"), "pk").unwrap(),
        FilterExpr::eq("pk", "abc")
    );
    assert_eq!(
        data_to_filter(&json!(9), "pk").unwrap(),
        FilterExpr::eq("pk", 9i64)
    );
    assert!(data_to_filter(&json!([]), "pk").unwrap().is_empty());
    assert!(data_to_filter(&json!({}), "pk").unwrap().is_empty());

    let f = data_to_filter(&json!([1, "2"]), "pk").unwrap();
    assert_eq!(f.groups[0][0].operator, Operator::In);
    assert_eq!(f.groups[0][0].value, DbValue::Json(json!([1, "2"])));

    let f = data_to_filter(&json!([{"pk": 1}, {"pk": 2}]), "pk").unwrap();
    assert_eq!(f.groups[0][0].value, DbValue::Json(json!([1, 2])));

    let f = data_to_filter(&json!([{"a": 1}, {"b": ">2"}]), "pk").unwrap();
    assert_eq!(f.groups.len(), 2);
    assert_eq!(f.groups[1][0].operator, Operator::Gt);

    // `{"data": [...]}` unwraps to the array form.
    let f = data_to_filter(&json!({"data": [1, 2]}), "pk").unwrap();
    assert_eq!(f.groups[0][0].operator, Operator::In);

    // A JSON string holding an object is parsed first.
    let f = data_to_filter(&json!(r#"{"a":"%x%"}"#), "pk").unwrap();
    assert_eq!(f.groups[0][0].operator, Operator::Like);
}

#[test]
fn raw_filter_array_merges_extra_keys_and_validates_shape() {
    let f = data_to_filter(
        &json!({"filter": [[["age", ">", 1]], [["age", "<", 0]]], "tenant": "t1"}),
        "id",
    )
    .unwrap();
    assert_eq!(f.groups.len(), 2);
    assert!(f
        .groups
        .iter()
        .all(|g| g.len() == 2 && g[1].column == "tenant"));

    // Empty raw filter + extra keys = one group of the extras.
    let f = data_to_filter(&json!({"filter": [], "tenant": "t1"}), "id").unwrap();
    assert_eq!(f, FilterExpr::eq("tenant", "t1"));

    // `[]` placeholder and two-element null checks.
    let f = data_to_filter(
        &json!({"filter": [[[], ["a", "is null"], ["b", "IS NOT NULL"]]]}),
        "id",
    )
    .unwrap();
    assert_eq!(f.groups[0][0].operator, Operator::IsNull);
    assert_eq!(f.groups[0][1].operator, Operator::IsNotNull);

    for bad in [
        json!({"filter": [1]}),
        json!({"filter": [[1]]}),
        json!({"filter": [[["a", "="]]]}),
        json!({"filter": [[["a", "=", 1, 2]]]}),
        json!({"filter": [[["", "=", 1]]]}),
        json!({"filter": [[["a", 5, 1]]]}),
        json!({"filter": [[[5, "is null"]]]}),
    ] {
        let err = data_to_filter(&bad, "id").unwrap_err();
        assert_eq!(err.message(), "Invalid filter Inputs", "{bad}");
    }
    let err = data_to_filter(&json!({"filter": [[["a", "bogus", 1]]]}), "id").unwrap_err();
    assert_eq!(err.message(), "Invalid filter operator: bogus");
}

#[test]
fn condition_normalises_null_comparisons_and_in_values() {
    let c = condition("a", Operator::Eq, DbValue::Null);
    assert_eq!(c.operator, Operator::IsNull);
    let c = condition("a", Operator::NotEq, DbValue::Null);
    assert_eq!(c.operator, Operator::IsNotNull);
    let c = condition("a", Operator::In, DbValue::Text(" [1,2]".into()));
    assert_eq!(c.value, DbValue::Json(json!([1, 2])));
    let c = condition("a", Operator::NotIn, DbValue::Text("[oops".into()));
    assert_eq!(c.value, DbValue::Json(json!(["[oops"])));
    let c = condition("a", Operator::In, DbValue::Int(3));
    assert_eq!(c.value, DbValue::Json(json!([3])));
    let c = condition("a", Operator::In, DbValue::Json(json!([4])));
    assert_eq!(c.value, DbValue::Json(json!([4])));
}

#[test]
fn normalize_sort_accepts_strings_arrays_and_json_strings() {
    let keys = normalize_sort(&json!(r#"["-a","b"]"#));
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].direction, SortDirection::Desc);
    let keys = normalize_sort(&json!(["+a", 5, "- b"]));
    assert_eq!(keys.len(), 2);
    assert_eq!(keys[0].direction, SortDirection::Asc);
    assert_eq!(keys[1].column.as_str(), "b");
    assert!(normalize_sort(&json!(5)).is_empty());
    assert!(normalize_sort(&json!(" , ")).is_empty());
}

#[test]
fn apply_search_fans_out_per_group_and_column() {
    let cols = vec!["name".to_string(), "email".to_string()];
    let base = FilterExpr::eq("a", 1i64).or(FilterExpr::eq("b", 2i64));
    assert_eq!(apply_search(base.clone(), &cols, None), base);
    assert_eq!(apply_search(base.clone(), &cols, Some("   ")), base);
    assert_eq!(apply_search(base.clone(), &[], Some("x")), base);
    let out = apply_search(base, &cols, Some(" x "));
    assert_eq!(out.groups.len(), 4);
    assert_eq!(out.groups[3][1].column.as_str(), "email");
    assert_eq!(out.groups[3][1].value, DbValue::Text("x".into()));
    let out = apply_search(FilterExpr::empty(), &cols, Some("x"));
    assert_eq!(out.groups.len(), 2);
    assert_eq!(out.groups[0].len(), 1);
}

// ------------------------------------------------------------------ rules

#[allow(clippy::needless_pass_by_value)]
fn check(rule: &str, v: Option<DbValue>) -> Option<String> {
    validate_value(&compile("field", rule).unwrap(), "field", v.as_ref())
}

#[test]
fn compile_reports_malformed_arguments() {
    let msg = |rule: &str| compile("col", rule).unwrap_err().message().to_string();
    assert_eq!(
        msg("maxLength"),
        "Seed in maxLength rule for col must be a number."
    );
    assert_eq!(
        msg("minLength:x"),
        "Seed in minLength rule for col must be a number."
    );
    assert_eq!(msg("max:"), "Seed in max rule for col must be a number.");
    assert_eq!(msg("min:abc"), "Seed in min rule for col must be a number.");
    assert_eq!(
        msg("regex"),
        "Seed in regex rule for col must be a pattern."
    );
    assert!(msg("regex:(").starts_with("Invalid regex for col: "));
    assert_eq!(msg("string|nope"), "Validation Rule: nope does not exists.");
    assert_eq!(
        msg("encrypted"),
        "Field \"col\" is marked \"encrypted\" but type \"(none)\" cannot be encrypted (supported: string, integer, numeric, boolean, datetime)"
    );
}

#[test]
fn compile_tracks_flags_base_type_and_required_position() {
    let r = compile(
        "c",
        "string|sometimes|auto_increment|integer|required|nullable",
    )
    .unwrap();
    assert!(r.is_required());
    assert!(r.nullable);
    assert_eq!(r.base_type.as_deref(), Some("string"));
    let r2: ColumnRules = r.without_required();
    assert!(!r2.is_required());
    assert_eq!(r2.rules.len(), 2);
    assert!(compile("c", "encrypted|numeric").unwrap().encrypted);
}

#[test]
fn every_rule_has_its_niv_message() {
    let cases: &[(&str, DbValue, &str)] = &[
        ("string", DbValue::Int(1), "The field must be a string."),
        ("numeric", "1a".into(), "The field must be a number."),
        (
            "decimal",
            ".".into(),
            "The field must be a valid decimal value.",
        ),
        (
            "decimal",
            "1.2.3".into(),
            "The field must be a valid decimal value.",
        ),
        (
            "boolean",
            DbValue::Int(2),
            "The field field must be boolean.",
        ),
        (
            "boolean",
            DbValue::Float(1.0),
            "The field field must be boolean.",
        ),
        (
            "object",
            DbValue::Json(json!([1])),
            "The field must be an object.",
        ),
        (
            "array",
            DbValue::Json(json!({"a": 1})),
            "The field must be an array.",
        ),
        (
            "datetime",
            "2024-13-01 00:00:00".into(),
            "The field must be a valid datetime(YYYY-MM-DD HH:mm:ss).",
        ),
        (
            "email",
            "nope".into(),
            "The field must be a valid email address.",
        ),
        (
            "minLength:3",
            "ab".into(),
            "The field can not be less than 3.",
        ),
        (
            "max:10",
            DbValue::Int(11),
            "The field can not be greater than 10.",
        ),
        (
            "max:10",
            "abc".into(),
            "The field can not be greater than 10.",
        ),
        (
            "min:2",
            DbValue::Float(1.5),
            "The field must be at least 2.",
        ),
        ("regex:^a+$", "b".into(), "The field format is invalid."),
        ("notIn:x,y", "y".into(), "The selected field is invalid."),
    ];
    for (rule, v, expected) in cases {
        assert_eq!(
            check(rule, Some(v.clone())).as_deref(),
            Some(*expected),
            "{rule}"
        );
    }
}

#[test]
fn passing_values_produce_no_message() {
    let ok: &[(&str, DbValue)] = &[
        ("numeric", "-1.5".into()),
        ("decimal", "+.5".into()),
        ("boolean", DbValue::Bool(true)),
        ("boolean", DbValue::Int(0)),
        ("object", DbValue::Json(json!({"a": 1}))),
        ("array", DbValue::Json(json!([1, null, "x"]))),
        ("minLength:2", "ab".into()),
        ("max:10", DbValue::Int(10)),
        ("min:2", DbValue::Int(2)),
        ("notIn:x,y", "z".into()),
        ("in:a,b", "b".into()),
        ("maxLength:3", DbValue::Bytes(b"abc".to_vec())),
        // Arrays are JS-stringified: [1,null,"x"] -> "1,,x" (4 UTF-16 units).
        ("maxLength:4", DbValue::Json(json!([1, null, "x"]))),
        ("in:[object Object]", DbValue::Json(json!({"k": 1}))),
    ];
    for (rule, v) in ok {
        assert_eq!(check(rule, Some(v.clone())), None, "{rule} {v:?}");
    }
}

#[test]
fn empty_values_skip_optional_rules_but_fail_required() {
    // NaN is "really empty" (JS String(NaN) is not empty, niv treats NaN as empty).
    assert_eq!(check("integer", Some(DbValue::Float(f64::NAN))), None);
    assert_eq!(check("integer", Some(DbValue::Json(json!([])))), None);
    assert_eq!(
        check("required|integer", Some(DbValue::Null)).as_deref(),
        Some("The field field is mandatory.")
    );
    // nullable only exempts an explicit null, not a missing key.
    assert_eq!(
        check("required|nullable", None).as_deref(),
        Some("The field field is mandatory.")
    );
    assert_eq!(check("required|nullable", Some(DbValue::Null)), None);
}

#[test]
fn attribute_name_splits_snake_and_camel_case() {
    assert_eq!(attribute_name("userID"), "user id");
    assert_eq!(attribute_name("CreatedAt"), "created at");
    assert_eq!(attribute_name("a_b"), "a b");
}

#[test]
fn collect_errors_prefixes_attributes_and_finish_joins_with_space() {
    let rules = [
        compile("first_name", "required|string").unwrap(),
        compile("age", "integer").unwrap(),
        compile("ok", "boolean").unwrap(),
    ];
    let r = row([("age", "x".into()), ("ok", true.into())]);
    let mut errors = Vec::new();
    collect_errors(&rules, &r, "data.2.", &mut errors);
    assert_eq!(
        errors,
        [
            "The data.2.first_name field is mandatory.",
            "The data.2.age must be an integer."
        ]
    );
    let err = finish(&errors).unwrap_err();
    assert_eq!(err.status_code(), 422);
    assert_eq!(
        err.message(),
        "The data.2.first_name field is mandatory. The data.2.age must be an integer."
    );
    assert!(finish(&[]).is_ok());
}
