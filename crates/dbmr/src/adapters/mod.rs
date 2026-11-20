//! `dbmr-adapters`: Feature-gated multi-engine database adapters.

pub mod compiler;

#[cfg(feature = "dynamodb")]
pub mod dynamodb;
#[cfg(feature = "mongodb")]
pub mod mongo_adapter;
#[cfg(feature = "mssql")]
pub mod mssql;
#[cfg(feature = "mysql")]
pub mod mysql;
#[cfg(feature = "oracle")]
pub mod oracle;
#[cfg(feature = "postgres")]
pub mod postgres;
#[cfg(feature = "redis")]
pub mod redis_adapter;
#[cfg(feature = "sqlite")]
pub mod sqlite;

pub use crate::core::adapter::{DatabaseAdapter, TransactionAdapter};

#[cfg(feature = "sqlite")]
pub use sqlite::SqliteAdapter;

#[cfg(feature = "postgres")]
pub use postgres::PostgresAdapter;

#[cfg(feature = "mysql")]
pub use mysql::MySqlAdapter;

#[cfg(feature = "redis")]
pub use redis_adapter::RedisAdapter;

#[cfg(feature = "mongodb")]
pub use mongo_adapter::MongoAdapter;

#[cfg(feature = "mssql")]
pub use mssql::MssqlAdapter;

#[cfg(feature = "dynamodb")]
pub use dynamodb::DynamoDbAdapter;

#[cfg(feature = "oracle")]
pub use oracle::OracleAdapter;

#[cfg(feature = "cockroachdb")]
pub type CockroachAdapter = PostgresAdapter;

#[cfg(feature = "mariadb")]
pub type MariaDbAdapter = MySqlAdapter;
