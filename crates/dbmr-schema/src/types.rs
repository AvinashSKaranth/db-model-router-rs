use compact_str::CompactString;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// Top-level schema specification matching `dbmr.schema.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Schema {
    pub adapter: String,
    pub framework: String,
    pub tables: IndexMap<String, TableDef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relationships: Vec<RelationshipDef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<SchemaOptions>,
}

/// Table definition inside schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableDef {
    #[serde(default = "default_id")]
    pub pk: String,
    pub columns: IndexMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unique: Option<Vec<Vec<String>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soft_delete: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_columns: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamps: Option<TimestampsDef>,
    /// Columns never returned to clients (`Model::hidden`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden: Option<Vec<String>>,
    /// Optimistic-lock column (`Model::version_column`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

fn default_id() -> String {
    "id".to_string()
}

/// Timestamp column overrides.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TimestampsDef {
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub modified_at: Option<String>,
}

/// Relationship definition between tables.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelationshipDef {
    pub parent: String,
    pub child: String,
    #[serde(rename = "foreignKey")]
    pub foreign_key: String,
}

/// Buildout options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SchemaOptions {
    #[serde(default = "default_session")]
    pub session: String,
    #[serde(default = "default_true", rename = "rateLimiting")]
    pub rate_limiting: bool,
    #[serde(default = "default_true")]
    pub helmet: bool,
    #[serde(default = "default_true")]
    pub logger: bool,
    #[serde(default = "default_false")]
    pub loki: bool,
    #[serde(default)]
    pub output: Option<String>,
    #[serde(default = "default_true", rename = "saasStructure")]
    pub saas_structure: bool,
    #[serde(default = "default_api_base", rename = "apiBasePath")]
    pub api_base_path: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encryption: Option<EncryptionOptions>,
}

fn default_session() -> String {
    "memory".to_string()
}
fn default_true() -> bool {
    true
}
fn default_false() -> bool {
    false
}
fn default_api_base() -> String {
    "/api".to_string()
}
fn default_port() -> u16 {
    3000
}

/// Encryption configuration inside schema options.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncryptionOptions {
    pub key: String,
    pub version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<IndexMap<String, String>>,
}

/// Normalized model metadata for code and runtime generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelMeta {
    pub table: String,
    pub structure: IndexMap<String, String>,
    pub primary_key: String,
    pub unique: Vec<Vec<String>>,
    pub option: ModelMetaOption,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelMetaOption {
    pub safe_delete: Option<String>,
    pub created_at: Option<String>,
    pub modified_at: Option<String>,
    pub search_columns: Vec<String>,
    #[serde(default)]
    pub hidden: Vec<String>,
    #[serde(default)]
    pub version: Option<String>,
}

/// Compiled column rule for runtime validation and execution.
#[derive(Debug, Clone, PartialEq)]
pub struct CompiledColumn {
    pub name: CompactString,
    pub index: usize,
    pub base_type: CompactString,
    pub required: bool,
    pub encrypted: bool,
    pub encrypted_path: Option<EncryptedPath>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EncryptedPath {
    pub parent_col: CompactString,
    pub nested_key: CompactString,
}
