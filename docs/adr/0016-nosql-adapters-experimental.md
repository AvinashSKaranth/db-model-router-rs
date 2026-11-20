# ADR-0016: Redis and DynamoDB adapters are experimental; transactions are SQLite/PostgreSQL only

**Status:** Accepted (2026-11-14)
**Amends:** ADR-0010 (live-database CI job is non-blocking until it is green once)
**Note (2026-11-19):** transactions now work on every SQL adapter (SQLite,
PostgreSQL/CockroachDB, MySQL/MariaDB, MSSQL, Oracle). Only MongoDB, Redis and DynamoDB
return `Active database adapter does not support transactions.` Decision 3 is superseded.

## Context
A code review found:
- **DynamoDB**: requests are unsigned (no SigV4), so only DynamoDB Local works. Every
  read is a full `Scan` filtered in memory. `Scan` pagination is now followed.
- **Redis**: every read is `KEYS table:*` plus `HGETALL` per key, filtered in memory.
- **Transactions**: `TransactionAdapter` is implemented for SQLite and PostgreSQL.
  Other adapters return an error from `begin_tx`.

## Decision
1. Keep DynamoDB and Redis behind their feature flags, documented as **experimental**:
   suitable for dev, tests and small tables, and not production-grade.
2. DynamoDB stays restricted to DynamoDB Local until SigV4 signing (or the AWS SDK) is added.
3. `Transaction::begin` on any adapter other than SQLite or PostgreSQL returns an
   `AppError::Internal`. Callers must not assume atomicity there.
4. The CI live-adapter job runs with `continue-on-error: true` until it has been green
   once. After that, remove the flag.

## Consequences
+ Users get an honest capability matrix (README, `OQ/OQ-B-adapter-conformance.md`).
- MySQL, MariaDB, CockroachDB, MSSQL and Oracle transactions are follow-up work.
