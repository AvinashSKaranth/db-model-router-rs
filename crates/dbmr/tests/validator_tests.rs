//! `validate_schema` rule coverage: every rejection path reports its exact path and message.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use dbmr::schema::validator::{base_type_of, is_encrypted_rule};
use dbmr::schema::{validate_schema, SchemaValidationError, ValidationError};
use serde_json::{json, Value};

/// All `(path, message)` pairs reported for `raw`.
fn errors(raw: &Value) -> Vec<(String, String)> {
    validate_schema(raw)
        .expect_err("schema should be rejected")
        .errors
        .into_iter()
        .map(|e| (e.path, e.message))
        .collect()
}

/// The single message reported at `path`.
fn message_at(raw: &Value, path: &str) -> String {
    let errs = errors(raw);
    let hits: Vec<&String> = errs
        .iter()
        .filter(|(p, _)| p == path)
        .map(|(_, m)| m)
        .collect();
    assert_eq!(hits.len(), 1, "expected one error at {path}, got {errs:?}");
    hits[0].clone()
}

/// Minimal valid schema with one `items` table.
fn with_table(table: &Value) -> Value {
    json!({"adapter": "sqlite3", "framework": "axum", "tables": {"items": table}})
}

fn with_options(options: &Value) -> Value {
    json!({"adapter": "sqlite3", "framework": "axum", "tables": {}, "options": options})
}

fn with_relationships(rels: &Value) -> Value {
    json!({
        "adapter": "sqlite3", "framework": "axum",
        "tables": {"a": {"columns": {}}, "b": {"columns": {}}},
        "relationships": rels
    })
}

#[test]
fn full_valid_schema_passes() {
    let raw = json!({
        "adapter": "postgres",
        "framework": "express",
        "tables": {
            "orgs": {"columns": {"id": "auto_increment", "name": "required|string"}},
            "users": {
                "pk": "id",
                "columns": {
                    "id": "auto_increment",
                    "name": "required|string|maxLength:20",
                    "deleted": "boolean",
                    "meta": "object",
                    "meta.ssn": "encrypted|string",
                    "secret": "encrypted|required|string"
                },
                "unique": [["id"], ["name"]],
                "softDelete": "deleted",
                "search_columns": ["name"],
                "parent": "orgs"
            }
        },
        "relationships": [{"parent": "orgs", "child": "users", "foreignKey": "org_id"}],
        "options": {
            "output": "./out", "saasStructure": false, "apiBasePath": "/v1", "port": 8080,
            "session": "redis", "rateLimiting": true, "helmet": false, "logger": true, "loki": false,
            "encryption": {"key": "env:KEY", "version": 2}
        }
    });
    validate_schema(&raw).unwrap();
}

#[test]
fn non_object_schema_is_rejected_immediately() {
    for raw in [json!(null), json!([]), json!("x")] {
        assert_eq!(
            errors(&raw),
            vec![(String::new(), "Schema must be a non-null object".into())]
        );
    }
}

#[test]
fn missing_top_level_fields_are_each_reported() {
    let errs = errors(&json!({}));
    assert_eq!(
        errs,
        vec![
            (
                "adapter".into(),
                "adapter is required and must be a string".into()
            ),
            (
                "framework".into(),
                "framework is required and must be a string".into()
            ),
            (
                "tables".into(),
                "tables is required and must be an object".into()
            ),
        ]
    );
}

#[test]
fn unknown_adapter_and_framework_list_allowed_values() {
    let raw = json!({"adapter": "db2", "framework": "rocket", "tables": {}});
    assert!(
        message_at(&raw, "adapter").starts_with("Invalid adapter \"db2\". Must be one of: mysql")
    );
    assert_eq!(
        message_at(&raw, "framework"),
        "Invalid framework \"rocket\". Must be one of: express, ultimate-express, axum"
    );
}

