#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use dbmr::adapters::SqliteAdapter;
use dbmr::{create_model_router, global_db, init_global, load_models_from_schema, DatabaseAdapter};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;

const SCHEMA: &str = r#"{
  "adapter": "sqlite3",
  "framework": "axum",
  "tables": {
    "products": {
      "columns": { "id": "auto_increment", "title": "required|string", "price": "required|numeric" },
      "pk": "id",
      "unique": [],
      "search_columns": ["title"]
    }
  }
}"#;

#[tokio::test]
async fn schema_to_router_end_to_end() {
    let adapter = SqliteAdapter::connect("sqlite::memory:").await.unwrap();
    adapter
        .query(
            "CREATE TABLE products (id INTEGER PRIMARY KEY AUTOINCREMENT, title TEXT NOT NULL, price REAL NOT NULL)",
            &[],
        )
        .await
        .unwrap();
    let db: Arc<dyn DatabaseAdapter> = adapter;

    // OnceLock global (ADR-0008): second init fails.
    let _ = init_global(db.clone());
    assert!(global_db().is_some());
    assert!(init_global(db.clone()).is_err());

    let mut models = load_models_from_schema(&db, SCHEMA, None).unwrap();
    let app = create_model_router(models.shift_remove("products").unwrap());

    let res = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/0")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"title": "Rust Book", "price": 39.99}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let created: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(created["title"], "Rust Book");

    let res = app
        .oneshot(
            Request::builder()
                .uri("/?search=rust")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let list: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(list["count"], 1);
}

#[tokio::test]
async fn schema_encrypted_column_requires_key() {
    // S3: the facade must never build a model that would store `encrypted` columns as plaintext.
    let schema = SCHEMA.replace(
        r#""price": "required|numeric""#,
        r#""price": "required|numeric", "secret": "encrypted|string""#,
    );
    assert_ne!(schema, SCHEMA);
    let adapter = SqliteAdapter::connect("sqlite::memory:").await.unwrap();
    let db: Arc<dyn DatabaseAdapter> = adapter;
    let err = load_models_from_schema(&db, &schema, None)
        .err()
        .expect("must fail without key");
    assert_eq!(err.status_code(), 422, "{}", err.message());
    assert!(
        !err.message().contains("Schema validation"),
        "{}",
        err.message()
    );
    let key = Arc::new(
        dbmr::EncryptionConfig::new("raw:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", 1, None)
            .unwrap(),
    );
    let ok = load_models_from_schema(&db, &schema, Some(&key));
    assert!(
        ok.is_ok(),
        "{:?}",
        ok.err().map(|e| e.message().to_string())
    );
}
