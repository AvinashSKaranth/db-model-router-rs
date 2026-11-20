# ADR-0006: JSON, decimal, and date fidelity contract

**Status:** Accepted (2026-11-08, user decision)
**Referenced by:** BRD §7.1/§15/§31/§35.6/§37, task P0-06/P3-02/P3-04

## Context
Node.js JavaScript runtimes suffer from IEEE-754 precision loss for integers greater than $2^{53} - 1$ (9,007,199,254,740,991) and floating-point rounding errors on decimal currency values. The upstream Node library attempted regex string pre-wrapping passes to protect 64-bit integers. In Rust, we require lossless serialization and exact database type fidelity across SQL and NoSQL engines.

## Decision
1. **Decimal & Numeric Support**:
   - Integrate `rust_decimal::Decimal` as a first-class citizen in internal row values:
     ```rust
     pub enum DbValue {
         Null,
         Bool(bool),
         Int(i128),
         Float(f64),
         Decimal(rust_decimal::Decimal),
         Text(String),
         Bytes(Vec<u8>),
         Json(serde_json::Value),
     }
     ```
   - SQL `DECIMAL`/`NUMERIC` driver columns decode directly into `DbValue::Decimal`.
   - String representations of numbers in decimal columns parse into `rust_decimal::Decimal` losslessly.
2. **Integer Fidelity**:
   - `DbValue::Int(i128)` handles up to signed 128-bit integers without precision loss.
   - Values exceeding `i128` are decoded as `DbValue::Text`.
3. **JSON Serialization**:
   - Enable `serde_json`'s `arbitrary_precision` feature across all workspace crates.
   - Numbers in JSON payloads retain their exact source lexeme; no regex pre-pass or string mangling.
4. **Dates & Timestamps**:
   - Date and datetime values are normalized to UTC ISO 8601 strings (`chrono::DateTime<Utc>`) on wire and JSON boundaries.
   - Database timestamp types map losslessly with millisecond/microsecond precision where supported.

## Consequences
+ Guarantees exact precision for financial, cryptographic, and high-precision IDs (e.g. Snowflake IDs, 64-bit auto-increments).
+ Eliminates brittle regex string transforms on JSON payloads.
+ Driver decoding converts directly into `DbValue::Decimal` or `DbValue::Int` without intermediate double-heap allocations.
