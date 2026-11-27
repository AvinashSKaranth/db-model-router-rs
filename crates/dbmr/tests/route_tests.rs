//! HTTP contract tests (BRD §20, OQ-A). SQLite in-memory; exact status codes and bodies.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use dbmr::adapters::SqliteAdapter;
use dbmr::core::{CurrentUser, Model};
use dbmr::http::create_model_router;
use serde_json::{json, Value};
use sqlx::sqlite::SqlitePoolOptions;
use std::sync::Arc;
use tower::ServiceExt;

async fn users_model() -> Model {
    // One connection: every pool checkout must see the same in-memory database.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            email TEXT NOT NULL UNIQUE,
            age INTEGER,
            note TEXT,
            tenant_id TEXT
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    Model::builder(Arc::new(SqliteAdapter::new(pool)), "users")
        .column("id", "integer")
        .column("name", "required|string|maxLength:20")
        .column("email", "required|email")
        .column("age", "integer")
        .column("note", "string")
        .column("tenant_id", "string")
        .unique(["email"])
        .search_columns(["name", "email"])
        .build()
        .unwrap()
}

async fn app() -> Router {
    create_model_router(users_model().await)
}

struct Res {
    status: StatusCode,
    content_type: String,
    allow: Option<String>,
    text: String,
}

impl Res {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or_else(|_| panic!("not JSON: {}", self.text))
    }
}

