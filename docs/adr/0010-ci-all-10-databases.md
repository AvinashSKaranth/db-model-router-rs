# ADR-0010: CI runs live service containers for all 10 adapters

**Status:** Accepted (2026-11-08, user decision)
**Referenced by:** BRD §12.1/§35.10, task P0-10/P1-08/P4-21

## Context
Options: (a) sqlite+postgres(+mysql) only, (b) sqlite+postgres, (c) all 10 as
services. User chose (c) maximum fidelity.

## Decision
GitHub Actions / compatible CI provisions service containers (or
`testcontainers` from tests) for **all 10 adapters**:

| Service | Image | Purpose |
|---|---|---|
| SQLite | — (file-in-tmpfs) | sync path, savepoints, WAL |
| PostgreSQL 16 | `postgres:16` | bool/int semantics, RETURNING, savepoints |
| CockroachDB | `cockroachdb/cockroach:latest-v24.1` (pin) | Postgres dialect diff |
| MySQL 8 | `mysql:8` | ON DUPLICATE KEY UPDATE, chunking |
| MariaDB 11 | `mariadb:11` | parity with MySQL path |
| MongoDB 7 | `mongo:7` | 24-hex→ObjectId, regex LIKE |
| MSSQL 2022 | `mcr.microsoft.com/mssql/server:2022-latest` | MERGE/OUTPUT, encrypt defaults |
| Oracle XE | `gvenzl/oracle-xe:21-slim` (pin) | MERGE/RETURNING, ORA codes |
| Redis 7 | `redis:7-alpine` | hash scans, O(N) filters |
| DynamoDB Local | `amazon/dynamodb-local:latest` (pin) | BatchWriteItem, contains filters |

Notes:

- Oracle XE image is large/slow to start; dedicated job lane (not blocking
  sqlite fast-test lane).
- CockroachDB may lag Postgres by one release line when a regression appears;
  treat failures as P1 with a 7-day fix window, then pin version.
- Redis and DynamoDB are functional tests (not perf gates); everything runs
  serially or in own job lanes to avoid port collisions.
- Test-case coverage must reach >95% across **all** service-backed tests
  without silently waiving adapter cases — if a service is unavailable, CI
  fails; it does not skip.

## Consequences
+ Highest behavioral confidence; regressions caught across dialects.
− CI bill, flaky windows (Oracle start times), maintenance of 10 images.
+ Adapter tests run in ~2 lanes: SQL fast-lane / heavy-lane (Oracle, MSSQL,
  Cockroach).
