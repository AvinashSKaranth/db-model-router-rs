//! Parser normalization, compiler output, printer canonical form, and serde defaults.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use dbmr::schema::{
    compile_columns, parse_schema, print_schema, schema_to_model_meta, EncryptedPath, Schema,
    TableDef,
};
use indexmap::IndexMap;
use serde_json::{json, Value};

const RICH: &str = r#"{
  "adapter": "postgres",
  "framework": "axum",
  "tables": {
    "posts": {
      "columns": {
        "id": "auto_increment",
        "org_id": "required|integer",
        "title": "required|string",
        "deleted": "boolean",
        "created": "datetime",
        "modified": "datetime"
      },
      "unique": ["title", "org_id"],
      "softDelete": "deleted",
      "search_columns": ["title"],
      "parent": "orgs",
      "timestamps": {"created_at": "created", "modified_at": "modified"}
    },
    "orgs": {
      "pk": "org_id",
      "columns": {"org_id": "required|integer", "name": "string"},
      "unique": [["name"], ["org_id"]]
    }
  },
  "relationships": [
    {"parent": "orgs", "child": "posts", "foreignKey": "org_id"},
    {"parent": "orgs", "child": "posts", "foreignKey": "owner_id"}
  ],
  "options": {
    "session": "database", "rateLimiting": false, "helmet": false, "logger": false,
    "loki": true, "output": "./gen", "saasStructure": false, "apiBasePath": "/v2",
    "port": 9000, "encryption": {"key": "env:K", "version": 3, "keys": {"2": "env:OLD"}}
  }
}"#;

#[test]
fn malformed_json_reports_invalid_json() {
    let err = parse_schema("{nope").unwrap_err();
    assert_eq!(err.errors.len(), 1);
    assert_eq!(err.errors[0].path, "");
    assert!(err.errors[0].message.starts_with("Invalid JSON: "));
    assert!(err
        .to_string()
        .starts_with("Schema validation failed: Invalid JSON: "));
}

#[test]
fn parse_normalizes_tables_and_options() {
    let s = parse_schema(RICH).unwrap();
    let posts = &s.tables["posts"];
    assert_eq!(posts.pk, "id");
    // A flat unique array becomes one group; nested groups are kept.
    assert_eq!(
        posts.unique,
        Some(vec![vec!["title".to_string(), "org_id".to_string()]])
    );
    assert_eq!(
        s.tables["orgs"].unique,
        Some(vec![vec!["name".to_string()], vec!["org_id".to_string()]])
    );
    assert_eq!(posts.soft_delete.as_deref(), Some("deleted"));
    assert_eq!(posts.search_columns, Some(vec!["title".to_string()]));
    assert_eq!(posts.parent.as_deref(), Some("orgs"));
    let ts = posts.timestamps.as_ref().unwrap();
    assert_eq!(ts.created_at.as_deref(), Some("created"));
    assert_eq!(ts.modified_at.as_deref(), Some("modified"));
    let orgs_ts = s.tables["orgs"].timestamps.as_ref().unwrap();
    assert!(orgs_ts.created_at.is_none() && orgs_ts.modified_at.is_none());

    let o = s.options.as_ref().unwrap();
    assert_eq!(o.session, "database");
    assert!(!o.rate_limiting && !o.helmet && !o.logger && o.loki && !o.saas_structure);
    assert_eq!(o.output.as_deref(), Some("./gen"));
    assert_eq!(o.api_base_path, "/v2");
    assert_eq!(o.port, 9000);
    let enc = o.encryption.as_ref().expect("encryption options are kept");
    assert_eq!(enc.key, "env:K");
    assert_eq!(enc.version, 3);
    assert_eq!(enc.keys.as_ref().unwrap()["2"], "env:OLD");
}

#[test]
fn parent_derives_relationship_and_explicit_duplicates_are_dropped() {
    let s = parse_schema(RICH).unwrap();
    let rels: Vec<(&str, &str, &str)> = s
        .relationships
        .iter()
        .map(|r| (r.parent.as_str(), r.child.as_str(), r.foreign_key.as_str()))
        .collect();
    // Derived from `parent` (fk = parent pk) first; explicit duplicate skipped; new one appended.
    assert_eq!(
        rels,
        vec![("orgs", "posts", "org_id"), ("orgs", "posts", "owner_id")]
    );
}

#[test]
fn print_is_canonical_and_round_trips() {
    let s = parse_schema(RICH).unwrap();
    let printed = print_schema(&s);
    assert!(printed.ends_with("}\n"));
    let v: Value = serde_json::from_str(&printed).unwrap();
    // Tables are sorted; non-default unique, softDelete, search_columns, parent, timestamps kept.
    let names: Vec<&String> = v["tables"].as_object().unwrap().keys().collect();
    assert_eq!(names, ["orgs", "posts"]);
    assert_eq!(v["tables"]["posts"]["unique"], json!([["title", "org_id"]]));
    assert_eq!(v["tables"]["posts"]["softDelete"], "deleted");
    assert_eq!(v["tables"]["posts"]["search_columns"], json!(["title"]));
    assert_eq!(v["tables"]["posts"]["parent"], "orgs");
    assert_eq!(v["tables"]["orgs"]["parent"], Value::Null);
    assert_eq!(
        v["tables"]["posts"]["timestamps"],
        json!({"created_at": "created", "modified_at": "modified"})
    );
    assert!(v["tables"]["orgs"].get("timestamps").is_none());
    assert_eq!(v["options"]["apiBasePath"], "/v2");
    assert_eq!(v["relationships"][1]["foreignKey"], "owner_id");

    let again = parse_schema(&printed).unwrap();
    assert_eq!(again, s);
    assert_eq!(print_schema(&again), printed);
}

