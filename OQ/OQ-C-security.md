# OQ-C: Security

**Objective:**
- show that the findings of the 2026-11 code review (S1–S10) are closed;
- show that the BRD §14 / §32 security requirements hold;
- show that the supply chain is checked.

**Acceptance criteria:**
- every in-scope finding has a passing automated check (Pass) or a recorded live check;
- `cargo deny`, `cargo audit`, `gitleaks` and `scripts/ci_lints.py` are clean.

## Review findings

| ID | Finding (before fix) | Fix | Check | Status |
|---|---|---|---|---|
| S1 | db-manager put a URL table name into SQL with `format!` | Component removed from the library | n/a | **Out of scope** (ADR-0013); the CLI project inherits it |
| S2 | Identifier quoting did not escape `"`, `` ` `` or `]`, enabling SQL injection through table/column names | `quote_identifier` doubles the dialect quote and strips NUL | `compiler.rs::quoting_cannot_be_escaped`, `route_tests.rs::hostile_identifiers_cannot_inject` | **Pass** |
| S3 | Facade built schema models with `encryption: None`, so `encrypted` columns were stored as plaintext | `model_from_meta` takes the config; the builder fails without a key | `facade_tests.rs::schema_encrypted_column_requires_key`, `model_contract.rs::encrypted_columns_round_trip_and_are_ciphertext_at_rest` | **Pass** |
| S4 | The override saw only headers (`user.*` resolved to `""`); the pre-check and DELETE `/{id}` ignored it, allowing cross-tenant access. A later adversarial review found three more bypasses: `data`-wrapped filters dropped sibling keys, bulk `PUT` reached other tenants' rows, and item routes accepted bulk bodies | Override DSL replaced by a `Scope` set in app middleware and enforced by the model on every operation (ADR-0017); `data` filters AND sibling keys; update/upsert targets are checked against the scope; item routes refuse `data` arrays | `crates/dbmr-http/tests/scope_tests.rs` (7 tests); `route_edge_tests.rs::item_routes_refuse_bulk_bodies`; OQ-E E2 acceptance | **Pass** |
| S5 | MSSQL always called `trust_cert()` | Honour `TrustServerCertificate` (default off); `connect_host(.., trust_cert)` | `mssql_tests.rs::test_mssql_untrusted_cert_is_rejected` (live). The first live run failed: tiberius had no TLS feature, so connections were plaintext. Fixed by enabling `native-tls` | **Pass** (live) |
| S6 | Mongo accepted `$`-prefixed keys, enabling operator / `$where` injection | `field()` rejects `$`/empty/NUL names with 422; every comparison uses an explicit `$eq`… | `mongo_adapter.rs::tests::filter_is_operator_safe`; live `conformance.rs::mongodb` | **Pass** |
| S7 | MSSQL/Oracle upsert ORed composite keys, so UPDATE could hit unrelated rows | One AND group over all key columns; a partial key becomes a plain insert | `conformance.rs::{mssql,oracle}`: upsert of `(a,1)` leaves `(a,2)` unchanged (live) | **Pass** (live) |
| S8 | `IN` with a non-array value was dropped, widening UPDATE/DELETE filters | Scalar → one-element list; empty `IN` → `1=0`, empty `NOT IN` → `1=1` | `compiler.rs::where_semantics` | **Pass** |
| S9 | The mass-delete guard ran before `before_delete`, so a hook could empty the filter; it also returned a 500 | Guard runs after the hook and returns 400 | `model_contract.rs::mass_delete_guard_runs_after_before_delete_hook`, `soft_delete_hides_rows_and_mass_delete_is_guarded` | **Pass** |
| S10 | 5xx responses leaked raw DB errors | Scrubbed unless `NODE_ENV` ∈ {development, test, TEST} | `route_tests.rs::server_errors_are_scrubbed` | **Pass** |

## Additional controls

| Control | Check | Status |
|---|---|---|
| Client cannot set or clear the soft-delete flag (D19). It was not enforced before 2026-11-14 when the column was declared | Flag in model strip list; `model_contract.rs::soft_delete_flag_is_not_client_writable` | **Pass** |
| Request body bounded (16 MiB) | `BODY_LIMIT` in `router.rs` | Review only |
| Encryption tamper detection | `encryption.rs::test_tamper_detection` | **Pass** |
| Oracle commit errors surfaced (previously swallowed) | `oracle.rs` `query` | Review only |
| MSSQL transport encrypted | tiberius `native-tls` feature | **Pass** (live, S5 test) |
| Unsafe code | `#![deny(unsafe_code)]` workspace lint | **Pass** (clippy clean) |
| Panics in library code | `clippy::unwrap_used` denied outside tests | **Pass** (clippy clean) |
| SQL built with `format!` outside the adapter allowlist; secrets in logs | `scripts/ci_lints.py` | CI job `security` |
| Vulnerable / unlicensed / yanked dependencies | `cargo deny` (`deny.toml` fixed for cargo-deny ≥ 0.16), `cargo audit` | CI job `security`; **Not executed locally** |
| Committed secrets | `gitleaks` | CI job `security`; **Not executed locally** |

## Execution log

| Date | Commit | Environment | Result |
|---|---|---|---|
| 2026-11-14 | uncommitted | Win11, rustc 1.97.1 | S2, S3, S4, S6, S8, S9, S10 **Pass**; S5 and S7 review only; supply-chain jobs not run locally |
| 2026-11-15 | uncommitted | Win11, rustc 1.97.1, Docker Desktop 29.3.1 (images per `docker-compose.yml`) | S5, S7 **Pass** live; S6 **Pass** live. All S2–S10 Pass. Supply-chain jobs (deny, audit, gitleaks) not installed locally: CI only |
