#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args
)]

use dbmr::schema::{parse_schema, print_schema, schema_to_model_meta};

fn sample_valid_schema_json() -> &'static str {
    r#"{
      "adapter": "postgres",
      "framework": "express",
      "tables": {
        "users": {
          "columns": {
            "name": "required|string",
            "email": "required|string"
          }
        }
      }
    }"#
}

#[test]
fn test_parse_valid_schema_defaults() {
    let schema = parse_schema(sample_valid_schema_json()).expect("Should parse valid schema");
    assert_eq!(schema.adapter, "postgres");
    assert_eq!(schema.framework, "express");

    let user_table = &schema.tables["users"];
    assert_eq!(user_table.pk, "id");
    assert_eq!(user_table.unique, Some(vec![vec!["id".to_string()]]));
    assert!(user_table.soft_delete.is_none());
    assert_eq!(user_table.columns.get("name").unwrap(), "required|string");

    let opts = schema.options.as_ref().unwrap();
    assert_eq!(opts.session, "memory");
    assert!(opts.rate_limiting);
    assert!(opts.helmet);
    assert_eq!(opts.port, 3000);
    assert_eq!(opts.api_base_path, "/api");
}

#[test]
fn test_invalid_adapter_fails() {
    let json = r#"{
      "adapter": "unsupported_db",
      "framework": "express",
      "tables": {}
    }"#;
    let err = parse_schema(json).expect_err("Should fail on unsupported adapter");
    assert!(err.errors.iter().any(|e| e.path == "adapter"));
}

#[test]
fn test_invalid_column_rule_fails() {
    let json = r#"{
      "adapter": "sqlite3",
      "framework": "express",
      "tables": {
        "items": {
          "columns": {
            "bad_col": "invalid_type"
          }
        }
      }
    }"#;
    let err = parse_schema(json).expect_err("Should fail on invalid column rule");
    assert!(err.errors.iter().any(|e| e.path.contains("bad_col")));
}

#[test]
fn test_encrypted_auto_increment_fails() {
    let json = r#"{
      "adapter": "sqlite3",
      "framework": "express",
      "tables": {
        "items": {
          "columns": {
            "bad_col": "encrypted|auto_increment"
          }
        }
      }
    }"#;
    let err = parse_schema(json).expect_err("Should fail on encrypted auto_increment");
    assert!(err.errors.iter().any(|e| e.path.contains("bad_col")));
}

#[test]
fn test_dotted_field_requires_object_parent() {
    let json = r#"{
      "adapter": "postgres",
      "framework": "express",
      "tables": {
        "users": {
          "columns": {
            "meta": "string",
            "meta.key": "encrypted|string"
          }
        }
      }
    }"#;
    let err = parse_schema(json).expect_err("Should fail because parent meta is not object");
    assert!(err.errors.iter().any(|e| e.path.contains("meta.key")));
}

#[test]
fn test_schema_roundtrip_idempotency() {
    let schema1 = parse_schema(sample_valid_schema_json()).unwrap();
    let printed1 = print_schema(&schema1);
    let schema2 = parse_schema(&printed1).unwrap();
    let printed2 = print_schema(&schema2);
    assert_eq!(printed1, printed2);
}

#[test]
fn test_schema_to_model_meta() {
    let schema = parse_schema(sample_valid_schema_json()).unwrap();
    let metas = schema_to_model_meta(&schema);
    assert_eq!(metas.len(), 1);
    assert_eq!(metas[0].table, "users");
    assert_eq!(metas[0].primary_key, "id");
    assert_eq!(metas[0].structure.len(), 2);
}
