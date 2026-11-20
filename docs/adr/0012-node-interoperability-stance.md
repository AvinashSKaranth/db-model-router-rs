# ADR-0012: Node interoperability stance for v1

**Status:** Accepted (2026-11-08, user decision)
**Referenced by:** BRD §35.12, task P0-12/P11-05

## Context
The previous library was written in JavaScript for Node.js. When undertaking this Rust rewrite, an option was to provide Node.js bindings via N-API / `napi-rs` so existing Node applications could call into the Rust library.

## Decision
1. **v1 is pure Rust**:
   - No N-API or Neon bindings are bundled in the `dbmr` crate for initial release.
   - The focus of v1 is empowering Rust web development directly with native models, Axum routers, CLI scaffolding, and type generation.
2. **Migration & Adoption**:
   - Provide a comprehensive migration guide (`docs/migration-from-node.md`) for teams transitioning from Node `db-model-router` to Rust.
   - Golden test fixtures and schema format (`dbmr.schema.json`) remain cross-compatible so existing schemas can be consumed without modification.
3. **Future Extension**:
   - A separate crate (e.g. `dbmr-napi`) may be scoped in a post-1.0 milestone if there is dedicated demand for Node integration.

## Consequences
+ Prevents C-ABI / N-API build complexities from impacting the core Rust build pipeline and CI times.
+ Keeps dependency tree minimal and fully native to Rust.
+ Clear architectural boundary between the Node predecessor and the Rust successor.
