# ADR-0002: `init` generates a Rust/Axum project

**Status:** Accepted (2026-11-08, user decision)
**Referenced by:** BRD §9.2/§29.4/§28.8, task P0-02/P7-19/P7-21

## Context
Current Node `init` scaffolds a Node/Express ESM project from `dbmr.schema.json`.
The Rust rewrite's stated goal is a coherent single-toolchain ecosystem; generating
Node code from a Rust CLI re-introduces the split we set out to remove.

## Decision
`db-model-router-rs init` generates a **Rust/Axum** project by default:

- Layout: workspace crate per `options`: `app` (bin), `models`, `migrations`,
  `routes`, `middleware`, `tests`, OpenAPI spec artifact, `Cargo.toml`, `.env`,
  Dockerfile/.dockerignore/docker-compose as needed, `.gitignore`, Cargo.lock
  committed.
- Auth/SaaS structure when `saasStructure=true`: tenants/users/roles/
  role_permissions/webhooks/webhook_logs, middleware, seeds, migrations —
  same table set as Node, expressed as Rust modules/models.
- Secrets generated once and shared between `.env` and compose.
- `--target=node` is **not** implemented in v1; a Node-emitting backend is a
  later, explicitly scoped feature (would double golden-test surface).
- Refusal semantics unchanged: refuses if artifacts exist (unless `--dry-run`).

## Consequences
+ Golden tests assert on Rust output; one toolchain for build/test/CI.
+ Generated projects consume the workspace `dbmr` facade directly (path dep or
  registry).
− Node users cannot migrate `init` output byte-for-byte; migration guide maps
  concepts 1:1.
