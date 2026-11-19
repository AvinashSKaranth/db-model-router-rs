use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;
use thiserror::Error;

static COLUMN_RULE_RE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::unwrap_used)]
    Regex::new(r"^(encrypted\|)?(required\|)?(string|integer|numeric|boolean|object|datetime|auto_increment)(:[^|]+)?(\|[^|]+)*$").unwrap()
});

pub static VALID_ADAPTERS: &[&str] = &[
    "mysql",
    "mariadb",
    "postgres",
    "sqlite3",
    "mongodb",
    "mssql",
    "cockroachdb",
    "oracle",
    "redis",
    "dynamodb",
];

pub static VALID_FRAMEWORKS: &[&str] = &["express", "ultimate-express", "axum"];

pub static ENCRYPTABLE_TYPES: &[&str] = &["string", "integer", "numeric", "boolean", "datetime"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationError {
    pub path: String,
    pub message: String,
}

#[derive(Debug, Error)]
#[error("Schema validation failed: {}", .errors.iter().map(|e| if e.path.is_empty() { e.message.clone() } else { format!("{}: {}", e.path, e.message) }).collect::<Vec<_>>().join("; "))]
pub struct SchemaValidationError {
    pub errors: Vec<ValidationError>,
}

pub fn is_encrypted_rule(rule: &str) -> bool {
    rule.split('|').any(|part| part == "encrypted")
}

pub fn base_type_of(rule: &str) -> Option<&str> {
    for part in rule.split('|') {
        if part == "required" || part == "encrypted" {
            continue;
        }
        let token = part.split(':').next().unwrap_or(part);
        match token {
            "string" | "integer" | "numeric" | "boolean" | "object" | "datetime"
            | "auto_increment" => return Some(token),
            _ => {}
        }
    }
    None
}

/// Validate a raw JSON schema object.
#[must_use]
pub fn validate_schema(raw: &serde_json::Value) -> Result<(), SchemaValidationError> {
    let mut errors = Vec::new();

    let Some(obj) = raw.as_object() else {
        errors.push(ValidationError {
            path: String::new(),
            message: "Schema must be a non-null object".to_string(),
        });
        return Err(SchemaValidationError { errors });
    };

    // 1. Adapter validation
    match obj.get("adapter").and_then(serde_json::Value::as_str) {
        Some(adapter) => {
            if !VALID_ADAPTERS.contains(&adapter) {
                errors.push(ValidationError {
                    path: "adapter".to_string(),
                    message: format!(
                        "Invalid adapter \"{adapter}\". Must be one of: {}",
                        VALID_ADAPTERS.join(", ")
                    ),
                });
            }
        }
        None => {
            errors.push(ValidationError {
                path: "adapter".to_string(),
                message: "adapter is required and must be a string".to_string(),
            });
        }
    }

    // 2. Framework validation
    match obj.get("framework").and_then(serde_json::Value::as_str) {
        Some(framework) => {
            if !VALID_FRAMEWORKS.contains(&framework) {
                errors.push(ValidationError {
                    path: "framework".to_string(),
                    message: format!(
                        "Invalid framework \"{framework}\". Must be one of: {}",
                        VALID_FRAMEWORKS.join(", ")
                    ),
                });
            }
        }
        None => {
            errors.push(ValidationError {
                path: "framework".to_string(),
                message: "framework is required and must be a string".to_string(),
            });
        }
    }

    // 3. Tables validation
    let mut table_names = HashSet::new();
    match obj.get("tables").and_then(serde_json::Value::as_object) {
        Some(tables) => {
            for k in tables.keys() {
                table_names.insert(k.clone());
            }
            validate_tables(tables, &mut errors);
        }
        None => {
            errors.push(ValidationError {
                path: "tables".to_string(),
                message: "tables is required and must be an object".to_string(),
            });
        }
    }

    // 4. Relationships validation
    if let Some(relationships) = obj.get("relationships") {
        if let Some(rel_arr) = relationships.as_array() {
            validate_relationships(rel_arr, &table_names, &mut errors);
        } else {
            errors.push(ValidationError {
                path: "relationships".to_string(),
                message: "relationships must be an array".to_string(),
            });
        }
    }

    // 5. Options validation
    if let Some(options) = obj.get("options") {
        if let Some(opt_obj) = options.as_object() {
            validate_options(opt_obj, &mut errors);
        } else {
            errors.push(ValidationError {
                path: "options".to_string(),
                message: "options must be an object".to_string(),
            });
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(SchemaValidationError { errors })
    }
}

fn validate_tables(
    tables: &serde_json::Map<String, serde_json::Value>,
    errors: &mut Vec<ValidationError>,
) {
    let table_names: HashSet<&str> = tables.keys().map(String::as_str).collect();

    for (table_name, table_val) in tables {
        let base_path = format!("tables.{table_name}");
        let Some(table_obj) = table_val.as_object() else {
            errors.push(ValidationError {
                path: base_path,
                message: format!("Table \"{table_name}\" must be an object"),
            });
            continue;
        };

        let Some(columns) = table_obj
            .get("columns")
            .and_then(serde_json::Value::as_object)
        else {
            errors.push(ValidationError {
                path: format!("{base_path}.columns"),
                message: format!("Table \"{table_name}\" must have a columns object"),
            });
            continue;
        };

        let column_names: HashSet<&str> = columns.keys().map(String::as_str).collect();
        let pk = table_obj
            .get("pk")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("id");

        // Validate each column rule
        for (col_name, rule_val) in columns {
            let rule = rule_val.as_str().unwrap_or("");
            if !COLUMN_RULE_RE.is_match(rule) {
                errors.push(ValidationError {
                    path: format!("{base_path}.columns.{col_name}"),
                    message: format!(
                        "Invalid column rule \"{rule}\" for column \"{col_name}\". Must match pattern: (encrypted|)?(required|)?(string|integer|numeric|boolean|object|datetime|auto_increment)"
                    ),
                });
            }

            let encrypted = is_encrypted_rule(rule);
            let is_dotted = col_name.contains('.');

            if is_dotted {
                let parts: Vec<&str> = col_name.split('.').collect();
                let parent = parts[0];
                if !columns.contains_key(parent) {
                    errors.push(ValidationError {
                        path: format!("{base_path}.columns.{col_name}"),
                        message: format!("Dotted field \"{col_name}\" has no parent column \"{parent}\" in table \"{table_name}\""),
                    });
                    continue;
                }
                let parent_rule = columns[parent].as_str().unwrap_or("");
                let parent_type = base_type_of(parent_rule).unwrap_or("");
                if parent_type != "object" {
                    errors.push(ValidationError {
                        path: format!("{base_path}.columns.{col_name}"),
                        message: format!("Dotted field \"{col_name}\" requires parent column \"{parent}\" to be declared as \"object\" (found \"{parent_type}\")"),
                    });
                }
                if parts.len() > 2 {
                    errors.push(ValidationError {
                        path: format!("{base_path}.columns.{col_name}"),
                        message: format!("Dotted field \"{col_name}\" must use at most one level of nesting (parent.key)"),
                    });
                }
                if encrypted {
                    let col_type = base_type_of(rule).unwrap_or("");
                    if !ENCRYPTABLE_TYPES.contains(&col_type) {
                        errors.push(ValidationError {
                            path: format!("{base_path}.columns.{col_name}"),
                            message: format!("Dotted field \"{col_name}\" uses type \"{col_type}\" which cannot be encrypted. Supported: {}", ENCRYPTABLE_TYPES.join(", ")),
                        });
                    }
                }
            } else if encrypted {
                let col_type = base_type_of(rule).unwrap_or("");
                if !ENCRYPTABLE_TYPES.contains(&col_type) {
                    errors.push(ValidationError {
                        path: format!("{base_path}.columns.{col_name}"),
                        message: format!("Column \"{col_name}\" uses type \"{col_type}\" which cannot be encrypted. Supported: {}", ENCRYPTABLE_TYPES.join(", ")),
                    });
                }
            }
        }

        // Validate PK
        if pk.contains('.') {
            errors.push(ValidationError {
                path: format!("{base_path}.pk"),
                message: format!("pk cannot reference a dotted (virtual JSON) field \"{pk}\""),
            });
        } else if let Some(rule_val) = columns.get(pk) {
            if is_encrypted_rule(rule_val.as_str().unwrap_or("")) {
                errors.push(ValidationError {
                    path: format!("{base_path}.pk"),
                    message: format!("pk cannot reference an encrypted column \"{pk}\" (encrypted fields cannot be used to look up rows)"),
                });
            }
        }

        // Validate softDelete
        if let Some(sd_val) = table_obj.get("softDelete") {
            if !sd_val.is_null() {
                if let Some(sd) = sd_val.as_str() {
                    if sd.contains('.') {
                        errors.push(ValidationError {
                            path: format!("{base_path}.softDelete"),
                            message: format!("softDelete cannot reference a dotted (virtual JSON) field \"{sd}\""),
                        });
                    } else if !column_names.contains(sd) {
                        errors.push(ValidationError {
                            path: format!("{base_path}.softDelete"),
                            message: format!("softDelete column \"{sd}\" does not exist in table \"{table_name}\""),
                        });
                    } else if let Some(rule) = columns.get(sd) {
                        if is_encrypted_rule(rule.as_str().unwrap_or("")) {
                            errors.push(ValidationError {
                                path: format!("{base_path}.softDelete"),
                                message: format!("softDelete cannot reference an encrypted column \"{sd}\" (encrypted fields cannot be filtered)"),
                            });
                        }
                    }
                } else {
                    errors.push(ValidationError {
                        path: format!("{base_path}.softDelete"),
                        message: format!("softDelete must be a string in table \"{table_name}\""),
                    });
                }
            }
        }

        // Validate unique
        if let Some(unique_val) = table_obj.get("unique") {
            if let Some(unique_arr) = unique_val.as_array() {
                let groups: Vec<&[serde_json::Value]> =
                    if unique_arr.first().is_some_and(serde_json::Value::is_array) {
                        unique_arr
                            .iter()
                            .filter_map(serde_json::Value::as_array)
                            .map(Vec::as_slice)
                            .collect()
                    } else {
                        vec![unique_arr.as_slice()]
                    };

                for (g, group) in groups.iter().enumerate() {
                    for (i, entry_val) in group.iter().enumerate() {
                        let group_path =
                            if unique_arr.first().is_some_and(serde_json::Value::is_array) {
                                format!("{base_path}.unique[{g}][{i}]")
                            } else {
                                format!("{base_path}.unique[{i}]")
                            };

                        if let Some(entry) = entry_val.as_str() {
                            if entry.contains('.') {
                                errors.push(ValidationError {
                                    path: group_path.clone(),
                                    message: format!("unique cannot reference a dotted (virtual JSON) field \"{entry}\""),
                                });
                            } else if let Some(rule) = columns.get(entry) {
                                if is_encrypted_rule(rule.as_str().unwrap_or("")) {
                                    errors.push(ValidationError {
                                        path: group_path.clone(),
                                        message: format!("unique entry \"{entry}\" cannot reference an encrypted column \"{entry}\" (encrypted values are never equal, so uniqueness cannot be enforced)"),
                                    });
                                }
                            } else if entry != pk && !column_names.contains(entry) {
                                errors.push(ValidationError {
                                    path: group_path,
                                    message: format!("unique entry \"{entry}\" does not match any column or the primary key \"{pk}\" in table \"{table_name}\""),
                                });
                            }
                        } else {
                            errors.push(ValidationError {
                                path: group_path,
                                message: format!(
                                    "unique entry must be a string in table \"{table_name}\""
                                ),
                            });
                        }
                    }
                }
            } else {
                errors.push(ValidationError {
                    path: format!("{base_path}.unique"),
                    message: format!("unique must be an array in table \"{table_name}\""),
                });
            }
        }

        // Validate search_columns
        if let Some(search_val) = table_obj.get("search_columns") {
            if !search_val.is_null() {
                if let Some(search_arr) = search_val.as_array() {
                    for (i, col_val) in search_arr.iter().enumerate() {
                        let col_path = format!("{base_path}.search_columns[{i}]");
                        if let Some(col) = col_val.as_str() {
                            if col.is_empty() {
                                errors.push(ValidationError {
                                    path: col_path,
                                    message: format!("search_columns entry must be a non-empty string in table \"{table_name}\""),
                                });
                            } else if col.contains('.') {
                                errors.push(ValidationError {
                                    path: col_path,
                                    message: format!("search_columns cannot reference a dotted (virtual JSON) field \"{col}\""),
                                });
                            } else if !column_names.contains(col) {
                                errors.push(ValidationError {
                                    path: col_path,
                                    message: format!("search_columns entry \"{col}\" does not exist in table \"{table_name}\""),
                                });
                            } else if let Some(rule) = columns.get(col) {
                                if is_encrypted_rule(rule.as_str().unwrap_or("")) {
                                    errors.push(ValidationError {
                                        path: col_path,
                                        message: format!("search_columns cannot reference an encrypted column \"{col}\" (encrypted fields cannot be searched)"),
                                    });
                                }
                            }
                        } else {
                            errors.push(ValidationError {
                                path: col_path,
                                message: format!("search_columns entry must be a non-empty string in table \"{table_name}\""),
                            });
                        }
                    }
                } else {
                    errors.push(ValidationError {
                        path: format!("{base_path}.search_columns"),
                        message: format!(
                            "search_columns must be an array of strings in table \"{table_name}\""
                        ),
                    });
                }
            }
        }

        // Validate hidden (array of existing columns) and version (existing column)
        if let Some(hidden_val) = table_obj.get("hidden").filter(|v| !v.is_null()) {
            match hidden_val.as_array() {
                Some(arr) => {
                    for (i, col_val) in arr.iter().enumerate() {
                        match col_val.as_str() {
                            Some(col) if column_names.contains(col) => {}
                            Some(col) => errors.push(ValidationError {
                                path: format!("{base_path}.hidden[{i}]"),
                                message: format!(
                                    "hidden entry \"{col}\" does not exist in table \"{table_name}\""
                                ),
                            }),
                            None => errors.push(ValidationError {
                                path: format!("{base_path}.hidden[{i}]"),
                                message: format!(
                                    "hidden entry must be a string in table \"{table_name}\""
                                ),
                            }),
                        }
                    }
                }
                None => errors.push(ValidationError {
                    path: format!("{base_path}.hidden"),
                    message: format!(
                        "hidden must be an array of strings in table \"{table_name}\""
                    ),
                }),
            }
        }
        if let Some(version_val) = table_obj.get("version").filter(|v| !v.is_null()) {
            match version_val.as_str() {
                Some(col) if column_names.contains(col) => {}
                _ => errors.push(ValidationError {
                    path: format!("{base_path}.version"),
                    message: format!(
                        "version must name an existing integer column in table \"{table_name}\""
                    ),
                }),
            }
        }

        // Validate parent
        if let Some(parent_val) = table_obj.get("parent") {
            if !parent_val.is_null() {
                if let Some(parent) = parent_val.as_str() {
                    if !table_names.contains(parent) {
                        errors.push(ValidationError {
                            path: format!("{base_path}.parent"),
                            message: format!("parent \"{parent}\" does not reference an existing table in table \"{table_name}\""),
                        });
                    }
                } else {
                    errors.push(ValidationError {
                        path: format!("{base_path}.parent"),
                        message: format!(
                            "parent must be a string or null in table \"{table_name}\""
                        ),
                    });
                }
            }
        }
    }
}

fn validate_relationships(
    relationships: &[serde_json::Value],
    table_names: &HashSet<String>,
    errors: &mut Vec<ValidationError>,
) {
    for (i, rel_val) in relationships.iter().enumerate() {
        let base_path = format!("relationships[{i}]");
        let Some(rel_obj) = rel_val.as_object() else {
            errors.push(ValidationError {
                path: base_path,
                message: "Each relationship must be an object".to_string(),
            });
            continue;
        };

        match rel_obj.get("parent").and_then(serde_json::Value::as_str) {
            Some(parent) => {
                if !table_names.contains(parent) {
                    errors.push(ValidationError {
                        path: format!("{base_path}.parent"),
                        message: format!(
                            "Relationship parent \"{parent}\" does not reference an existing table"
                        ),
                    });
                }
            }
            None => {
                errors.push(ValidationError {
                    path: format!("{base_path}.parent"),
                    message: "Relationship must have a parent string".to_string(),
                });
            }
        }

        match rel_obj.get("child").and_then(serde_json::Value::as_str) {
            Some(child) => {
                if !table_names.contains(child) {
                    errors.push(ValidationError {
                        path: format!("{base_path}.child"),
                        message: format!(
                            "Relationship child \"{child}\" does not reference an existing table"
                        ),
                    });
                }
            }
            None => {
                errors.push(ValidationError {
                    path: format!("{base_path}.child"),
                    message: "Relationship must have a child string".to_string(),
                });
            }
        }

        if rel_obj
            .get("foreignKey")
            .and_then(serde_json::Value::as_str)
            .is_none()
        {
            errors.push(ValidationError {
                path: format!("{base_path}.foreignKey"),
                message: "Relationship must have a foreignKey string".to_string(),
            });
        }
    }
}

fn validate_options(
    options: &serde_json::Map<String, serde_json::Value>,
    errors: &mut Vec<ValidationError>,
) {
    if let Some(out_val) = options.get("output") {
        if !out_val.is_null() && (out_val.as_str().is_none_or(str::is_empty)) {
            errors.push(ValidationError {
                path: "options.output".to_string(),
                message: "options.output must be a non-empty string path or null".to_string(),
            });
        }
    }

    if let Some(saas_val) = options.get("saasStructure") {
        if !saas_val.is_boolean() {
            errors.push(ValidationError {
                path: "options.saasStructure".to_string(),
                message: "options.saasStructure must be a boolean".to_string(),
            });
        }
    }

    if let Some(base_val) = options.get("apiBasePath") {
        if base_val.as_str().is_none_or(|s| !s.starts_with('/')) {
            errors.push(ValidationError {
                path: "options.apiBasePath".to_string(),
                message: "options.apiBasePath must be a string starting with \"/\" (e.g. \"/api\")"
                    .to_string(),
            });
        }
    }

    if let Some(port_val) = options.get("port") {
        if port_val.as_u64().is_none_or(|p| p == 0 || p > 65535) {
            errors.push(ValidationError {
                path: "options.port".to_string(),
                message: "options.port must be a positive integer".to_string(),
            });
        }
    }

    if let Some(sess_val) = options.get("session") {
        if let Some(s) = sess_val.as_str() {
            if !["memory", "redis", "database"].contains(&s) {
                errors.push(ValidationError {
                    path: "options.session".to_string(),
                    message: "options.session must be one of: memory, redis, database".to_string(),
                });
            }
        }
    }

    for key in &["rateLimiting", "helmet", "logger", "loki"] {
        if let Some(val) = options.get(*key) {
            if !val.is_boolean() {
                errors.push(ValidationError {
                    path: format!("options.{key}"),
                    message: format!("options.{key} must be a boolean"),
                });
            }
        }
    }

    if let Some(enc_val) = options.get("encryption") {
        let Some(enc_obj) = enc_val.as_object() else {
            errors.push(ValidationError {
                path: "options.encryption".to_string(),
                message: "options.encryption must be an object { key, version, keys? }".to_string(),
            });
            return;
        };

        if enc_obj
            .get("key")
            .and_then(serde_json::Value::as_str)
            .is_none()
        {
            errors.push(ValidationError {
                path: "options.encryption.key".to_string(),
                message:
                    "options.encryption.key must be a string (env:VAR or raw:<base64 32-byte key>)"
                        .to_string(),
            });
        }

        if enc_obj
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .is_none_or(|v| v < 1)
        {
            errors.push(ValidationError {
                path: "options.encryption.version".to_string(),
                message: "options.encryption.version must be a positive integer".to_string(),
            });
        }
    }
}
