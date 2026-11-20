# ADR-0008: Global `db` / `init()` singleton backed by OnceLock

**Status:** Accepted (2026-11-08, user decision)
**Referenced by:** BRD §6.1/§35.8, task P0-08/P1-03/P3-25

## Context
The Node.js library relied on a global `init(adapter, config)` call that configured a mutable global `db` singleton. In Rust, multi-threaded concurrent web servers require lock-free reads and deterministic thread-safe access. While explicit dependency injection (`dbmr::model(db, ...)`) is the cleanest architectural pattern, parity and ease-of-use benefit from an application-level database handle.

## Decision
1. **Explicit Context is Primary**:
   All core model and transaction factories take `Arc<dyn DatabaseAdapter>` directly:
   ```rust
   pub fn model(db: Arc<dyn DatabaseAdapter>, table: &str, ...) -> ModelInstance;
   ```
2. **Global Singleton backed by `OnceLock`**:
   The facade crate provides `dbmr::init(adapter, config)` and `dbmr::global_db()`:
   - Stored in a `std::sync::OnceLock<Arc<dyn DatabaseAdapter>>`.
   - Thread-safe, atomic, and lock-free on the read hot path (no `Mutex` contention).
   - Calling `dbmr::init(...)` when already initialized returns an explicit `AppError::Internal("Database already initialized")`.
   - `model_from_global(table, ...)` resolves `db` from the global `OnceLock`.
3. **Testing Isolation**:
   Under the `testing` cargo feature, `arc-swap::ArcSwapOption<dyn DatabaseAdapter>` is used to allow isolated resets between test cases without affecting production release builds.

## Consequences
+ Delivers full ergonomic parity with Node `init()` / `db` workflows for simple web services.
+ Guarantees zero lock overhead on queries in production builds.
+ Preserves pure dependency injection for advanced multi-tenant and multi-database architectures.
