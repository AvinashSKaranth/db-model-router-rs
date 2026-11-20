# ADR-0004: No `.js` migration execution — SQL + Rust-native only

**Status:** Accepted (2026-11-08, user decision)
**Referenced by:** BRD §30/§29/§13, task P0-04/P7-11/P7-15

## Context
Node migration runner uses dynamic `require()` on `.js` migrations
(arbitrary code execution by design) and expects JS↔SQL pairing via
`up(db)/down()` exports. A Rust CLI cannot execute JavaScript safely.

## Decision
1. Rust `migrate:*` supports **`.sql`** migrations only out of the box.
2. Node-era `.js` migration files are **detected and rejected** with a clear,
   actionable error ("Convert to `NNN_name.up.sql`/`.down.sql`; see
   `docs/migrations-js-to-sql.md`") — never evaluated, never shelled out to node.
3. Rust-native migrations are supported only through SQL `.sql` pairs
   (`up`/`down`); until the workspace pattern for compiled migrations lands
   (e.g. embedded `.rs` → `include_str!` at build time), no arbitrary Rust
   closures are run either.
4. `migrate:create <name>` emits `YYYYMMDDHHmmss_<sanitized>.up.sql` (and optionally
   `.down.sql` when `--with-down`), never `.js`.

## Consequences
+ Zero arbitrary-code execution; consistent with Rust security posture (§32/§36).
+ Predictable migration story for new projects; SQL-first like most Rust stacks.
− Existing Node migrations must be converted before adopting the Rust CLI;
  conversion requires the Node CLI one final time (documented step).
− Migration `.down.sql` is required for `migrate:down` (SQLite/PG/SQL path).
