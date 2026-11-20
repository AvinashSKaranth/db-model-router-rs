use crate::schema::types::{RelationshipDef, Schema, SchemaOptions, TableDef, TimestampsDef};
use crate::schema::validator::{validate_schema, SchemaValidationError, ValidationError};
use indexmap::IndexMap;
use std::collections::HashSet;

/// Parse and normalize a schema from raw JSON.
///
/// # Errors
/// Returns [`SchemaValidationError`] if JSON is malformed or violates schema rules.
pub fn parse_schema(input: &str) -> Result<Schema, SchemaValidationError> {
    let raw: serde_json::Value =
        serde_json::from_str(input).map_err(|e| SchemaValidationError {
            errors: vec![ValidationError {
                path: String::new(),
                message: format!("Invalid JSON: {e}"),
            }],
        })?;

    validate_schema(&raw)?;

    let adapter = raw["adapter"].as_str().unwrap_or("sqlite3").to_string();
    let framework = raw["framework"].as_str().unwrap_or("express").to_string();

    let mut tables = IndexMap::new();
    if let Some(tables_obj) = raw["tables"].as_object() {
        for (name, table_val) in tables_obj {
            let pk = table_val["pk"].as_str().unwrap_or("id").to_string();

            let mut columns = IndexMap::new();
            if let Some(cols_obj) = table_val["columns"].as_object() {
                for (col_name, rule_val) in cols_obj {
                    columns.insert(
                        col_name.clone(),
                        rule_val.as_str().unwrap_or("").to_string(),
                    );
                }
            }

            let unique = if let Some(u_val) = table_val.get("unique") {
                if let Some(u_arr) = u_val.as_array() {
                    if u_arr.first().is_some_and(serde_json::Value::is_array) {
                        Some(
                            u_arr
                                .iter()
                                .filter_map(serde_json::Value::as_array)
                                .map(|g| {
                                    g.iter()
                                        .filter_map(serde_json::Value::as_str)
                                        .map(ToString::to_string)
                                        .collect()
                                })
                                .collect(),
                        )
                    } else {
                        Some(vec![u_arr
                            .iter()
                            .filter_map(serde_json::Value::as_str)
                            .map(ToString::to_string)
                            .collect()])
                    }
                } else {
                    Some(vec![vec![pk.clone()]])
                }
            } else {
                Some(vec![vec![pk.clone()]])
            };

            let soft_delete = table_val["softDelete"].as_str().map(ToString::to_string);
            let parent = table_val["parent"].as_str().map(ToString::to_string);
            let hidden = table_val["hidden"].as_array().map(|arr| {
                arr.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(ToString::to_string)
                    .collect()
            });
            let version = table_val["version"].as_str().map(ToString::to_string);

            let search_columns = table_val["search_columns"].as_array().map(|arr| {
                arr.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(ToString::to_string)
                    .collect()
            });

            let timestamps = if let Some(ts_val) = table_val.get("timestamps") {
                Some(TimestampsDef {
                    created_at: ts_val["created_at"].as_str().map(ToString::to_string),
                    modified_at: ts_val["modified_at"].as_str().map(ToString::to_string),
                })
            } else {
                Some(TimestampsDef {
                    created_at: None,
                    modified_at: None,
                })
            };

            tables.insert(
                name.clone(),
                TableDef {
                    pk,
                    columns,
                    unique,
                    soft_delete,
                    search_columns,
                    parent,
                    timestamps,
                    hidden,
                    version,
                },
            );
        }
    }

    // Derive relationships from `parent` references
    let mut derived_relationships = Vec::new();
    let mut seen_rels = HashSet::new();

    for (name, table_def) in &tables {
        if let Some(parent_name) = &table_def.parent {
            if let Some(parent_table) = tables.get(parent_name) {
                let fk = parent_table.pk.clone();
                let key = format!("{parent_name}:{name}:{fk}");
                if !seen_rels.contains(&key) {
                    seen_rels.insert(key);
                    derived_relationships.push(RelationshipDef {
                        parent: parent_name.clone(),
                        child: name.clone(),
                        foreign_key: fk,
                    });
                }
            }
        }
    }

    // Merge explicit relationships
    if let Some(rels_arr) = raw["relationships"].as_array() {
        for rel_val in rels_arr {
            let parent = rel_val["parent"].as_str().unwrap_or("").to_string();
            let child = rel_val["child"].as_str().unwrap_or("").to_string();
            let fk = rel_val["foreignKey"].as_str().unwrap_or("").to_string();
            let key = format!("{parent}:{child}:{fk}");
            if !seen_rels.contains(&key) {
                seen_rels.insert(key);
                derived_relationships.push(RelationshipDef {
                    parent,
                    child,
                    foreign_key: fk,
                });
            }
        }
    }

    // Options normalization: always populate defaults per §28.3
    let opt_val = raw.get("options");
    let opt_obj = opt_val.and_then(serde_json::Value::as_object);
    let options = Some(SchemaOptions {
        session: opt_obj
            .and_then(|o| o.get("session"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("memory")
            .to_string(),
        rate_limiting: opt_obj
            .and_then(|o| o.get("rateLimiting"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        helmet: opt_obj
            .and_then(|o| o.get("helmet"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        logger: opt_obj
            .and_then(|o| o.get("logger"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        loki: opt_obj
            .and_then(|o| o.get("loki"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        output: opt_obj
            .and_then(|o| o.get("output"))
            .and_then(serde_json::Value::as_str)
            .map(ToString::to_string),
        saas_structure: opt_obj
            .and_then(|o| o.get("saasStructure"))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true),
        api_base_path: opt_obj
            .and_then(|o| o.get("apiBasePath"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or("/api")
            .to_string(),
        #[allow(clippy::cast_possible_truncation)]
        port: opt_obj
            .and_then(|o| o.get("port"))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(3000) as u16,
        // Already validated above; keep it so print/parse round-trips do not drop it.
        encryption: opt_obj
            .and_then(|o| o.get("encryption"))
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
    });

    Ok(Schema {
        adapter,
        framework,
        tables,
        relationships: derived_relationships,
        options,
    })
}