#[test]
fn print_sorts_relationships_and_omits_empty_parts() {
    let mut tables = IndexMap::new();
    tables.insert(
        "t".to_string(),
        TableDef {
            pk: "id".into(),
            columns: IndexMap::new(),
            unique: Some(vec![vec!["id".into()]]),
            soft_delete: None,
            search_columns: Some(vec![]),
            parent: None,
            timestamps: None,
            hidden: None,
            version: None,
        },
    );
    let mut s = Schema {
        adapter: "sqlite3".into(),
        framework: "axum".into(),
        tables,
        relationships: vec![],
        options: None,
    };
    let v: Value = serde_json::from_str(&print_schema(&s)).unwrap();
    assert!(v.get("options").is_none());
    assert!(v.get("relationships").is_none());
    let t = v["tables"]["t"].as_object().unwrap();
    assert!(!t.contains_key("unique") && !t.contains_key("search_columns"));

    s.relationships = serde_json::from_value(json!([
        {"parent": "b", "child": "a", "foreignKey": "x"},
        {"parent": "a", "child": "z", "foreignKey": "x"},
        {"parent": "a", "child": "c", "foreignKey": "x"}
    ]))
    .unwrap();
    let v: Value = serde_json::from_str(&print_schema(&s)).unwrap();
    let order: Vec<(&str, &str)> = v["relationships"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["parent"].as_str().unwrap(), r["child"].as_str().unwrap()))
        .collect();
    assert_eq!(order, [("a", "c"), ("a", "z"), ("b", "a")]);
}

#[test]
fn model_meta_excludes_managed_columns_and_sorts_tables() {
    let s = parse_schema(RICH).unwrap();
    let metas = schema_to_model_meta(&s);
    assert_eq!(metas[0].table, "orgs");
    assert_eq!(metas[0].primary_key, "org_id");
    assert_eq!(metas[0].structure.keys().collect::<Vec<_>>(), ["name"]);

    let posts = &metas[1];
    // pk, timestamps, softDelete and auto_increment columns are not part of the structure.
    assert_eq!(
        posts.structure.keys().collect::<Vec<_>>(),
        ["org_id", "title"]
    );
    assert_eq!(posts.option.safe_delete.as_deref(), Some("deleted"));
    assert_eq!(posts.option.created_at.as_deref(), Some("created"));
    assert_eq!(posts.option.modified_at.as_deref(), Some("modified"));
    assert_eq!(posts.option.search_columns, ["title"]);
    assert_eq!(posts.unique, vec![vec!["title", "org_id"]]);
}

#[test]
fn model_meta_skips_required_auto_increment_and_defaults_missing_parts() {
    let mut columns = IndexMap::new();
    columns.insert("seq".to_string(), "required|auto_increment".to_string());
    columns.insert("name".to_string(), "string".to_string());
    let mut tables = IndexMap::new();
    tables.insert(
        "t".to_string(),
        TableDef {
            pk: "id".into(),
            columns,
            unique: None,
            soft_delete: None,
            search_columns: None,
            parent: None,
            timestamps: None,
            hidden: None,
            version: None,
        },
    );
    let s = Schema {
        adapter: "sqlite3".into(),
        framework: "axum".into(),
        tables,
        relationships: vec![],
        options: None,
    };
    // No unique groups at all: nothing to print.
    assert!(!print_schema(&s).contains("\"unique\""));
    let m = &schema_to_model_meta(&s)[0];
    assert_eq!(m.structure.keys().collect::<Vec<_>>(), ["name"]);
    assert!(m.unique.is_empty());
    assert!(m.option.search_columns.is_empty());
    assert!(m.option.created_at.is_none() && m.option.modified_at.is_none());
}

#[test]
fn compile_columns_extracts_flags_base_type_and_encrypted_path() {
    let mut cols = IndexMap::new();
    cols.insert("name".to_string(), "required|string:50".to_string());
    cols.insert("meta.ssn".to_string(), "encrypted|string".to_string());
    cols.insert(
        "secret".to_string(),
        "encrypted|required|integer".to_string(),
    );
    cols.insert("bare".to_string(), "required".to_string());
    let c = compile_columns(&cols);

    assert_eq!((c[0].index, c[0].base_type.as_str()), (0, "string"));
    assert!(c[0].required && !c[0].encrypted && c[0].encrypted_path.is_none());

    assert!(c[1].encrypted && !c[1].required);
    assert_eq!(
        c[1].encrypted_path,
        Some(EncryptedPath {
            parent_col: "meta".into(),
            nested_key: "ssn".into()
        })
    );

    assert_eq!(c[2].base_type, "integer");
    assert!(c[2].encrypted && c[2].required && c[2].encrypted_path.is_none());

    // No type token at all falls back to "string".
    assert_eq!(
        (c[3].name.as_str(), c[3].base_type.as_str()),
        ("bare", "string")
    );
}

#[test]
fn serde_defaults_fill_missing_fields() {
    let s: Schema = serde_json::from_value(json!({
        "adapter": "sqlite3",
        "framework": "axum",
        "tables": {"t": {"columns": {"a": "string"}}},
        "options": {}
    }))
    .unwrap();
    assert_eq!(s.tables["t"].pk, "id");
    assert!(s.relationships.is_empty());
    let o = s.options.unwrap();
    assert_eq!(o.session, "memory");
    assert!(o.rate_limiting && o.helmet && o.logger && o.saas_structure);
    assert!(!o.loki);
    assert_eq!(o.api_base_path, "/api");
    assert_eq!(o.port, 3000);
    assert!(o.output.is_none() && o.encryption.is_none());

    let ts: dbmr::schema::TimestampsDef = serde_json::from_value(json!({})).unwrap();
    assert!(ts.created_at.is_none() && ts.modified_at.is_none());
}
