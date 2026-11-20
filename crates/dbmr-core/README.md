# dbmr-core

Core foundation of `db-model-router` (`dbmr`):
- `Model` and `ModelBuilder` with declarative schema validation
- AST query filters and SQL/NoSQL where-clause generation
- Row-level permissions and security `Scope`
- AES-256-GCM column encryption with key rotation
- Lifecycle hooks (`before_save`, `after_save`, `before_remove`, `after_remove`)
- Optimistic locking and version management
- Strongly typed model access (`TypedModel<T>`)
- Transaction handles and event dispatching

Part of the [db-model-router](https://crates.io/crates/db-model-router) workspace.
