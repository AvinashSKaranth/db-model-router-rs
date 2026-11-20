# ADR-0015: Pure-Rust drivers: `rskafka` for Kafka, `oracle-rs` for Oracle

**Status:** Accepted (2026-11-14)
**Deviation from:** BRD §11 dependency table (`rdkafka`, Oracle via OCI)

## Context
`rdkafka` needs librdkafka (C, cmake) and the Oracle OCI driver needs Oracle Instant
Client at build and run time. Both break `cargo build` on a clean machine and in
minimal containers.

## Decision
Use `rskafka` (pure Rust) for the Kafka event sink and `oracle-rs` (pure Rust, thin
protocol) for the Oracle adapter.

## Consequences
+ `cargo build --all-features` works with no system libraries.
- `rskafka` has no transactional producer and fewer tuning knobs than librdkafka. dbmr
  buffers events until DB commit itself (BRD §26), so exactly-once Kafka transactions are
  not required.
- `oracle-rs` is less mature than OCI. The Oracle adapter is **unverified** until the
  live suite (`crates/dbmr-adapters/tests/oracle_tests.rs`) passes against
  `gvenzl/oracle-free` (see `OQ/OQ-B-adapter-conformance.md`).
