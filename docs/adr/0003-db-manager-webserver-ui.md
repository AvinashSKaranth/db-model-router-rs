# ADR-0003: db-manager ships a webserver with embedded UI

**Status:** Accepted (2026-11-08, user decision)
**Referenced by:** BRD §10/§32/§35.3, task P0-03/P8-01..P8-06

## Context
The Node version of `db-model-router` provided an Express-based dashboard with server-side rendered templates (`db-manager/`) for inspecting tables, browsing/editing rows, running SQL, and exporting CSV. For the Rust rewrite, we needed to decide whether to provide an API-only service, defer UI entirely, or ship a self-contained Rust-native service with a bundled web UI.

## Decision
1. `db-manager` will ship as an optional crate/feature (`dbmr-dbmanager` / feature `db-manager-web`) that serves both REST APIs and a compiled, self-contained web UI.
2. Web UI assets (HTML/CSS/JS) are embedded into the binary using compile-time embedding (`rust-embed` or `include_str!`), ensuring zero external runtime asset dependencies.
3. **Security Defaults**:
   - Default bind address is strictly `127.0.0.1` (loopback only). Binding to `0.0.0.0` or external addresses requires an explicit `--host` argument and emits a prominent startup warning.
   - Raw SQL execution is disabled by default and requires the `--enable-raw-sql` flag.
   - Query history is stored locally in an embedded SQLite file (`.dbmanager.sqlite`) with query redaction for sensitive patterns (passwords, tokens).
   - Default port is `4000` (configurable via `--port`).

## Consequences
+ Delivers full operational parity with the Node `db-manager` developer experience without needing Node/npm.
+ Single-binary distribution with zero external static asset file management.
+ Eliminates the security hazards of the upstream implementation (binding all interfaces with no auth and unprotected raw SQL).
− Slightly larger binary footprint for the CLI when `db-manager-web` feature is enabled.
