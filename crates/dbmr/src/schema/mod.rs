//! `dbmr-schema`: Pure schema parsing, validation, normalization, and printing.

pub mod compiler;
pub mod parser;
pub mod printer;
pub mod types;
pub mod validator;

pub use compiler::{compile_columns, schema_to_model_meta};
pub use parser::parse_schema;
pub use printer::print_schema;
pub use types::{
    CompiledColumn, EncryptedPath, EncryptionOptions, ModelMeta, ModelMetaOption, RelationshipDef,
    Schema, SchemaOptions, TableDef, TimestampsDef,
};
pub use validator::{validate_schema, SchemaValidationError, ValidationError};