#[test]
fn table_must_be_object_with_columns_object() {
    let raw = json!({"adapter": "sqlite3", "framework": "axum",
        "tables": {"a": 1, "b": {"columns": []}}});
    assert_eq!(
        message_at(&raw, "tables.a"),
        "Table \"a\" must be an object"
    );
    assert_eq!(
        message_at(&raw, "tables.b.columns"),
        "Table \"b\" must have a columns object"
    );
}

#[test]
fn non_string_column_rule_is_invalid() {
    let raw = with_table(&json!({"columns": {"n": 5}}));
    assert!(message_at(&raw, "tables.items.columns.n").starts_with("Invalid column rule \"\""));
}

#[test]
fn dotted_field_without_parent_column() {
    let raw = with_table(&json!({"columns": {"meta.k": "string"}}));
    assert_eq!(
        message_at(&raw, "tables.items.columns.meta.k"),
        "Dotted field \"meta.k\" has no parent column \"meta\" in table \"items\""
    );
}

#[test]
fn dotted_field_with_two_levels_of_nesting() {
    let raw = with_table(&json!({"columns": {"meta": "object", "meta.a.b": "string"}}));
    assert_eq!(
        message_at(&raw, "tables.items.columns.meta.a.b"),
        "Dotted field \"meta.a.b\" must use at most one level of nesting (parent.key)"
    );
}

#[test]
fn dotted_parent_with_unknown_type_reports_empty_type() {
    let raw = with_table(&json!({"columns": {"meta": "weird", "meta.k": "string"}}));
    let errs = errors(&raw);
    assert!(errs.contains(&(
        "tables.items.columns.meta.k".into(),
        "Dotted field \"meta.k\" requires parent column \"meta\" to be declared as \"object\" (found \"\")".into()
    )));
}

#[test]
fn encrypted_object_types_cannot_be_encrypted() {
    let raw = with_table(&json!({"columns": {
        "meta": "object",
        "meta.k": "encrypted|object",
        "blob": "encrypted|object"
    }}));
    assert_eq!(
        message_at(&raw, "tables.items.columns.meta.k"),
        "Dotted field \"meta.k\" uses type \"object\" which cannot be encrypted. Supported: string, integer, numeric, boolean, datetime"
    );
    assert_eq!(
        message_at(&raw, "tables.items.columns.blob"),
        "Column \"blob\" uses type \"object\" which cannot be encrypted. Supported: string, integer, numeric, boolean, datetime"
    );
}

#[test]
fn pk_cannot_be_dotted_or_encrypted() {
    let dotted = with_table(&json!({"pk": "meta.id", "columns": {"meta": "object"}}));
    assert_eq!(
        message_at(&dotted, "tables.items.pk"),
        "pk cannot reference a dotted (virtual JSON) field \"meta.id\""
    );
    let enc = with_table(&json!({"pk": "code", "columns": {"code": "encrypted|string"}}));
    assert_eq!(
        message_at(&enc, "tables.items.pk"),
        "pk cannot reference an encrypted column \"code\" (encrypted fields cannot be used to look up rows)"
    );
}

#[test]
fn soft_delete_rules() {
    let cols = json!({"del": "boolean", "sec": "encrypted|boolean"});
    let case = |sd: Value| with_table(&json!({"columns": cols, "softDelete": sd}));
    assert_eq!(
        message_at(&case(json!("a.b")), "tables.items.softDelete"),
        "softDelete cannot reference a dotted (virtual JSON) field \"a.b\""
    );
    assert_eq!(
        message_at(&case(json!("nope")), "tables.items.softDelete"),
        "softDelete column \"nope\" does not exist in table \"items\""
    );
    assert_eq!(
        message_at(&case(json!("sec")), "tables.items.softDelete"),
        "softDelete cannot reference an encrypted column \"sec\" (encrypted fields cannot be filtered)"
    );
    assert_eq!(
        message_at(&case(json!(1)), "tables.items.softDelete"),
        "softDelete must be a string in table \"items\""
    );
    validate_schema(&case(Value::Null)).unwrap();
    validate_schema(&case(json!("del"))).unwrap();
}

