#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args
)]

use dbmr_core::filter::{
    apply_search, data_to_filter, normalize_sort, parse_filter_value, Condition, FilterExpr,
    Operator,
};
use dbmr_core::types::{DbValue, SortDirection};
use serde_json::json;

#[test]
fn test_parse_filter_value_precedence() {
    // 1. !in
    let (op, val) = parse_filter_value(&json!("!in(apple, banana)"));
    assert_eq!(op, Operator::NotIn);
    assert_eq!(val, DbValue::Json(json!(["apple", "banana"])));

    // 2. in
    let (op, val) = parse_filter_value(&json!("in(red, blue, green)"));
    assert_eq!(op, Operator::In);
    assert_eq!(val, DbValue::Json(json!(["red", "blue", "green"])));

    // 3. not like (!%)
    let (op, val) = parse_filter_value(&json!("!%john%"));
    assert_eq!(op, Operator::NotLike);
    assert_eq!(val, DbValue::Text("%john%".to_string()));

    // 4. like (%)
    let (op, val) = parse_filter_value(&json!("%smith%"));
    assert_eq!(op, Operator::Like);
    assert_eq!(val, DbValue::Text("%smith%".to_string()));

    // 5. >=
    let (op, val) = parse_filter_value(&json!(">=100"));
    assert_eq!(op, Operator::Gte);
    assert_eq!(val, DbValue::Text("100".to_string()));

    // 6. <=
    let (op, val) = parse_filter_value(&json!("<=50"));
    assert_eq!(op, Operator::Lte);
    assert_eq!(val, DbValue::Text("50".to_string()));

    // 7. >
    let (op, val) = parse_filter_value(&json!(">10"));
    assert_eq!(op, Operator::Gt);
    assert_eq!(val, DbValue::Text("10".to_string()));

    // 8. <
    let (op, val) = parse_filter_value(&json!("<5"));
    assert_eq!(op, Operator::Lt);
    assert_eq!(val, DbValue::Text("5".to_string()));

    // 9. !=
    let (op, val) = parse_filter_value(&json!("!active"));
    assert_eq!(op, Operator::NotEq);
    assert_eq!(val, DbValue::Text("active".to_string()));

    // 10. =
    let (op, val) = parse_filter_value(&json!("exact"));
    assert_eq!(op, Operator::Eq);
    assert_eq!(val, DbValue::Text("exact".to_string()));
}

#[test]
fn test_data_to_filter_decision_table() {
    let pk = "id";

    // 1. null / empty -> 422
    assert!(data_to_filter(&json!(null), pk).is_err());
    assert!(data_to_filter(&json!(""), pk).is_err());

    // 2. number or string
    let f1 = data_to_filter(&json!(42), pk).unwrap();
    assert_eq!(f1.groups[0][0].operator, Operator::Eq);
    assert_eq!(f1.groups[0][0].value, DbValue::Int(42));

    let f2 = data_to_filter(&json!("user_123"), pk).unwrap();
    assert_eq!(f2.groups[0][0].operator, Operator::Eq);
    assert_eq!(f2.groups[0][0].value, DbValue::Text("user_123".to_string()));

    // 3. empty array -> [[[]]]
    let f3 = data_to_filter(&json!([]), pk).unwrap();
    assert_eq!(f3.groups, vec![vec![]]);

    // 4. array of primitives -> IN (...)
    let f4 = data_to_filter(&json!([1, 2, 3]), pk).unwrap();
    assert_eq!(f4.groups[0][0].operator, Operator::In);

    // 5. array of objects with only PK
    let f5 = data_to_filter(&json!([{"id": 10}, {"id": 20}]), pk).unwrap();
    assert_eq!(f5.groups[0][0].operator, Operator::In);

    // 6. array of objects otherwise -> OR of groups
    let f6 = data_to_filter(&json!([{"name": "Alice"}, {"name": "Bob"}]), pk).unwrap();
    assert_eq!(f6.groups.len(), 2);

    // 7. { data: [...] } recurses
    let f7 = data_to_filter(&json!({"data": [100, 200]}), pk).unwrap();
    assert_eq!(f7.groups[0][0].operator, Operator::In);

    // 8. { filter: [...] } merges without mutating input
    let orig = json!({
        "filter": [[["status", "=", "active"]]],
        "role": "admin"
    });
    let f8 = data_to_filter(&orig, pk).unwrap();
    assert_eq!(f8.groups[0].len(), 2);
    assert!(orig.get("filter").is_some()); // Original was not mutated!

    // 9. empty object {} -> [[[]]]
    let f9 = data_to_filter(&json!({}), pk).unwrap();
    assert_eq!(f9.groups, vec![vec![]]);
}

#[test]
fn test_normalize_sort() {
    let s1 = normalize_sort(&json!("-created_at, +name, age"));
    assert_eq!(s1.len(), 3);
    assert_eq!(s1[0].column.as_str(), "created_at");
    assert_eq!(s1[0].direction, SortDirection::Desc);
    assert_eq!(s1[1].column.as_str(), "name");
    assert_eq!(s1[1].direction, SortDirection::Asc);
    assert_eq!(s1[2].column.as_str(), "age");
    assert_eq!(s1[2].direction, SortDirection::Asc);
}

#[test]
fn test_apply_search() {
    let search_cols = vec!["name".to_string(), "email".to_string()];
    let base = FilterExpr {
        groups: vec![vec![Condition {
            column: "active".into(),
            operator: Operator::Eq,
            value: DbValue::Bool(true),
        }]],
    };

    let f = apply_search(base, &search_cols, Some("john"));
    assert_eq!(f.groups.len(), 2);
    assert_eq!(f.groups[0].len(), 2);
    assert_eq!(f.groups[0][1].column.as_str(), "name");
    assert_eq!(f.groups[1][1].column.as_str(), "email");
}
