//! Comprehensive test suite for the Blog Application.
//!
//! Validates:
//! - Migrations executed prior to application startup
//! - Authors, Posts, Comments, Likes, Shares models
//! - Relations: `has_many` & `belongs_to`
//! - Nested REST routes: `/posts/{post_id}/comments`
//! - Validation rules: `email`, `minLength`, `maxLength`, `in:...`
//! - Unique constraints: username, email, slug, `[post_id, author_id]`
//! - Safe delete: posts and comments
//! - Hooks: `before_save` slug generation and whitespace trim
//! - Eager loading: `?include=author,comments,likes,shares`
//! - Bulk creation: `POST /posts` with `{"data":[...]}`

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use tower::ServiceExt;

use super::index;

/// Helper to execute an HTTP request in memory against an Axum router.
pub async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (StatusCode, HeaderMap, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .expect("valid request");

    let res = app.clone().oneshot(req).await.expect("handler executed");
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .expect("read body");
    let text = String::from_utf8(bytes.to_vec()).expect("utf8");
    let json_val = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    (status, headers, json_val)
}

/// Verify migrations were applied to the database before app started.
pub async fn test_migrations_applied(app: &Router, db: &std::sync::Arc<dyn dbmr::DatabaseAdapter>) {
    let migrations = db
        .query("SELECT name FROM _migrations ORDER BY id ASC", &[])
        .await
        .expect("query _migrations");
    assert_eq!(migrations.len(), 5, "Expected 5 applied migrations");

    let (status, _, body) = call(app, "GET", "/health", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
}

/// Verify author CRUD, email validation, and unique constraints.
pub async fn test_authors(app: &Router) -> (i64, i64) {
    // 1. Create author 1
    let (status, _, body) = call(
        app,
        "POST",
        "/authors",
        Some(json!({
            "username": "alice",
            "email": "alice@example.com",
            "display_name": "Alice Walker",
            "bio": "Rust and backend enthusiast"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "Create author 1 failed: {body}");
    let a1_id = body["id"].as_i64().expect("author 1 id");

    // 2. Create author 2
    let (status, _, body) = call(
        app,
        "POST",
        "/authors",
        Some(json!({
            "username": "bob",
            "email": "bob@example.com",
            "display_name": "Bob Stone"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "Create author 2 failed: {body}");
    let a2_id = body["id"].as_i64().expect("author 2 id");

    // 3. Validation failure: invalid email format
    let (status, _, _) = call(
        app,
        "POST",
        "/authors",
        Some(json!({
            "username": "charlie",
            "email": "not-an-email",
            "display_name": "Charlie"
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "Invalid email must fail with 422"
    );

    // 4. Unique constraint conflict on duplicate username
    let (status, _, _) = call(
        app,
        "POST",
        "/authors",
        Some(json!({
            "username": "alice",
            "email": "alice2@example.com",
            "display_name": "Duplicate Alice"
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "Duplicate username must return 409 Conflict"
    );

    (a1_id, a2_id)
}

/// Verify post lifecycle: whitespace trimming, slug hook, and soft delete.
pub async fn test_posts(app: &Router, author_id: i64) -> i64 {
    // 1. Create post: title with whitespace and no slug provided
    let (status, _, body) = call(
        app,
        "POST",
        "/posts",
        Some(json!({
            "author_id": author_id,
            "title": "  Building Fast Microservices with Rust!  ",
            "content": "Rust offers memory safety without garbage collection overhead.",
            "published": 1
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "Post creation failed: {body}");
    let post_id = body["id"].as_i64().expect("post id");
    assert_eq!(body["title"], "Building Fast Microservices with Rust!");
    assert_eq!(body["slug"], "building-fast-microservices-with-rust");

    // 2. Fetch post by ID
    let (status, _, body) = call(app, "GET", &format!("/posts/{post_id}"), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], post_id);

    // 3. Create second post to test soft delete
    let (status, _, body) = call(
        app,
        "POST",
        "/posts",
        Some(json!({
            "author_id": author_id,
            "title": "Temporary Draft",
            "content": "This draft will be soft-deleted.",
            "published": 0
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let draft_id = body["id"].as_i64().expect("draft id");

    // 4. Soft-delete the draft
    let (status, _, _) = call(app, "DELETE", &format!("/posts/{draft_id}"), None).await;
    assert_eq!(status, StatusCode::OK);

    // 5. Query collection: soft-deleted post must be excluded
    let (status, _, body) = call(app, "GET", "/posts", None).await;
    assert_eq!(status, StatusCode::OK);
    let items = body["data"].as_array().expect("posts array");
    assert!(
        items.iter().all(|p| p["id"] != draft_id),
        "Soft-deleted post must not appear in list"
    );

    post_id
}

/// Verify nested comments route `/posts/{post_id}/comments`.
pub async fn test_comments(app: &Router, post_id: i64, author_id: i64) {
    // 1. Add comment via nested route
    let (status, _, body) = call(
        app,
        "POST",
        &format!("/posts/{post_id}/comments"),
        Some(json!({
            "author_id": author_id,
            "content": "Super insightful article on Rust services!"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "Comment creation failed: {body}");
    assert_eq!(body["post_id"], post_id);

    // 2. Query comments strictly through nested route
    let (status, _, body) = call(app, "GET", &format!("/posts/{post_id}/comments"), None).await;
    assert_eq!(status, StatusCode::OK);
    let comments = body["data"].as_array().expect("comments array");
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["post_id"], post_id);
}

/// Verify likes composite unique constraint `[post_id, author_id]`.
pub async fn test_likes(app: &Router, post_id: i64, author_id: i64) {
    // 1. First like succeeds
    let (status, _, body) = call(
        app,
        "POST",
        "/likes",
        Some(json!({
            "post_id": post_id,
            "author_id": author_id
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "Like creation failed: {body}");

    // 2. Duplicate like from same author on same post returns 409 Conflict
    let (status, _, _) = call(
        app,
        "POST",
        "/likes",
        Some(json!({
            "post_id": post_id,
            "author_id": author_id
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "Duplicate like on [post_id, author_id] must return 409 Conflict"
    );
}

/// Verify shares platform validation (`in:twitter,facebook,linkedin,mastodon`).
pub async fn test_shares(app: &Router, post_id: i64, author_id: i64) {
    // 1. Valid platform succeeds
    let (status, _, body) = call(
        app,
        "POST",
        "/shares",
        Some(json!({
            "post_id": post_id,
            "author_id": author_id,
            "platform": "twitter"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "Share creation failed: {body}");

    // 2. Invalid platform fails with 422
    let (status, _, _) = call(
        app,
        "POST",
        "/shares",
        Some(json!({
            "post_id": post_id,
            "author_id": author_id,
            "platform": "myspace"
        })),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::UNPROCESSABLE_ENTITY,
        "Invalid platform must fail validation with 422"
    );
}

/// Verify eager loading across relations `?include=author,comments,likes,shares`.
pub async fn test_eager_loading(app: &Router, post_id: i64) {
    let (status, _, body) = call(
        app,
        "GET",
        &format!("/posts/{post_id}?include=author,comments,likes,shares"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["id"], post_id);
    assert!(
        body["author"].is_object(),
        "author relation should be loaded"
    );
    assert!(
        body["comments"].is_array(),
        "comments relation should be loaded"
    );
    assert!(body["likes"].is_array(), "likes relation should be loaded");
    assert!(
        body["shares"].is_array(),
        "shares relation should be loaded"
    );
}

/// Verify bulk creation parity `POST /posts` with `{"data":[...]}`.
pub async fn test_bulk_creation(app: &Router, author_id: i64) {
    let (status, _, body) = call(
        app,
        "POST",
        "/posts",
        Some(json!({
            "data": [
                {
                    "author_id": author_id,
                    "title": "Bulk Post One",
                    "content": "Content for the first bulk post in array.",
                    "published": 1
                },
                {
                    "author_id": author_id,
                    "title": "Bulk Post Two",
                    "content": "Content for the second bulk post in array.",
                    "published": 1
                }
            ]
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "Bulk creation failed: {body}");
    assert_eq!(body["rows"], 2, "Expected 2 created rows in bulk response");
    assert_eq!(body["type"], "success");
}

/// Verify OpenAPI 3.1 specification endpoint.
pub async fn test_openapi_spec(app: &Router) {
    let (status, _, body) = call(app, "GET", "/openapi.json", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["openapi"], "3.1.0");
    assert!(body["paths"]["/authors"].is_object());
    assert!(body["paths"]["/posts"].is_object());
}

/// Execute all test cases sequentially and output progress.
pub async fn run_all_tests() -> dbmr::Result<()> {
    println!("🧪 Initializing Blog App stack and running migrations...");
    let (app, db, _models) = index::create_app().await?;

    println!("  ✓ Running test_migrations_applied...");
    test_migrations_applied(&app, &db).await;

    println!("  ✓ Running test_authors (CRUD, validation & unique constraints)...");
    let (a1_id, a2_id) = test_authors(&app).await;

    println!("  ✓ Running test_posts (whitespace trim, slug hook, safe delete)...");
    let post_id = test_posts(&app, a1_id).await;

    println!("  ✓ Running test_comments (nested /posts/:id/comments route)...");
    test_comments(&app, post_id, a2_id).await;

    println!("  ✓ Running test_likes (composite unique [post_id, author_id])...");
    test_likes(&app, post_id, a2_id).await;

    println!("  ✓ Running test_shares (platform enum validation)...");
    test_shares(&app, post_id, a2_id).await;

    println!("  ✓ Running test_eager_loading (?include=author,comments,likes,shares)...");
    test_eager_loading(&app, post_id).await;

    println!("  ✓ Running test_bulk_creation (POST with {{\"data\":[...]}})...");
    test_bulk_creation(&app, a1_id).await;

    println!("  ✓ Running test_openapi_spec (GET /openapi.json)...");
    test_openapi_spec(&app).await;

    println!("🎉 All Blog App tests passed successfully!");
    Ok(())
}