#[test]
fn unique_flat_array_rules_use_flat_paths() {
    let raw = with_table(&json!({
        "columns": {"a": "string", "s": "encrypted|string"},
        "unique": ["id", "a", "x.y", "s", "ghost", 7]
    }));
    let errs = errors(&raw);
    assert_eq!(
        errs,
        vec![
            (
                "tables.items.unique[2]".into(),
                "unique cannot reference a dotted (virtual JSON) field \"x.y\"".into()
            ),
            (
                "tables.items.unique[3]".into(),
                "unique entry \"s\" cannot reference an encrypted column \"s\" (encrypted values are never equal, so uniqueness cannot be enforced)".into()
            ),
            (
                "tables.items.unique[4]".into(),
                "unique entry \"ghost\" does not match any column or the primary key \"id\" in table \"items\"".into()
            ),
            (
                "tables.items.unique[5]".into(),
                "unique entry must be a string in table \"items\"".into()
            ),
        ]
    );
}

#[test]
fn unique_nested_groups_use_group_paths() {
    let raw = with_table(&json!({"columns": {"a": "string"}, "unique": [["a"], ["a", "ghost"]]}));
    assert!(message_at(&raw, "tables.items.unique[1][1]").starts_with("unique entry \"ghost\""));
}

#[test]
fn unique_must_be_array() {
    let raw = with_table(&json!({"columns": {}, "unique": "a"}));
    assert_eq!(
        message_at(&raw, "tables.items.unique"),
        "unique must be an array in table \"items\""
    );
}

#[test]
fn search_columns_rules() {
    let raw = with_table(&json!({
        "columns": {"a": "string", "s": "encrypted|string"},
        "search_columns": ["a", "", "x.y", "ghost", "s", 3]
    }));
    let errs = errors(&raw);
    let at = |i: usize| {
        errs.iter()
            .find(|(p, _)| *p == format!("tables.items.search_columns[{i}]"))
            .map(|(_, m)| m.as_str())
    };
    assert_eq!(at(0), None);
    assert_eq!(
        at(1),
        Some("search_columns entry must be a non-empty string in table \"items\"")
    );
    assert_eq!(
        at(2),
        Some("search_columns cannot reference a dotted (virtual JSON) field \"x.y\"")
    );
    assert_eq!(
        at(3),
        Some("search_columns entry \"ghost\" does not exist in table \"items\"")
    );
    assert_eq!(
        at(4),
        Some("search_columns cannot reference an encrypted column \"s\" (encrypted fields cannot be searched)")
    );
    assert_eq!(
        at(5),
        Some("search_columns entry must be a non-empty string in table \"items\"")
    );
    assert_eq!(errs.len(), 5);
}

#[test]
fn search_columns_must_be_array_but_may_be_null() {
    let bad = with_table(&json!({"columns": {}, "search_columns": "a"}));
    assert_eq!(
        message_at(&bad, "tables.items.search_columns"),
        "search_columns must be an array of strings in table \"items\""
    );
    validate_schema(&with_table(&json!({"columns": {}, "search_columns": null}))).unwrap();
}

#[test]
fn parent_must_reference_existing_table() {
    let unknown = with_table(&json!({"columns": {}, "parent": "orgs"}));
    assert_eq!(
        message_at(&unknown, "tables.items.parent"),
        "parent \"orgs\" does not reference an existing table in table \"items\""
    );
    let non_string = with_table(&json!({"columns": {}, "parent": 1}));
    assert_eq!(
        message_at(&non_string, "tables.items.parent"),
        "parent must be a string or null in table \"items\""
    );
    validate_schema(&with_table(&json!({"columns": {}, "parent": null}))).unwrap();
}

