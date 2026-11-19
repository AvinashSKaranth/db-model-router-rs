# ADR-0011: Differential benchmark contenders and release budgets

**Status:** Accepted (2026-11-08, user decision)
**Referenced by:** BRD §35.11/§38, task P0-11/P10-01..P10-10

## Context
A primary goal of the Rust rewrite is providing significant performance and memory improvements over the Node.js implementation while remaining competitive with canonical Rust ORMs. To verify this objectively, we need pinned contenders, standardized scenarios, and strict acceptance budgets.

## Decision
1. **Pinned Contenders**:
   - `dbmr` (PostgreSQL adapter, validation enabled).
   - **Diesel 2.x** (`postgres` feature, `r2d2` pool): Canonical Rust compile-time ORM.
   - **SeaORM** (latest stable, async): Canonical dynamic async Rust ORM.
   - **SQLx** (raw query harness): SQL-level async baseline.
   - **tokio-postgres** (`deadpool-postgres`): Low-level protocol ceiling.
   - **Node `db-model-router`**: Historical baseline.
2. **Standard Scenarios (15 Scenarios §38.4)**:
   - `insert_single`, `bulk_100`, `bulk_1000`, `bulk_10000`, `read_pk`, `find_filter`, `find_sort_limit`, `list_page`, `update_pk`, `bulk_update_100`, `upsert`, `delete_pk`, `tx_multi`, `tx_rollback`, `jsonb_roundtrip`.
3. **Acceptance Budgets (§38.6)**:
   - Within **10% throughput of Diesel** on single-row CRUD (`insert_single`, `read_pk`, `update_pk`, `delete_pk`, `jsonb_roundtrip`).
   - Within **10% throughput of SeaORM** on bulk and transaction scenarios.
   - $\ge$ **85% of SQLx raw** throughput on single-row CRUD.
   - $\ge$ **2× throughput** (or $\le$ 50% median latency) compared to the Node reference.
   - Any scenario with $>$ **20% regression** between releases fails the release gate.

## Consequences
+ Prevents performance degradation over time with automated regression gates.
+ Establishes credibility by benchmarking directly against standard Rust ecosystem libraries.
+ Benchmarking crate (`crates/dbmr-bench`) runs in nightly CI with HTML/JSON report artifacts.
