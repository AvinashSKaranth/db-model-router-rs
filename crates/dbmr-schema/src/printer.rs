use crate::types::Schema;
use serde_json::{json, Map, Value};

/// Serialize a parsed schema into formatted JSON.
///
/// - Tables sorted alphabetically
/// - Relationships sorted by `[parent, child]`
/// - 2-space indentation with trailing newline
#[must_use]
pub fn print_schema(schema: &Schema) -> String {
    let mut out = Map::new();
    out.insert("adapter".to_string(), json!(schema.adapter));
    out.insert("framework".to_string(), json!(schema.framework));

    if let Some(opts) = &schema.options {
        #[allow(clippy::unwrap_used)]
        out.insert("options".to_string(), serde_json::to_value(opts).unwrap());
    }

    let mut sorted_table_names: Vec<&String> = schema.tables.keys().collect();
    sorted_table_names.sort();

    let mut tables = Map::new();
    for name in sorted_table_names {
        let table = &schema.tables[name];
        let mut table_def = Map::new();
        #[allow(clippy::unwrap_used)]
        table_def.insert(
            "columns".to_string(),
            serde_json::to_value(&table.columns).unwrap(),
        );
        table_def.insert("pk".to_string(), json!(table.pk));

        // Preserve unique if not default [pk]
        let default_unique = vec![vec![table.pk.clone()]];
        if let Some(u) = &table.unique {
            if u != &default_unique {
                table_def.insert("unique".to_string(), json!(u));
            }
        }

        if let Some(sd) = &table.soft_delete {
            table_def.insert("softDelete".to_string(), json!(sd));
        }

        if let Some(sc) = &table.search_columns {
            if !sc.is_empty() {
                table_def.insert("search_columns".to_string(), json!(sc));
            }
        }

        if let Some(p) = &table.parent {
            table_def.insert("parent".to_string(), json!(p));
        } else {
            table_def.insert("parent".to_string(), Value::Null);
        }

        if let Some(ts) = &table.timestamps {
            if ts.created_at.is_some() || ts.modified_at.is_some() {
                #[allow(clippy::unwrap_used)]
                table_def.insert("timestamps".to_string(), serde_json::to_value(ts).unwrap());
            }
        }

        tables.insert(name.clone(), Value::Object(table_def));
    }

    out.insert("tables".to_string(), Value::Object(tables));

    if !schema.relationships.is_empty() {
        let mut sorted_rels = schema.relationships.clone();
        sorted_rels.sort_by(|a, b| a.parent.cmp(&b.parent).then_with(|| a.child.cmp(&b.child)));
        #[allow(clippy::unwrap_used)]
        out.insert(
            "relationships".to_string(),
            serde_json::to_value(&sorted_rels).unwrap(),
        );
    }

    #[allow(clippy::unwrap_used)]
    let mut s = serde_json::to_string_pretty(&Value::Object(out)).unwrap();
    s.push('\n');
    s
}
