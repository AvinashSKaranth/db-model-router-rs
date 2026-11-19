# ADR-0009: Upstream defects — fix these 8, preserve the rest

**Status:** Accepted (2026-11-08, user decision)
**Referenced by:** BRD §18/§32/§34/§35.9/§12.5, task P0-09

## Context
Node version contains documented defects (BRD §34, D1–D20). Rewriting in Rust
is the one moment to fix structural bugs without breaking runtime call sites we
cannot see. User approved the following fixes.

## Decision — FIX
| ID | Fix |
|---|---|
| D2 | Typed `AppError` carrying HTTP status; status is never read from a stringly `cause`; `err.status` set for backward compat but **never consulted**. |
| D3 | Model-layer missing record ⇒ `Option<Record>` uniformly (`find_one`, `by_id`, `patch`, single `insert`). HTTP `None` ⇒ `404 {"message":"Not Found","type":"danger"}`. No `false` sentinel anywhere. |
| D8 | `"null"`-string deletion footgun removed: override applies values verbatim; literal `"null"` is stored as the string `"null"`. Missing override path resolves to `""` (unchanged, documented). **Superseded by ADR-0017:** override rules are gone; a missing required scope is 401 `Missing scope: <col>`. |
| D10 | Explicit NULL semantics per adapter: `null` in a filter becomes `IS NULL` (SQL) / `{ field: null }` (Mongo) explicitly; never `= NULL`. Documented in §22.7 as corrected behavior. |
| D11 | Kafka events from nested SQLite savepoints are **not** emitted immediately; buffering happens at the outermost transaction scope only. |
| D15 | Model-layer mass-delete guard: empty filter on `remove` ⇒ error `Missing filter` (500 at HTTP layer) unless `allow_destructive = true` in model option (opt-in, logged). Route-layer empty-criteria 400 stays. |
| D17 | MSSQL default credentials insecure TLS (`encrypt:false`, `trustServerCertificate:true`) removed; adapter defaults to `encrypt:true`/`trustServerCertificate:false` and requires **explicit** opt-out, with a startup warning. |
| D19 | `safeDelete` column excluded from HTTP-write payloads (and from bulk `UPDATE` validators) — soft-delete flag is server-managed; client cannot set/unset it. |

## Decision — PRESERVE (do not "fix" silently)
| ID | Kept behavior |
|---|---|
| D1 | Single-insert route stays `POST /:id` (path id ignored). |
| D4 | Insert/update/upsert single returning `None` on re-fetch miss: returns `None` today; rewrite keeps `Option`, but adds a `RefetchFailed` **warning** log and an after-`record==None` metric hook instead of a panic (flagged as partial fix; full typed error deferred with ADR revisal if desired). |
| D5 | Bulk detection via own-`data` property kept; schema validator warns on columns named `data`. |
| D6 | CSV header-from-first-row; XML does not escape `'` — preserved, byte-parity. |
| D7 | `{ data: … }`-like bodies treated as rows per `sendFormatted` rules — preserved. |
| D9 | `dataToFilter` purification: `filter` merge effect kept (groups extended with object-conditions), **input no longer mutated** — small deviation recorded. |
| D12 | CLI single OutputContext (`log`/`result`/`flush`) — preserved (this is itself a fix of the Node crash; recorded as improvement). |
| D13 | Timestamp auto-management claims removed; DB-defaults contract documented. |
| D14 | `count` = total matches (not page length) — documented, kept. |
| D16 | `inspect` heuristics kept (report-only confidence output). |
| D18 | Timestamp-variant stripping lists kept; `timestamps.{x}: null` means "declared, name not overridden". |
| D20 | Route GET-by-id does **not** 422 on bad ids (find-based path) — kept; document. |

## Consequences
+ All eight approved fixes are **security-relevant or correctness** issues; fixing
  them strengthens the rewrite's value proposition.
+ Deviation inventory updated (`docs/deviations.md`).
− Node-exact error/body parity breaks in these eight places; migration guide
  lists them explicitly.