#[test]
fn relationships_must_be_array_of_complete_objects() {
    assert_eq!(
        message_at(&with_relationships(&json!({})), "relationships"),
        "relationships must be an array"
    );
    let raw = with_relationships(&json!([
        5,
        {},
        {"parent": "x", "child": "y", "foreignKey": "a_id"},
        {"parent": "a", "child": "b", "foreignKey": "a_id"}
    ]));
    assert_eq!(
        errors(&raw),
        vec![
            (
                "relationships[0]".into(),
                "Each relationship must be an object".into()
            ),
            (
                "relationships[1].parent".into(),
                "Relationship must have a parent string".into()
            ),
            (
                "relationships[1].child".into(),
                "Relationship must have a child string".into()
            ),
            (
                "relationships[1].foreignKey".into(),
                "Relationship must have a foreignKey string".into()
            ),
            (
                "relationships[2].parent".into(),
                "Relationship parent \"x\" does not reference an existing table".into()
            ),
            (
                "relationships[2].child".into(),
                "Relationship child \"y\" does not reference an existing table".into()
            ),
        ]
    );
}

#[test]
fn options_must_be_object() {
    let raw = json!({"adapter": "sqlite3", "framework": "axum", "tables": {}, "options": []});
    assert_eq!(message_at(&raw, "options"), "options must be an object");
}

#[test]
fn every_invalid_option_is_reported() {
    let raw = with_options(&json!({
        "output": "", "saasStructure": "yes", "apiBasePath": "api", "port": 0,
        "session": "cookie", "rateLimiting": 1, "helmet": "y", "logger": null, "loki": [],
        "encryption": {"version": 0}
    }));
    assert_eq!(
        errors(&raw),
        vec![
            (
                "options.output".into(),
                "options.output must be a non-empty string path or null".into()
            ),
            (
                "options.saasStructure".into(),
                "options.saasStructure must be a boolean".into()
            ),
            (
                "options.apiBasePath".into(),
                "options.apiBasePath must be a string starting with \"/\" (e.g. \"/api\")".into()
            ),
            (
                "options.port".into(),
                "options.port must be a positive integer".into()
            ),
            (
                "options.session".into(),
                "options.session must be one of: memory, redis, database".into()
            ),
            (
                "options.rateLimiting".into(),
                "options.rateLimiting must be a boolean".into()
            ),
            (
                "options.helmet".into(),
                "options.helmet must be a boolean".into()
            ),
            (
                "options.logger".into(),
                "options.logger must be a boolean".into()
            ),
            (
                "options.loki".into(),
                "options.loki must be a boolean".into()
            ),
            (
                "options.encryption.key".into(),
                "options.encryption.key must be a string (env:VAR or raw:<base64 32-byte key>)"
                    .into()
            ),
            (
                "options.encryption.version".into(),
                "options.encryption.version must be a positive integer".into()
            ),
        ]
    );
}

#[test]
fn option_edge_values() {
    // null output is allowed; non-string output, port above u16 and non-object encryption are not.
    validate_schema(&with_options(&json!({"output": null, "session": 5}))).unwrap();
    assert!(
        message_at(&with_options(&json!({"output": 3})), "options.output").contains("non-empty")
    );
    assert!(
        message_at(&with_options(&json!({"port": 65536})), "options.port").contains("positive")
    );
    assert!(message_at(&with_options(&json!({"port": "80"})), "options.port").contains("positive"));
    assert!(message_at(
        &with_options(&json!({"apiBasePath": 1})),
        "options.apiBasePath"
    )
    .contains("starting with"));
    assert_eq!(
        message_at(
            &with_options(&json!({"encryption": "k"})),
            "options.encryption"
        ),
        "options.encryption must be an object { key, version, keys? }"
    );
}

#[test]
fn error_display_joins_paths_and_messages() {
    let err = SchemaValidationError {
        errors: vec![
            ValidationError {
                path: String::new(),
                message: "root".into(),
            },
            ValidationError {
                path: "adapter".into(),
                message: "bad".into(),
            },
        ],
    };
    assert_eq!(
        err.to_string(),
        "Schema validation failed: root; adapter: bad"
    );
}

#[test]
fn rule_helpers() {
    assert!(is_encrypted_rule("required|encrypted|string"));
    assert!(!is_encrypted_rule("string:encrypted"));
    assert_eq!(
        base_type_of("encrypted|required|integer:5"),
        Some("integer")
    );
    assert_eq!(base_type_of("required|email"), None);
}
