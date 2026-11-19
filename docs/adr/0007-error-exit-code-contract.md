# ADR-0007: Typed AppError and CLI exit-code contract

**Status:** Accepted (2026-11-08, user decision)
**Referenced by:** BRD §20.3/§29.2/§35.7, task P0-07/P3-01/P5-15/P7-02

## Context
In the upstream Node library, errors were dynamically mapped using `err.cause.status` or `err.status`, leading to stringly-typed status propagation (Defect D2 in ADR-0009). CLI commands also had inconsistent exit codes and crashed when output contexts lacked logging helpers (Defect D12). We need a unified, compile-time typed error architecture and standard CLI exit codes.

## Decision
1. **Typed `AppError`**:
   - `AppError` is a strongly-typed enum (`thiserror`) carrying an associated `axum::http::StatusCode` and human-readable message:
     ```rust
     #[derive(Debug, thiserror::Error)]
     pub enum AppError {
         #[error("{0}")]
         BadRequest(String), // 400
         #[error("{0}")]
         NotFound(String),   // 404
         #[error("{0}")]
         UnprocessableEntity(String), // 422 (validators, filter errors)
         #[error("{0}")]
         Internal(String),   // 500
         #[error(transparent)]
         Adapter(#[from] AdapterError),
         #[error(transparent)]
         Schema(#[from] SchemaError),
     }
     ```
   - In `IntoResponse`:
     - Status 4xx: outputs `{ "type": "danger", "message": "<error message>" }`.
     - Status 404: outputs `{ "message": "Not Found", "type": "danger" }`.
     - Status 422: preserves space-separated validation error strings matching the upstream format.
     - Status 5xx: in `NODE_ENV` / `RUST_ENV` of `development` or `test`, emits actual error / database message; in production, scrubs message to `"Internal server error"`.
2. **CLI Exit Codes**:
   - **`0`**: Success.
   - **`1`**: Usage errors, invalid CLI arguments, schema validation errors, or doctor failures.
   - **`2`**: Internal execution failure, database connection error, or unexpected panic.
   - When `--json` is supplied, stdout outputs a single JSON response containing error details and exit code 1 or 2 is returned.

## Consequences
+ Guarantees compile-time exhaustiveness for HTTP error handling without string-based status inspection.
+ CLI callers (CI/CD scripts) have standard POSIX exit-code guarantees.
+ Preserves exact client response body format (`{ "type": "danger", "message": ... }`).