async fn call_with(app: &Router, mut req: Request<Body>, user: Option<Value>) -> Res {
    if let Some(u) = user {
        req.extensions_mut().insert(CurrentUser(u));
    }
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let allow = res
        .headers()
        .get(header::ALLOW)
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    Res {
        status,
        content_type,
        allow,
        text: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

fn req(method: &str, uri: &str, body: Option<Value>, tenant: Option<&str>) -> Request<Body> {
    let mut b = Request::builder().method(method).uri(uri);
    if let Some(t) = tenant {
        b = b.header("x-tenant", t);
    }
    match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    }
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> Res {
    call_with(app, req(method, uri, body, None), None).await
}

async fn seed(app: &Router) {
    let r = call(
        app,
        "POST",
        "/",
        Some(json!({"data": [
            {"name": "Ann", "email": "ann@x.io", "age": 30},
            {"name": "Bob", "email": "bob@x.io", "age": 40},
            {"name": "Cy", "email": "cy@x.io", "age": 50}
        ]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
}

const NOT_FOUND: &str = r#"{"message":"Not Found","type":"danger"}"#;

#[tokio::test]
async fn single_insert_ignores_path_id_and_pk() {
    let app = app().await;
    let r = call(
        &app,
        "POST",
        "/77",
        Some(json!({"id": 99, "name": "Ann", "email": "ann@x.io"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let rec = r.json();
    assert_eq!(rec["id"], 1);
    assert_eq!(rec["name"], "Ann");
}

#[tokio::test]
async fn bulk_put_requires_data_array_and_post_takes_one_object() {
    let app = app().await;
    let r = call(&app, "PUT", "/", Some(json!({"name": "x"}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        r.json(),
        json!({"type": "danger", "message": "Request body must contain a 'data' array"})
    );
    // `POST /` with one object creates one record and returns it.
    let r = call(
        &app,
        "POST",
        "/",
        Some(json!({"id": 9, "name": "Ann", "email": "ann@x.io"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(
        (&r.json()["id"], &r.json()["name"]),
        (&json!(1), &json!("Ann"))
    );
}

#[tokio::test]
async fn bulk_insert_summary() {
    let app = app().await;
    let r = call(
        &app,
        "POST",
        "/",
        Some(json!({"data": [
            {"name": "Ann", "email": "ann@x.io"}, {"name": "Bob", "email": "bob@x.io"}
        ]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let b = r.json();
    assert_eq!(b["rows"], 2);
    assert_eq!(b["type"], "success");
    assert_eq!(
        b["message"],
        dbmr::adapters::compiler::format_bulk_saved_message("users", 2)
    );
}

#[tokio::test]
async fn validation_is_422_with_niv_message() {
    let app = app().await;
    let r = call(&app, "POST", "/0", Some(json!({"email": "nope"}))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(
        r.json(),
        json!({"type": "danger", "message": "The name field is mandatory. The email must be a valid email address."})
    );
    let r = call(
        &app,
        "POST",
        "/",
        Some(json!({"data": [{"name": "Ann", "email": "a@x.io"}, {"email": "b@x.io"}]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json()["message"], "The data.1.name field is mandatory.");
}

#[tokio::test]
async fn get_by_id_and_404() {
    let app = app().await;
    seed(&app).await;
    let r = call(&app, "GET", "/2", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["name"], "Bob");
    let r = call(&app, "GET", "/999", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert_eq!(r.json(), serde_json::from_str::<Value>(NOT_FOUND).unwrap());
    // select_columns on by-id
    let r = call(&app, "GET", "/2?select_columns=id,name", None).await;
    assert_eq!(r.json(), json!({"id": 2, "name": "Bob"}));
}

#[tokio::test]
async fn query_method_by_id_and_list() {
    let app = app().await;
    seed(&app).await;
    let r = call(
        &app,
        "QUERY",
        "/",
        Some(json!({"age": ">=40", "sort": "-age"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK);
    let b = r.json();
    assert_eq!(b["count"], 2);
    assert_eq!(b["data"][0]["name"], "Cy");
    let r = call(
        &app,
        "QUERY",
        "/1",
        Some(json!({"select_columns": ["name"]})),
    )
    .await;
    assert_eq!(r.json(), json!({"name": "Ann"}));
    // malformed JSON body is ignored for QUERY
    let bad = Request::builder()
        .method("QUERY")
        .uri("/")
        .body(Body::from("{not json"))
        .unwrap();
    let r = call_with(&app, bad, None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["count"], 3);
}

#[tokio::test]
async fn list_filters_paging_search() {
    let app = app().await;
    seed(&app).await;
    let r = call(&app, "GET", "/?page=1&size=2&sort=name", None).await;
    let b = r.json();
    assert_eq!(b["count"], 3);
    assert_eq!(b["data"].as_array().unwrap().len(), 1);
    assert_eq!(b["data"][0]["name"], "Cy");
    let r = call(&app, "GET", "/?search=bo", None).await;
    assert_eq!(r.json()["count"], 1);
    let r = call(&app, "GET", "/?name=in(Ann,Cy)", None).await;
    assert_eq!(r.json()["count"], 2);
    let r = call(&app, "GET", "/?age=%3C40", None).await;
    assert_eq!(r.json()["data"][0]["name"], "Ann");
    let r = call(&app, "GET", "/?size=999", None).await;
    assert_eq!(r.json()["count"], 3);
}

#[tokio::test]
async fn put_patch_delete_with_precheck() {
    let app = app().await;
    seed(&app).await;
    let r = call(
        &app,
        "PUT",
        "/1",
        Some(json!({"name": "Ann2", "email": "ann@x.io", "age": 31})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json()["name"], "Ann2");
    let r = call(&app, "PATCH", "/1", Some(json!({"age": 32}))).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json()["age"], 32);
    assert_eq!(r.json()["name"], "Ann2");
    for m in ["PUT", "PATCH", "DELETE"] {
        let r = call(
            &app,
            m,
            "/999",
            Some(json!({"name": "x", "email": "z@x.io"})),
        )
        .await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{m}");
        assert_eq!(r.text, NOT_FOUND);
    }
    let r = call(&app, "DELETE", "/1", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(
        r.json(),
        json!({"rows": 1, "message": "1 users removed", "type": "success"})
    );
}

#[tokio::test]
async fn bulk_update_and_delete() {
    let app = app().await;
    seed(&app).await;
    let r = call(
        &app,
        "PUT",
        "/",
        Some(json!({"data": [
            {"id": 1, "name": "A", "email": "ann@x.io"}, {"id": 2, "name": "B", "email": "bob@x.io"}
        ]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(call(&app, "GET", "/2", None).await.json()["name"], "B");

    let r = call(&app, "DELETE", "/", Some(json!({}))).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        r.json(),
        json!({"type": "danger", "message": "Request body must contain filter criteria"})
    );
    let r = call(&app, "DELETE", "/", None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    let r = call(&app, "DELETE", "/", Some(json!({"age": ">=40"}))).await;
    assert_eq!(
        r.json(),
        json!({"rows": 2, "message": "2 userss removed", "type": "success"})
    );
    assert_eq!(call(&app, "GET", "/", None).await.json()["count"], 1);
}

#[tokio::test]
async fn csv_and_xml() {
    let app = app().await;
    call(
        &app,
        "POST",
        "/0",
        Some(json!({"name": "A,1", "email": "a@x.io"})),
    )
    .await;
    let r = call(
        &app,
        "GET",
        "/?output_content_type=csv&select_columns=id,name,note",
        None,
    )
    .await;
    assert_eq!(r.content_type, "text/csv");
    assert_eq!(r.text, "id,name,note\n1,\"A,1\",");
    let r = call(
        &app,
        "GET",
        "/1?output_content_type=xml&select_columns=name",
        None,
    )
    .await;
    assert_eq!(r.content_type, "application/xml");
    assert_eq!(
        r.text,
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<records>\n  <record>\n    <name>A,1</name>\n  </record>\n</records>\n"
    );
}

#[tokio::test]
async fn literal_null_string_is_stored_verbatim() {
    let app = app().await;
    let r = call_with(
        &app,
        req(
            "POST",
            "/0",
            Some(json!({"name": "Ann", "email": "a@x.io", "note": "null"})),
            Some("t"),
        ),
        None,
    )
    .await;
    assert_eq!(r.json()["note"], "null");
}

#[tokio::test]
async fn big_integers_are_lossless() {
    let app = app().await;
    let r = call(
        &app,
        "POST",
        "/0",
        Some(json!({"name": "A", "email": "a@x.io", "age": 9_007_199_254_740_993_i64})),
    )
    .await;
    assert!(r.text.contains("9007199254740993"), "{}", r.text);
}

#[tokio::test]
async fn server_errors_are_scrubbed() {
    let app = app().await;
    call(
        &app,
        "POST",
        "/0",
        Some(json!({"name": "A", "email": "a@x.io"})),
    )
    .await;
    // A declared unique key is checked before the database: 409 with a clear message.
    let r = call(
        &app,
        "POST",
        "/0",
        Some(json!({"name": "B", "email": "a@x.io"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT);
    assert_eq!(
        r.json()["message"],
        "Duplicate value for unique key (email)."
    );
    // A database error (column missing from the table) → 500, details hidden.
    let ghost = Model::builder(users_model().await.adapter().clone(), "users")
        .column("name", "string")
        .column("ghost", "string")
        .build()
        .unwrap();
    let app = create_model_router(ghost);
    let r = call(&app, "POST", "/0", Some(json!({"name": "C", "ghost": "x"}))).await;
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
    if std::env::var("NODE_ENV").is_err() {
        assert_eq!(
            r.json(),
            json!({"type": "danger", "message": "Internal server error"})
        );
    }
}

#[tokio::test]
async fn hostile_identifiers_cannot_inject() {
    let app = app().await;
    seed(&app).await;
    // Reject undeclared client columns before compiling identifiers for the adapter.
    let r = call(&app, "GET", "/?name%22%20OR%201%3D1%20--=x", None).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);
    let r = call(
        &app,
        "GET",
        "/?sort=name%22%3B%20DROP%20TABLE%20users%3B%20--",
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{}", r.text);
    assert_eq!(call(&app, "GET", "/", None).await.json()["count"], 3);
    // Scalar IN never widens to "no filter".
    let r = call(
        &app,
        "QUERY",
        "/",
        Some(json!({"filter": [[["id", "in", 2]]]})),
    )
    .await;
    assert_eq!(r.json()["count"], 1, "{}", r.text);
}

#[tokio::test]
async fn unknown_method_is_405() {
    let app = app().await;
    let r = call(&app, "TRACE", "/", None).await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    assert!(r.allow.is_some());
}

#[tokio::test]
async fn options_method_on_collection_and_item() {
    let app = app().await;
    let r_coll = call(&app, "OPTIONS", "/", None).await;
    assert_eq!(r_coll.status, StatusCode::OK);
    let allow = r_coll.allow.as_deref().expect("allow header on collection");
    assert!(allow.contains("OPTIONS"));
    assert!(allow.contains("GET"));
    assert!(allow.contains("POST"));
    assert!(allow.contains("DISCOVER"));
    assert_eq!(r_coll.json()["resource"], "collection");

    let r_item = call(&app, "OPTIONS", "/1", None).await;
    assert_eq!(r_item.status, StatusCode::OK);
    let allow_item = r_item.allow.as_deref().expect("allow header on item");
    assert!(allow_item.contains("OPTIONS"));
    assert!(allow_item.contains("GET"));
    assert!(allow_item.contains("PATCH"));
    assert_eq!(r_item.json()["resource"], "item");
}

#[tokio::test]
async fn malformed_json_is_400() {
    let app = app().await;
    let r = Request::builder()
        .method("POST")
        .uri("/0")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let r = call_with(&app, r, None).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        r.json(),
        json!({"message": "Malformed JSON body", "type": "danger"})
    );
}

#[tokio::test]
async fn item_route_path_id_is_literal_not_filter_operator() {
    let app = app().await;
    seed(&app).await;
    // Attempting range delete via path ID `DELETE /%3E%3D1` (which is '>=1')
    let r = call(&app, "DELETE", "/%3E%3D1", None).await;
    // Must be 404 Not Found because literal PK equality is applied
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    // Verify that all 3 rows still exist!
    let list = call(&app, "GET", "/", None).await;
    assert_eq!(list.json()["count"], 3);
}
