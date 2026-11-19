//! Facade schema loading: invalid schemas surface as 422; timestamp options reach the model.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use dbmr::adapters::SqliteAdapter;
use dbmr::core::types::{json_to_row, row_to_json};
use dbmr::{load_models_from_schema, Context, DatabaseAdapter};
use serde_json::json;
use std::sync::Arc;

async fn db() -> Arc<dyn DatabaseAdapter> {
    SqliteAdapter::connect("sqlite::memory:").await.unwrap()
}

#[tokio::test]
async fn invalid_schema_is_422_with_validation_detail() {
    let err = load_models_from_schema(&db().await, r#"{"adapter":"db2"}"#, None)
        .err()
        .expect("invalid schema must fail");
    assert_eq!(err.status_code(), 422);
    assert!(
        err.message()
            .starts_with("Schema validation failed: adapter: Invalid adapter \"db2\""),
        "{}",
        err.message()
    );
}

/// Insert `{"body","made","changed"}` through a schema-built model and return the stored row.
async fn insert_with(timestamps: &str) -> serde_json::Value {
    let db = db().await;
    db.query(
        "CREATE TABLE notes (id INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT, made TEXT, changed TEXT)",
        &[],
    )
    .await
    .unwrap();
    let schema = format!(
        r#"{{
      "adapter": "sqlite3", "framework": "axum",
      "tables": {{"notes": {{
        "columns": {{"id": "auto_increment", "body": "string", "made": "string", "changed": "string"}}
        {timestamps}
      }}}}
    }}"#
    );
    let models = load_models_from_schema(&db, &schema, None).unwrap();
    let rec = models["notes"]
        .insert(
            json_to_row(&json!({"body": "hi", "made": "x", "changed": "y"})),
            &Context::new(),
        )
        .await
        .unwrap()
        .expect("inserted record");
    row_to_json(&rec)
}

#[tokio::test]
async fn schema_timestamp_columns_are_server_managed() {
    // Plain columns accept client values...
    let plain = insert_with("").await;
    assert_eq!(
        (&plain["made"], &plain["changed"]),
        (&json!("x"), &json!("y"))
    );
    // ...but once declared as timestamps, client values are replaced by the model's UTC time.
    let managed =
        insert_with(r#", "timestamps": {"created_at": "made", "modified_at": "changed"}"#).await;
    assert_eq!(managed["body"], "hi");
    for col in ["made", "changed"] {
        let v = managed[col].as_str().unwrap_or_default();
        assert!(v.len() == 24 && v.ends_with('Z'), "{col}: {managed}");
    }
}

#[tokio::test]
async fn schema_hidden_version_and_relationships_reach_the_models() {
    let db = db().await;
    for ddl in [
        "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT, pw TEXT, version INTEGER NOT NULL DEFAULT 1)",
        "CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, user_id INTEGER, title TEXT)",
    ] {
        db.query(ddl, &[]).await.unwrap();
    }
    let schema = r#"{
      "adapter": "sqlite3", "framework": "axum",
      "tables": {
        "users": {"columns": {"id": "auto_increment", "email": "string", "pw": "string",
                  "version": "integer"}, "hidden": ["pw"], "version": "version"},
        "posts": {"columns": {"id": "auto_increment", "user_id": "integer", "title": "string"},
                  "parent": "users"}
      },
      "relationships": [{"parent": "users", "child": "posts", "foreignKey": "user_id"}]
    }"#;
    let models = load_models_from_schema(&db, schema, None).unwrap();
    let ctx = Context::new();
    let users = &models["users"];
    assert_eq!(users.version_column(), Some("version"));
    let u = users
        .insert(json_to_row(&json!({"email": "a@b.c", "pw": "h"})), &ctx)
        .await
        .unwrap()
        .unwrap();
    assert!(!u.contains_key("pw"), "hidden: {u:?}");
    assert_eq!(row_to_json(&u)["version"], 1);
    models["posts"]
        .insert(json_to_row(&json!({"user_id": 1, "title": "t"})), &ctx)
        .await
        .unwrap();
    let q = dbmr::Query::new().include(["posts"]);
    let page = users.list(q, &ctx).await.unwrap();
    assert_eq!(row_to_json(&page.data[0])["posts"][0]["title"], "t");
    let q = dbmr::Query::new().include(["users"]);
    let post = models["posts"].find_one(q, &ctx).await.unwrap().unwrap();
    assert_eq!(row_to_json(&post)["users"]["email"], "a@b.c");

    let bad = r#"{"adapter":"sqlite3","framework":"axum","tables":{"t":{"columns":{"a":"string"},
                 "hidden":["zz"],"version":"nope"}}}"#;
    let err = load_models_from_schema(&db, bad, None).err().unwrap();
    assert!(
        err.message().contains("hidden entry \"zz\""),
        "{}",
        err.message()
    );
    assert!(
        err.message().contains("version must name"),
        "{}",
        err.message()
    );
}
