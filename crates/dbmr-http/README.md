# dbmr-http

Custom Axum REST router generator for `db-model-router` (`dbmr`).

Features:
- Generates standard REST endpoints for any `Model`:
  - `GET /` (list with pagination, filters, sorting, relations)
  - `GET /:id` (item retrieval)
  - `POST /` (single or bulk create)
  - `PUT /:id` and `PATCH /:id` (full update / partial update)
  - `DELETE /:id` (item delete / soft delete)
- Row-level security scope enforcement via middleware
- ETag and `If-Match` optimistic locking support
- OpenAPI 3.0 document generation
- Health check endpoints (`health_router`)

Part of the [db-model-router](https://crates.io/crates/db-model-router) workspace.
