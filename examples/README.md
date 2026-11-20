# Examples

All `dbmr` examples live in [`crates/dbmr/examples/`](../crates/dbmr/examples/):

- [**Blog Application Modular Project** (`crates/dbmr/examples/blog_app/`)](../crates/dbmr/examples/blog_app/):
  Structured like a real-world backend project with:
  - `index.rs`: Application initialization, database connection, and server lifecycle.
  - `migration.rs`: Automatic migration runner that executes all SQL migrations before the app starts.
  - `migrations/`: Folder holding all sequential database migrations (`0001` through `0005`).
  - `model.rs`: Authors, Posts, Comments, Likes, Shares models with validations, hooks, scopes, and relations.
  - `router.rs`: REST routing with `create_model_router`, nested routes (`/posts/:id/comments`), and OpenAPI 3.1.
  - `test.rs`: Full integration test suite.
  - `main.rs`: CLI entrypoint (`--serve` or automated test verification).

- [**Quickstart** (`crates/dbmr/examples/quickstart.rs`)](../crates/dbmr/examples/quickstart.rs):
  Minimal starter template.

- [**Full Feature Tour** (`crates/dbmr/examples/tour.rs`)](../crates/dbmr/examples/tour.rs):
  Exhaustive tour of all features, filters, validations, encryption, and multi-tenant scopes.
