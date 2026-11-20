# ADR-0017: Row-level scope, enforced by the model, replaces payload override rules

**Status:** Accepted (2026-11-19, user decision)
**Supersedes:** the override-rule part of BRD §20.6 and deviation D8. Amends ADR-0014.

## Context
The router took an override DSL, `create_model_router(model, json!({"tenant_id":
"user.tenant_id"}))`, that read values from `user.*`, `params.*`, `query.*`, `headers.*` or
`body.*` and forced them into filters and payloads. Tenancy was only one use of it, and it
was the only permission mechanism. An adversarial review found it bypassable:
- a `{"data": {...}, ...}` filter dropped sibling keys;
- bulk `PUT` could update or take over rows of another tenant (including through the upsert
  conflict key);
- item routes accepted bulk bodies that skipped item-level checks.

The checks lived in the HTTP layer, so jobs and other callers of the model had none. With
the CLI and generator gone (ADR-0013), the library needs a plain permission hook, not a DSL.

## Decision
1. The app's auth middleware inserts a `Scope(Map<column, value>)` request extension next to
   `CurrentUser`. The router copies both into `Context` (`ctx.scope`, `ctx.user`).
   `create_model_router(model)` takes no rules.
2. The **model** enforces the scope on every operation:
   - reads (`find`, `find_one`, `list`, `count`, `by_id`) AND it into every OR-group after
     `before_find`;
   - `remove` ANDs it in after the mass-delete guard, which still checks the caller's
     criteria only;
   - inserts get scalar scope values written in;
   - update and upsert targets (by PK, or by the conflict key for upsert) must be inside it,
     else 404;
   - `patch` keys on PK AND scope;
   - includes and `belongs_to` checks go through the related model, which applies the scope.
3. An array value means IN. An insert under a list scope must name one of the values, else
   422 `The <col> is outside the scope.`. An empty list matches nothing.
4. `ModelBuilder::require_scope(cols)` fails closed: a missing or `null` value is 401
   `Missing scope: <col>`. `Context::system()` skips it and has no scope, for jobs that must
   see every row. Jobs that work per tenant use `Context::new().with_scope(..)`.
5. Scope columns the model does not declare are ignored, so one scope fits related tables.
6. Nested routes add the path parameter with a `route_layer` middleware. Per-role route
   rules are ordinary middleware.
7. `POST /` with one object is a single create; item routes refuse `{"data":[...]}`.

## Consequences
+ One mechanism for tenancy, ownership, multi-tenant users and parent scoping, enforced in
  one place for HTTP and jobs alike.
+ Fewer concepts: no DSL, no path resolver (`dbmr_core::path` removed).
- Breaking change for every caller of `create_model_router`.
- The update/upsert target check reads then writes (not atomic). A concurrent write that
  moves a row out of the scope between the two statements is not covered. Scope columns
  should be immutable; a single `UPDATE ... WHERE pk AND scope` is the upgrade path.
- Without `require_scope`, a request with no `Scope` reaches every row. Models holding
  tenant data should set it.
