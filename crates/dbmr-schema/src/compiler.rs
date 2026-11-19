use crate::types::{CompiledColumn, EncryptedPath, ModelMeta, ModelMetaOption, Schema};
use compact_str::CompactString;
use indexmap::IndexMap;
use std::collections::HashSet;

/// Convert parsed schema into model metadata used by generators and runtime models.
#[must_use]
pub fn schema_to_model_meta(schema: &Schema) -> Vec<ModelMeta> {
    let mut table_names: Vec<&String> = schema.tables.keys().collect();
    table_names.sort();

    table_names
        .into_iter()
        .map(|table_name| {
            let table_def = &schema.tables[table_name];
            let mut exclude_set = HashSet::new();
            exclude_set.insert(table_def.pk.as_str());

            if let Some(ts) = &table_def.timestamps {
                if let Some(ca) = &ts.created_at {
                    exclude_set.insert(ca.as_str());
                }
                if let Some(ma) = &ts.modified_at {
                    exclude_set.insert(ma.as_str());
                }
            }

            if let Some(sd) = &table_def.soft_delete {
                exclude_set.insert(sd.as_str());
            }

            let mut structure = IndexMap::new();
            for (col_name, rule) in &table_def.columns {
                if !exclude_set.contains(col_name.as_str()) {
                    let base_type = rule.strip_prefix("required|").unwrap_or(rule.as_str());
                    if base_type == "auto_increment" {
                        continue;
                    }
                    structure.insert(col_name.clone(), rule.clone());
                }
            }

            let option = ModelMetaOption {
                safe_delete: table_def.soft_delete.clone(),
                created_at: table_def
                    .timestamps
                    .as_ref()
                    .and_then(|t| t.created_at.clone()),
                modified_at: table_def
                    .timestamps
                    .as_ref()
                    .and_then(|t| t.modified_at.clone()),
                search_columns: table_def.search_columns.clone().unwrap_or_default(),
                hidden: table_def.hidden.clone().unwrap_or_default(),
                version: table_def.version.clone(),
            };

            ModelMeta {
                table: table_name.clone(),
                structure,
                primary_key: table_def.pk.clone(),
                unique: table_def.unique.clone().unwrap_or_default(),
                option,
            }
        })
        .collect()
}

/// Compile column rules into runtime AST.
#[must_use]
pub fn compile_columns(columns: &IndexMap<String, String>) -> Vec<CompiledColumn> {
    columns
        .iter()
        .enumerate()
        .map(|(index, (name, rule))| {
            let parts: Vec<&str> = rule.split('|').collect();
            let required = parts.contains(&"required");
            let encrypted = parts.contains(&"encrypted");

            let base_type = parts
                .iter()
                .copied()
                .find(|p| *p != "required" && *p != "encrypted")
                .map_or("string", |p| p.split(':').next().unwrap_or(p));

            let encrypted_path = if encrypted && name.contains('.') {
                let segments: Vec<&str> = name.split('.').collect();
                Some(EncryptedPath {
                    parent_col: CompactString::new(segments[0]),
                    nested_key: CompactString::new(segments[1]),
                })
            } else {
                None
            };

            CompiledColumn {
                name: CompactString::new(name),
                index,
                base_type: CompactString::new(base_type),
                required,
                encrypted,
                encrypted_path,
            }
        })
        .collect()
}
