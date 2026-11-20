//! Integration test exercising the Blog Application domain model on dbmr.
//!
//! Validates:
//! - Authors, Posts, Comments, Likes, Shares models
//! - Relations: has_many & belongs_to
//! - Nested REST routes: /posts/{post_id}/comments
//! - Validation rules: email, minLength, maxLength, in:...
//! - Unique constraints: username, email, slug, [post_id, author_id]
//! - Safe delete: posts and comments
//! - Hooks: before_save slug generation and whitespace trim
//! - Eager loading: ?include=author,comments,likes,shares
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

use std::collections::HashMap;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Request};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::Router;
use dbmr::adapters::SqliteAdapter;
use dbmr::core::types::DbValue;
use dbmr::{create_model_router, DatabaseAdapter, Model, ModelHooks, Scope};
use serde_json::{json, Value};
use tower::ServiceExt;

const DDL: &[&str] = &[
    "CREATE TABLE authors (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        username TEXT NOT NULL UNIQUE,
        email TEXT NOT NULL UNIQUE,
        display_name TEXT NOT NULL,
        bio TEXT,
        is_active INTEGER NOT NULL DEFAULT 1,
        created_at TEXT NOT NULL DEFAULT (datetime('now'))
    )",
    "CREATE TABLE posts (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        author_id INTEGER NOT NULL,
        title TEXT NOT NULL,
        slug TEXT NOT NULL UNIQUE,
        content TEXT NOT NULL,
        published INTEGER NOT NULL DEFAULT 0,
        is_deleted INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        updated_at TEXT NOT NULL DEFAULT (datetime('now')),
        FOREIGN KEY(author_id) REFERENCES authors(id)
    )",
    "CREATE TABLE comments (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        post_id INTEGER NOT NULL,
        author_id INTEGER NOT NULL,
        content TEXT NOT NULL,
        is_deleted INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        FOREIGN KEY(post_id) REFERENCES posts(id),
        FOREIGN KEY(author_id) REFERENCES authors(id)
    )",
    "CREATE TABLE likes (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        post_id INTEGER NOT NULL,
        author_id INTEGER NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        CONSTRAINT uq_post_author UNIQUE (post_id, author_id),
        FOREIGN KEY(post_id) REFERENCES posts(id),
        FOREIGN KEY(author_id) REFERENCES authors(id)
    )",
    "CREATE TABLE shares (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        post_id INTEGER NOT NULL,
        author_id INTEGER NOT NULL,
        platform TEXT NOT NULL,
        created_at TEXT NOT NULL DEFAULT (datetime('now')),
        FOREIGN KEY(post_id) REFERENCES posts(id),
        FOREIGN KEY(author_id) REFERENCES authors(id)
    )",
];

async fn call(
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
        .unwrap();

    let res = app.clone().oneshot(req).await.unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let json_val = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    };
    (status, headers, json_val)
}

async fn post_comments_scope(
    Path(p): Path<HashMap<String, String>>,
    mut req: Request,
    next: Next,
) -> Response {
    let mut scope = req.extensions().get::<Scope>().cloned().unwrap_or_default();
    scope.0.insert("post_id".into(), json!(p["post_id"]));
    req.extensions_mut().insert(scope);
    next.run(req).await
}

