//! Router edge paths: item routes refuse bulk bodies, filter errors, DB errors, 405 on
//! items, and formatter edge cases.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use dbmr::adapters::SqliteAdapter;
use dbmr::core::Model;
use dbmr::http::{create_model_router, format_csv, format_xml};
use serde_json::{json, Value};
use sqlx::sqlite::SqlitePoolOptions;
use std::sync::Arc;
use tower::ServiceExt;

async fn model(create_table: bool) -> Model {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    if create_table {
        sqlx::query(
            "CREATE TABLE users (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                age INTEGER,
                note TEXT
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
    }
    Model::builder(Arc::new(SqliteAdapter::new(pool)), "users")
        .column("id", "integer")
        .column("name", "required|string")
        .column("age", "integer")
        .column("note", "string")
        .build()
        .unwrap()
}

async fn app() -> Router {
    create_model_router(model(true).await)
}

async fn call_raw(app: &Router, method: &str, uri: &str, body: &str) -> (StatusCode, String) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let body = body.map(|b| b.to_string()).unwrap_or_default();
    let (status, text) = call_raw(app, method, uri, &body).await;
    let v = if text.is_empty() {
        Value::Null
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| panic!("not JSON: {text}"))
    };
    (status, v)
}

async fn seed(app: &Router) {
    let (s, _) = call(
        app,
        "POST",
        "/",
        Some(json!({"data": [{"name": "Ann", "age": 30}, {"name": "Bob", "age": 40}]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
}

const BAD_FILTER: &str = r#"{"filter": [[["age", "bogus", 1]]]}"#;

#[tokio::test]
async fn single_row_bulk_insert_summary_carries_id() {
    let app = app().await;
    let (s, v) = call(&app, "POST", "/", Some(json!({"data": [{"name": "Ann"}]}))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["rows"], 1);
    assert_eq!(v["type"], "success");
    assert_eq!(v["id"], 1);
}

#[tokio::test]
async fn item_routes_refuse_bulk_bodies() {
    // Bulk writes go to the collection route; an item route never writes a `data` array.
    let app = app().await;
    seed(&app).await;
    for m in ["POST", "PUT", "PATCH", "DELETE"] {
        let (s, v) = call(
            &app,
            m,
            "/1",
            Some(json!({"data": [{"id": 2, "name": "X"}]})),
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{m}: {v}");
        assert_eq!(
            v["message"],
            "Bulk writes ({\"data\": [...]}) use the collection route"
        );
    }
    assert_eq!(call(&app, "GET", "/2", None).await.1["name"], "Bob");
}

#[tokio::test]
async fn invalid_filter_is_422_on_list_and_item_query() {
    let app = app().await;
    seed(&app).await;
    for uri in ["/", "/1"] {
        let (s, text) = call_raw(&app, "QUERY", uri, BAD_FILTER).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{uri}: {text}");
        let v: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(v["type"], "danger");
        assert!(!v["message"].as_str().unwrap().is_empty());
    }
}

#[tokio::test]
async fn database_error_in_precheck_is_500() {
    // No table: the existence pre-check itself fails.
    let app = create_model_router(model(false).await);
    for m in ["PUT", "PATCH", "DELETE"] {
        let (s, v) = call(&app, m, "/1", Some(json!({"name": "x"}))).await;
        assert_eq!(s, StatusCode::INTERNAL_SERVER_ERROR, "{m}");
        assert_eq!(v["type"], "danger");
    }
}

#[tokio::test]
async fn unknown_method_on_item_is_405() {
    let app = app().await;
    let (s, _) = call_raw(&app, "OPTIONS", "/1", "").await;
    assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn malformed_json_on_collection_is_400() {
    let app = app().await;
    let (s, text) = call_raw(&app, "POST", "/", "{nope").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        json!({"type": "danger", "message": "Malformed JSON body"})
    );
}

#[tokio::test]
async fn query_with_non_object_body_lists_everything() {
    let app = app().await;
    seed(&app).await;
    let (s, v) = call(&app, "QUERY", "/", Some(json!([1]))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["count"], 2);
}

#[tokio::test]
async fn csv_of_empty_list_is_empty_body() {
    let app = app().await;
    let (s, text) = call_raw(&app, "GET", "/?output_content_type=csv", "").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(text, "");
}

#[test]
fn csv_neutralizes_formulas() {
    let rows =
        [json!({"a": "=1+1", "b": "+x", "c": "-y", "d": "@z", "e": "\tq", "f": -5, "g": "ok"})];
    assert_eq!(
        format_csv(&rows),
        "a,b,c,d,e,f,g\n'=1+1,'+x,'-y,'@z,'\tq,-5,ok"
    );
}

#[test]
fn formatters_handle_empty_and_non_object_rows() {
    assert_eq!(format_csv(&[]), "");
    assert_eq!(format_csv(&[json!(1)]), "");
    assert_eq!(
        format_xml(&[json!(1)]),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<records>\n  <record>\n  </record>\n</records>\n"
    );
}
