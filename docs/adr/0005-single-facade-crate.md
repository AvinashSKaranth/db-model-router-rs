# ADR-0005: Single published facade crate, feature-gated adapters

**Status:** Accepted (2026-11-08, user decision)
**Referenced by:** BRD §5.1/§7.3, task P0-05/P1-01/P1-03/P12-02/P12-03

## Context
Options considered: (a) one crate per adapter published separately,
(b) `dbmr-adapters` with all adapters feature-gated but still publishes multiple
crates, (c) one facade `dbmr` with everything. User requirement: easy to install
(`cargo add dbmr`), single artifact bundle, sqlite in default features, no
multiple library bundles.

## Decision
- **One published crate: `dbmr`**. Workspace members `crates/dbmr-core`,
  `crates/dbmr-schema`, `crates/dbmr-adapters`, `crates/dbmr-http`,
  `crates/dbmr-kafka`, `crates/dbmr-dbmanager`, `crates/dbmr-cli` are all
  `publish = false`.
- `dbmr` re-exports core + schema + http; adapters are modules behind features.
- Default features: `["sqlite"]`. Other 9 adapters behind `postgres`, `mysql`,
  `mariadb`, `mssql`, `oracle`, `cockroachdb`, `mongodb`, `redis`, `dynamodb`.
- `kafka` and `db-manager` behind `kafka` and `db-manager-web` features
  (empty-by-default; never required for library use).
- CLI binary published as `dbmr` bin from the same crate (`[[bin]] name = "dbmr"`).
- MSRV 1.85, edition 2021, license MIT.

## Consequences
+ Trivial adoption: `cargo add dbmr` (SQLite) or `cargo add dbmr --features postgres`.
+ One bundle on crates.io; `cargo install dbmr` for the CLI.
+ Internal crate boundaries stay free to evolve without a public registry dance.
− Larger default dependency tree than per-crate install (mitigated: only sqlite
  in default).
− Docs must be written against the facade (`dbmr::model`, `dbmr::Adapter`), not
  internal crates.
