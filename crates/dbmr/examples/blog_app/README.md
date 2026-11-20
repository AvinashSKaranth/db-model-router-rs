# Blog Application Example Backend

A complete, production-grade example backend structured as a realistic modular project built on `dbmr`.

## Project Structure

```
blog_app/
├── README.md               # Architecture documentation and quickstart
├── main.rs                 # CLI entrypoint (demo runner or HTTP server)
├── index.rs                # Application coordinator: DB connect, migrations, router assembly
├── migration.rs            # Migration runner executing SQL scripts before app starts
├── migrations/             # 1 folder holding all sequential SQL migrations
│   ├── 0001_create_authors.sql
│   ├── 0002_create_posts.sql
│   ├── 0003_create_comments.sql
│   ├── 0004_create_likes.sql
│   └── 0005_create_shares.sql
├── model.rs                # Domain models, validation rules, hooks, scopes, and relations
├── router.rs               # REST router generation, nested route mounting, OpenAPI spec
└── test.rs                 # Comprehensive end-to-end integration test suite
```

## How It Works

1. **Migrations Run Before App Starts**:
   - `migration.rs` reads all `.sql` files in `migrations/` in ordered sequence.
   - It maintains a `_migrations` table so migrations are only executed once.
   - Called by `index::create_app()` before any models or routers are initialized.

2. **Domain Models & Rules (`model.rs`)**:
   - **Authors**: `id`, `username`, `email`, `display_name`, `bio`, `is_active`, `created_at`. Unique indexes on `username` and `email`.
   - **Posts**: `id`, `author_id`, `title`, `slug`, `content`, `published`, `is_deleted`. Safe delete enabled. `SlugAndTrimHook` trims whitespace and derives URL-safe slugs.
   - **Comments**: `id`, `post_id`, `author_id`, `content`, `is_deleted`. Belongs to post and author. Safe delete enabled.
   - **Likes**: `id`, `post_id`, `author_id`. Composite unique constraint on `[post_id, author_id]` prevents duplicate likes.
   - **Shares**: `id`, `post_id`, `author_id`, `platform`. Validated against `in:twitter,facebook,linkedin,mastodon`.

3. **REST Routers & Scopes (`router.rs`)**:
   - Full CRUD endpoints generated with `create_model_router`.
   - Nested route `/posts/{post_id}/comments` isolates comments query to the post using `Scope` middleware.
   - Generates OpenAPI 3.1 specification at `GET /openapi.json`.

4. **Testing (`test.rs`)**:
   - Validates migrations, validations (422), unique constraints (409), soft deletes, nested routes, relations eager-loading, and bulk writes (`{"data":[...]}`).

## Running the Example

### Run automated verification
```bash
cargo run -p dbmr --example blog_app
```

### Serve live HTTP server
```bash
cargo run -p dbmr --example blog_app -- --serve
```
The server will listen on `http://127.0.0.1:3000` with interactive endpoints.