#[tokio::test]
async fn test_blog_app_full_workflow() -> dbmr::Result<()> {
    let db: Arc<dyn DatabaseAdapter> = SqliteAdapter::connect("sqlite::memory:").await?;
    for sql in DDL {
        db.query(sql, &[]).await?;
    }

    let authors = Model::builder(db.clone(), "authors")
        .column("id", "integer")
        .column("username", "required|string|minLength:3|maxLength:30")
        .column("email", "required|email")
        .column("display_name", "required|string|maxLength:100")
        .column("bio", "string|nullable")
        .column("is_active", "boolean")
        .created_at("created_at")
        .unique(["username"])
        .unique(["email"])
        .search_columns(["username", "display_name", "bio"])
        .build()?;

    let posts = Model::builder(db.clone(), "posts")
        .column("id", "integer")
        .column("author_id", "required|integer")
        .column("title", "required|string|maxLength:120")
        .column("slug", "required|string|maxLength:150")
        .column("content", "required|string")
        .column("published", "boolean")
        .safe_delete("is_deleted")
        .created_at("created_at")
        .modified_at("updated_at")
        .unique(["slug"])
        .search_columns(["title", "content"])
        .hooks(ModelHooks::new().before_save(|mut row, _ctx| async move {
            if let Some(DbValue::Text(t)) = row.get("title") {
                let trimmed = t.trim().to_string();
                row.insert("title".into(), trimmed.clone().into());
                let needs_slug = match row.get("slug") {
                    None | Some(DbValue::Null) => true,
                    Some(DbValue::Text(s)) => s.trim().is_empty(),
                    _ => false,
                };
                if needs_slug {
                    let slug = trimmed.to_lowercase().replace(' ', "-");
                    row.insert("slug".into(), slug.into());
                }
            }
            Ok(row)
        }))
        .build()?;

    let comments = Model::builder(db.clone(), "comments")
        .column("id", "integer")
        .column("post_id", "required|integer")
        .column("author_id", "required|integer")
        .column("content", "required|string|maxLength:1000")
        .safe_delete("is_deleted")
        .created_at("created_at")
        .search_columns(["content"])
        .build()?;

    let likes = Model::builder(db.clone(), "likes")
        .column("id", "integer")
        .column("post_id", "required|integer")
        .column("author_id", "required|integer")
        .created_at("created_at")
        .unique(["post_id", "author_id"])
        .build()?;

    let shares = Model::builder(db, "shares")
        .column("id", "integer")
        .column("post_id", "required|integer")
        .column("author_id", "required|integer")
        .column(
            "platform",
            "required|string|in:twitter,linkedin,reddit,facebook,direct",
        )
        .created_at("created_at")
        .build()?;

    let authors = authors.has_many("posts", &posts, "author_id");
    let posts = posts
        .belongs_to("author", &authors, "author_id")
        .has_many("comments", &comments, "post_id")
        .has_many("likes", &likes, "post_id")
        .has_many("shares", &shares, "post_id");

    let comments =
        comments
            .belongs_to("post", &posts, "post_id")
            .belongs_to("author", &authors, "author_id");

    let likes =
        likes
            .belongs_to("post", &posts, "post_id")
            .belongs_to("author", &authors, "author_id");

    let shares =
        shares
            .belongs_to("post", &posts, "post_id")
            .belongs_to("author", &authors, "author_id");

    let app = Router::new()
        .nest("/authors", create_model_router(authors))
        .nest("/posts", create_model_router(posts))
        .nest("/comments", create_model_router(comments.clone()))
        .nest("/likes", create_model_router(likes))
        .nest("/shares", create_model_router(shares))
        .nest(
            "/posts/{post_id}/comments",
            create_model_router(comments).route_layer(middleware::from_fn(post_comments_scope)),
        );

    // 1. Create Authors
    let (s, _, alice) = call(
        &app,
        "POST",
        "/authors",
        Some(json!({
            "username": "alice",
            "email": "alice@blog.io",
            "display_name": "Alice K."
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(alice["id"], 1);

    let (s, _, _) = call(
        &app,
        "POST",
        "/authors",
        Some(json!({
            "username": "bob",
            "email": "bob@blog.io",
            "display_name": "Bob B."
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // 2. Validate author rules
    let (s, _, _) = call(
        &app,
        "POST",
        "/authors",
        Some(json!({
            "username": "al",
            "email": "bademail",
            "display_name": "Al"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // 3. Create Posts
    let (s, _, post) = call(
        &app,
        "POST",
        "/posts",
        Some(json!({
            "author_id": 1,
            "title": "  Hello World with dbmr  ",
            "content": "Building databases without friction.",
            "published": true
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(post["title"], "Hello World with dbmr");
    assert_eq!(post["slug"], "hello-world-with-dbmr");

    // 4. Nested Comments
    let (s, _, c1) = call(
        &app,
        "POST",
        "/posts/1/comments",
        Some(json!({
            "author_id": 2,
            "content": "First comment on post 1!"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(c1["post_id"], 1);

    let (s, _, c_list) = call(&app, "GET", "/posts/1/comments", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(c_list["count"], 1);

    // 5. Likes and unique constraint
    let (s, _, _) = call(
        &app,
        "POST",
        "/likes",
        Some(json!({ "post_id": 1, "author_id": 2 })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // Duplicate like -> 409
    let (s, _, _) = call(
        &app,
        "POST",
        "/likes",
        Some(json!({ "post_id": 1, "author_id": 2 })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // 6. Shares
    let (s, _, _) = call(
        &app,
        "POST",
        "/shares",
        Some(json!({ "post_id": 1, "author_id": 2, "platform": "twitter" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    let (s, _, _) = call(
        &app,
        "POST",
        "/shares",
        Some(json!({ "post_id": 1, "author_id": 2, "platform": "invalid_platform" })),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // 7. Includes (?include=author,comments,likes,shares)
    let (s, _, p_inc) = call(
        &app,
        "GET",
        "/posts/1?include=author,comments,likes,shares",
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(p_inc["author"]["username"], "alice");
    assert_eq!(p_inc["comments"].as_array().unwrap().len(), 1);
    assert_eq!(p_inc["likes"].as_array().unwrap().len(), 1);
    assert_eq!(p_inc["shares"].as_array().unwrap().len(), 1);

    // 8. Soft Delete
    let (s, _, _) = call(&app, "DELETE", "/posts/1", None).await;
    assert_eq!(s, StatusCode::OK);

    let (s, _, _) = call(&app, "GET", "/posts/1", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    Ok(())
}
