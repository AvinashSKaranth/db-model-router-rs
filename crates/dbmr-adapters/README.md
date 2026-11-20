# dbmr-adapters

Database adapters for `db-model-router` (`dbmr`).
Provides unified async execution and query compilation across 10 databases:

- **SQLite** (`sqlx`)
- **PostgreSQL** & **CockroachDB** (`sqlx`)
- **MySQL** & **MariaDB** (`sqlx`)
- **MSSQL** (`tiberius`)
- **Oracle** (`oracle-rs`)
- **MongoDB** (`mongodb`)
- **Redis** (`redis`)
- **DynamoDB** (`reqwest` with AWS SigV4)

Part of the [db-model-router](https://crates.io/crates/db-model-router) workspace.
