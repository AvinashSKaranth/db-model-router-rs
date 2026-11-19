# Architecture Decision Records — db-model-router-rs

| ADR | Title | Status |
|---|---|---|
| [0001](0001-rust-native-api.md) | Rust-native API, behavior parity | Accepted |
| [0002](0002-init-generates-rust-axum.md) | `init` generates Rust/Axum | Superseded by 0013 |
| [0003](0003-db-manager-webserver-ui.md) | db-manager ships webserver with embedded UI | Superseded by 0013 |
| [0004](0004-migrations-sql-only-no-js.md) | SQL-only migrations; never eval JS | Superseded by 0013 |
| [0005](0005-single-facade-crate.md) | Single published `dbmr` facade, feature-gated adapters | Accepted |
| [0006](0006-json-decimal-date-fidelity.md) | JSON, decimal (`rust_decimal`), and date fidelity | Accepted |
| [0007](0007-error-exit-code-contract.md) | Typed `AppError` and CLI exit-code contract | Accepted (CLI part superseded by 0013) |
| [0008](0008-global-db-oncelock-shim.md) | Global `db` / `init()` singleton backed by OnceLock | Accepted |
| [0009](0009-defects-fix-vs-preserve.md) | Fix D2/D3/D8/D10/D11/D15/D17/D19; preserve rest | Accepted |
| [0010](0010-ci-all-10-databases.md) | CI runs all 10 database services | Amended by 0016 |
| [0011](0011-benchmark-contenders-budgets.md) | Differential benchmark contenders and release budgets | Accepted |
| [0012](0012-node-interoperability-stance.md) | Node interoperability stance for v1 | Accepted |
| [0013](0013-library-scope-model-and-router-only.md) | Library = model + router; CLI/db-manager/migrations extracted | Accepted |
| [0014](0014-axum-only-no-actix.md) | Axum only; no Actix feature | Accepted |
| [0015](0015-rskafka-and-oracle-rs.md) | Pure-Rust drivers: `rskafka`, `oracle-rs` | Accepted |
| [0016](0016-nosql-adapters-experimental.md) | Redis/DynamoDB experimental; transactions on SQL adapters | Accepted (transaction scope widened; see note) |
| [0017](0017-row-scope-replaces-payload-overrides.md) | Row-level scope, enforced by the model, replaces payload override rules | Accepted |
