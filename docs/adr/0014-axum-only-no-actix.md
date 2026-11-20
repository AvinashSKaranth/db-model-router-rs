# ADR-0014: Axum is the only HTTP integration; no Actix feature

**Status:** Accepted (2026-11-14)
**Deviation from:** BRD §11 "Optional: Actix-web behind feature flag", §17.3 Actix QUERY note

## Context
The BRD lists Actix-web as optional. Supporting two frameworks doubles the router, extractor
and test surface. No consumer asked for Actix.

## Decision
`dbmr-http` targets Axum 0.8 only. The router is a plain `axum::Router`, so it nests into
any Axum or Tower-compatible app. The framework-neutral logic (`format_csv`, `format_xml`,
`error_response`) is public, and permissions live in the model (`Context.scope`, ADR-0017), so an Actix adapter can be built
outside this repository.

## Consequences
+ One router implementation and one test suite (`crates/dbmr-http/tests/route_tests.rs`).
- Actix users must write a thin adapter. Revisit when there is a concrete request.
