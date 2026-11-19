# ADR-0001: Rust-native API, behavior parity (no drop-in proxy)

**Status:** Accepted (2026-11-08, user decision)
**Supersedes:** none · Referenced by: BRD §35.1, §6.1, task P0-01/P0-08

## Context
Responsibilities are Rust-heavy (ownership, typed errors, generics, dyn-compat).
A drop-in dynamic `init()+db` proxy inherits Node footguns: global lock contention,
falsy coercions, no compile-time guarantees.

## Decision
The public API is Rust-native (typed builders/contexts), with observable semantics
parity to the Node library (statuses, bodies, return-shape *semantics*, error messages,
hook ordering). No JS-style dynamic proxy in the default build.

- Missing-record semantics unify to `Option<Record>` at the model layer; the HTTP
  layer maps `None` to the Node-compatible `404 {"message":"Not Found","type":"danger"}`.
- Global `init()`/`db` shim is deferred to ADR-0008 (default: explicit context);
  not part of this ADR.

## Consequences
+ Compile-time safety, zero hidden global state, clean generics for transactions.
+ `ModelInstance` returns `Option<...>` uniformly (fixes D3 in API contract).
− Migration guide for Node users required (signature changes documented in
  README + `docs/deviations.md`).
− External TS/Node wrappers are out of scope for v1 (ADR-0012).
