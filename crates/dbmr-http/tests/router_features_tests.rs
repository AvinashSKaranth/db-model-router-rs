//! Router options, hidden columns, optimistic locking over HTTP, scoped relations,
//! `?include=`, OpenAPI generation and the health route. SQLite in-memory.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use axum::Router;
use dbmr_adapters::SqliteAdapter;
use dbmr_core::{Context, CurrentUser, DatabaseAdapter, Model, Scope, CONFLICT_MESSAGE};
use dbmr_http::{
    create_model_router, create_model_router_with, health_router, openapi, openapi_route,
    RouterOptions,
};
use serde_json::{json, Value};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;
use std::sync::Arc;
use tower::ServiceExt;

async fn pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    for ddl in [
        "CREATE TABLE accounts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            email TEXT,
            password_hash TEXT,
            deleted INTEGER NOT NULL DEFAULT 0,
            version INTEGER,
            tenant_id TEXT
        )",
        "CREATE TABLE posts (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            account_id INTEGER,
            title TEXT
        )",
    ] {
        sqlx::query(ddl).execute(&pool).await.unwrap();
    }
    pool
}

/// `(accounts with hidden password_hash + posts relation, posts with account relation)`.
async fn models(versioned: bool) -> (Model, Model) {
    let db = Arc::new(SqliteAdapter::new(pool().await));
    let mut b = Model::builder(db.clone(), "accounts")
        .column("id", "integer")
        .column("name", "required|string|maxLength:20")
        .column("email", "nullable|email")
        .column("password_hash", "string")
        .column("deleted", "boolean")
        .column("tenant_id", "string")
        .safe_delete("deleted")
        .hidden(["password_hash"]);
    if versioned {
        b = b.version_column("version");
    }
    let accounts = b.build().unwrap();
    let posts = Model::builder(db, "posts")
        .column("id", "integer")
        .column("account_id", "integer")
        .column("title", "string|in:a,b,c")
        .build()
        .unwrap();
    let accounts_r = accounts.has_many("posts", &posts, "account_id");
    let posts_r = posts.belongs_to("account", &accounts, "account_id");
    (accounts_r, posts_r)
}

struct Res {
    status: StatusCode,
    headers: HeaderMap,
    text: String,
}

impl Res {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or_else(|_| panic!("not JSON: {}", self.text))
    }
    fn etag(&self) -> Option<&str> {
        self.headers.get(header::ETAG).map(|v| v.to_str().unwrap())
    }
}

async fn send(
    app: &Router,
    method: &str,
    uri: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
    user: Option<Value>,
) -> Res {
    let mut b = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let mut req = match body {
        Some(v) => b
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(v.to_string()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    if let Some(u) = user {
        req.extensions_mut().insert(CurrentUser(u));
    }
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    Res {
        status,
        headers,
        text: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> Res {
    send(app, method, uri, body, &[], None).await
}

fn danger(message: &str) -> Value {
    json!({"message": message, "type": "danger"})
}

// ------------------------------------------------------------------ 1. options

#[tokio::test]
async fn read_only_router_answers_405_for_writes() {
    let (accounts, _) = models(false).await;
    accounts
        .insert(
            dbmr_core::json_to_row(&json!({"name": "Ann"})),
            &Context::new(),
        )
        .await
        .unwrap();
    let app = create_model_router_with(accounts, RouterOptions::read_only());
    for (m, uri, body) in [
        ("POST", "/0", json!({"name": "x"})),
        ("POST", "/", json!({"data": [{"name": "x"}]})),
        ("PUT", "/", json!({"data": [{"id": 1, "name": "x"}]})),
        ("PUT", "/1", json!({"name": "x"})),
        ("PATCH", "/1", json!({"name": "x"})),
        ("DELETE", "/1", json!({})),
        ("DELETE", "/", json!({"name": "Ann"})),
    ] {
        let r = call(&app, m, uri, Some(body)).await;
        assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED, "{m} {uri}");
        assert_eq!(r.json(), danger("Method Not Allowed"), "{m} {uri}");
    }
    assert_eq!(call(&app, "GET", "/", None).await.json()["count"], 1);
    assert_eq!(
        call(&app, "QUERY", "/", Some(json!({}))).await.json()["count"],
        1
    );
    assert_eq!(call(&app, "GET", "/1", None).await.json()["name"], "Ann");
    assert_eq!(call(&app, "QUERY", "/1", None).await.json()["name"], "Ann");
    let r = call(&app, "GET", "/?output_content_type=csv", None).await;
    assert_eq!(r.status, StatusCode::OK);
    // Unknown methods share the same 405 body.
    let r = call(&app, "OPTIONS", "/", None).await;
    assert_eq!(r.json(), danger("Method Not Allowed"));
}

#[tokio::test]
async fn single_and_bulk_flags_are_independent_and_export_can_be_off() {
    let (accounts, _) = models(false).await;
    let opts = RouterOptions {
        bulk_create: false,
        list: false,
        export: false,
        ..RouterOptions::default()
    };
    let app = create_model_router_with(accounts, opts);
    let r = call(&app, "POST", "/0", Some(json!({"name": "Ann"}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = call(&app, "POST", "/", Some(json!({"data": [{"name": "B"}]}))).await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    let r = call(&app, "POST", "/", Some(json!({"name": "B"}))).await;
    assert_eq!(
        r.status,
        StatusCode::OK,
        "single create on the collection route"
    );
    let r = call(&app, "GET", "/", None).await;
    assert_eq!(r.status, StatusCode::METHOD_NOT_ALLOWED);
    for fmt in ["csv", "xml"] {
        let r = call(&app, "GET", &format!("/1?output_content_type={fmt}"), None).await;
        assert_eq!(r.status, StatusCode::BAD_REQUEST);
        assert_eq!(r.json(), danger("Export is disabled"));
    }
    assert_eq!(call(&app, "GET", "/1", None).await.json()["name"], "Ann");
}

// ------------------------------------------------------------------ 3. hidden columns

#[tokio::test]
async fn hidden_columns_are_not_writable_or_filterable() {
    let (accounts, _) = models(false).await;
    let app = create_model_router(accounts.clone());
    let r = call(
        &app,
        "POST",
        "/0",
        Some(json!({"name": "Ann", "password_hash": "evil"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert!(r.json().get("password_hash").is_none());
    let r = call(
        &app,
        "POST",
        "/",
        Some(json!({"data": [{"name": "Bob", "password_hash": "evil"}]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = call(
        &app,
        "PATCH",
        "/1",
        Some(json!({"name": "Ann2", "password_hash": "evil"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let r = call(
        &app,
        "PUT",
        "/",
        Some(json!({"data": [{"id": 2, "name": "Bob2", "password_hash": "evil"}]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let ctx = Context::new();
    for id in [1, 2] {
        let rec = accounts
            .with_hidden()
            .by_id(id, &ctx)
            .await
            .unwrap()
            .unwrap();
        assert!(
            rec.get("password_hash")
                .is_none_or(dbmr_core::DbValue::is_null),
            "row {id} got a client-written hash"
        );
    }

    for uri in [
        "/?password_hash=x",
        "/?password_hash=%25a%25",
        "/?sort=-password_hash",
        "/?select_columns=id,password_hash",
        "/1?password_hash=x",
        "/1?select_columns=password_hash",
    ] {
        let r = call(&app, "GET", uri, None).await;
        assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY, "{uri}");
        assert_eq!(r.json(), danger("Invalid filter Inputs"));
    }
    let raw = json!({"filter": [[["password_hash", "=", "x"]]]});
    let r = call(&app, "QUERY", "/", Some(raw.clone())).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = call(&app, "DELETE", "/", Some(raw)).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    let r = call(&app, "DELETE", "/", Some(json!({"password_hash": "x"}))).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    // Other columns still filter.
    assert_eq!(
        call(&app, "GET", "/?name=Ann2", None).await.json()["count"],
        1
    );
}

// ------------------------------------------------------------------ 4. optimistic locking

#[tokio::test]
async fn etag_and_if_match_drive_optimistic_locking() {
    let (accounts, _) = models(true).await;
    let app = create_model_router(accounts);
    let r = call(&app, "POST", "/0", Some(json!({"name": "Ann"}))).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.etag(), Some("\"1\""));
    let r = call(&app, "GET", "/1", None).await;
    assert_eq!(r.etag(), Some("\"1\""));
    assert_eq!(r.json()["version"], 1);

    // No version anywhere → 428.
    let r = call(&app, "PUT", "/1", Some(json!({"name": "A2"}))).await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED, "{}", r.text);
    assert_eq!(r.json()["type"], "danger");
    // If-Match supplies it.
    let r = send(
        &app,
        "PUT",
        "/1",
        Some(json!({"name": "A2"})),
        &[("if-match", "\"1\"")],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.etag(), Some("\"2\""));
    // Stale (weak tags tolerated) → 409.
    let r = send(
        &app,
        "PATCH",
        "/1",
        Some(json!({"name": "A3"})),
        &[("if-match", "W/\"1\"")],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json(), danger(CONFLICT_MESSAGE));
    // A body version wins over If-Match; unquoted If-Match accepted.
    let r = send(
        &app,
        "PATCH",
        "/1",
        Some(json!({"name": "A3", "version": 2})),
        &[("if-match", "1")],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.etag(), Some("\"3\""));

    // DELETE: stale If-Match → 409 and the row survives; matching → deleted.
    let r = send(&app, "DELETE", "/1", None, &[("if-match", "\"2\"")], None).await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    assert_eq!(r.json(), danger(CONFLICT_MESSAGE));
    assert_eq!(call(&app, "GET", "/1", None).await.status, StatusCode::OK);
    let r = send(&app, "DELETE", "/1", None, &[("if-match", "\"3\"")], None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json()["rows"], 1);
    assert_eq!(
        call(&app, "GET", "/1", None).await.status,
        StatusCode::NOT_FOUND
    );

    // DELETE without If-Match behaves as before.
    call(&app, "POST", "/0", Some(json!({"name": "Bob"}))).await;
    let r = call(&app, "DELETE", "/2", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json()["rows"], 1);
}

#[tokio::test]
async fn bulk_put_carries_versions_per_row() {
    let (accounts, _) = models(true).await;
    let app = create_model_router(accounts);
    call(
        &app,
        "POST",
        "/",
        Some(json!({"data": [{"name": "A"}, {"name": "B"}]})),
    )
    .await;
    let r = call(
        &app,
        "PUT",
        "/",
        Some(json!({"data": [{"id": 1, "name": "A2", "version": 1}, {"id": 2, "name": "B2", "version": 1}]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json()["rows"], 2);
    let r = call(
        &app,
        "PUT",
        "/",
        Some(json!({"data": [{"id": 1, "name": "A3", "version": 1}]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::CONFLICT, "{}", r.text);
    let r = call(
        &app,
        "PUT",
        "/",
        Some(json!({"data": [{"id": 1, "name": "A3"}]})),
    )
    .await;
    assert_eq!(r.status, StatusCode::PRECONDITION_REQUIRED, "{}", r.text);
}

// ------------------------------------------------------------------ 5. include

#[tokio::test]
async fn include_works_on_list_query_and_item() {
    let (accounts, posts) = models(false).await;
    let accounts_app = create_model_router(accounts);
    let posts_app = create_model_router(posts);
    call(
        &accounts_app,
        "POST",
        "/0",
        Some(json!({"name": "Ann", "password_hash": "h"})),
    )
    .await;
    call(
        &posts_app,
        "POST",
        "/",
        Some(json!({"data": [{"account_id": 1, "title": "a"}, {"account_id": 1, "title": "b"}]})),
    )
    .await;

    let r = call(&accounts_app, "GET", "/?include=posts", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json()["data"][0]["posts"].as_array().unwrap().len(), 2);
    let r = call(
        &accounts_app,
        "QUERY",
        "/",
        Some(json!({"include": ["posts"]})),
    )
    .await;
    assert_eq!(r.json()["data"][0]["posts"].as_array().unwrap().len(), 2);
    let r = call(&accounts_app, "GET", "/1?include=posts", None).await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    assert_eq!(r.json()["posts"][1]["title"], "b");
    let r = call(&accounts_app, "GET", "/9?include=posts", None).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    let r = call(&posts_app, "GET", "/1?include=account", None).await;
    assert_eq!(r.json()["account"]["name"], "Ann");
    assert!(r.json()["account"].get("password_hash").is_none());

    let r = call(&accounts_app, "GET", "/?include=nope", None).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json()["type"], "danger");
}

// ------------------------------------------------------------------ 6. OpenAPI

#[tokio::test]
async fn openapi_document_follows_models_and_options() {
    let (accounts, posts) = models(true).await;
    let ro = RouterOptions::read_only();
    let all = RouterOptions::default();
    let spec = openapi(
        "Test",
        "1.0.0",
        &[
            ("/accounts", &accounts, &all),
            ("/accounts/{account_id}/posts", &posts, &ro),
        ],
    );
    assert_eq!(spec["openapi"], "3.1.0");
    assert_eq!(spec["info"]["title"], "Test");
    let paths = spec["paths"].as_object().unwrap();
    let methods = |p: &str| -> Vec<String> {
        let mut m: Vec<String> = paths[p].as_object().unwrap().keys().cloned().collect();
        m.sort();
        m
    };
    assert_eq!(methods("/accounts"), ["delete", "get", "post", "put"]);
    assert_eq!(
        methods("/accounts/{id}"),
        ["delete", "get", "patch", "post", "put"]
    );
    assert_eq!(methods("/accounts/{account_id}/posts"), ["get"]);
    assert_eq!(methods("/accounts/{account_id}/posts/{id}"), ["get"]);
    let nested_params = paths["/accounts/{account_id}/posts/{id}"]["get"]["parameters"]
        .as_array()
        .unwrap();
    assert!(nested_params
        .iter()
        .any(|p| p["name"] == "account_id" && p["in"] == "path"));

    let schemas = &spec["components"]["schemas"];
    let rec = &schemas["accounts"]["properties"];
    assert!(
        rec.get("password_hash").is_none(),
        "hidden column documented"
    );
    assert!(rec.get("deleted").is_none(), "soft-delete flag documented");
    assert_eq!(rec["name"]["type"], "string");
    assert_eq!(rec["name"]["maxLength"], 20);
    assert_eq!(rec["email"]["type"], json!(["string", "null"]));
    assert_eq!(rec["email"]["format"], "email");
    assert_eq!(rec["version"]["type"], "integer");
    assert!(rec["version"]["description"].is_string());
    let create = &schemas["accountsCreate"];
    assert_eq!(create["required"], json!(["name"]));
    assert!(create["properties"].get("id").is_none());
    assert!(create["properties"].get("version").is_none());
    assert_eq!(
        schemas["posts"]["properties"]["title"]["enum"],
        json!(["a", "b", "c"])
    );

    // Includes enumerate relation names; versioned writes document If-Match / ETag / 409 / 428.
    let get_item = &paths["/accounts/{id}"]["get"];
    let include = get_item["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "include")
        .unwrap();
    assert_eq!(include["schema"]["items"]["enum"], json!(["posts"]));
    assert!(get_item["responses"]["200"]["headers"]["ETag"].is_object());
    let put = &paths["/accounts/{id}"]["put"];
    assert!(put["parameters"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["name"] == "If-Match"));
    for code in ["400", "401", "404", "405", "409", "422", "428", "500"] {
        assert_eq!(
            put["responses"][code]["content"]["application/json"]["schema"]["$ref"],
            "#/components/schemas/Error",
            "{code}"
        );
    }
    let bulk = &paths["/accounts"]["post"]["requestBody"]["content"]["application/json"]["schema"];
    assert_eq!(
        bulk["properties"]["data"]["items"]["$ref"],
        "#/components/schemas/accountsCreate"
    );
    let list =
        &paths["/accounts"]["get"]["responses"]["200"]["content"]["application/json"]["schema"];
    assert!(list["properties"]["count"].is_object());

    // Served as JSON.
    let app = openapi_route(spec.clone());
    let r = call(&app, "GET", "/", None).await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.json(), spec);
}

// ------------------------------------------------------------------ 7. health

#[tokio::test]
async fn health_reports_database_state_without_leaking_errors() {
    let pool = pool().await;
    let db: Arc<dyn DatabaseAdapter> = Arc::new(SqliteAdapter::new(pool.clone()));
    let app = health_router(db);
    let r = call(&app, "GET", "/", None).await;
    assert_eq!(
        (r.status, r.json()),
        (StatusCode::OK, json!({"status": "ok"}))
    );
    pool.close().await;
    let r = call(&app, "GET", "/", None).await;
    assert_eq!(
        (r.status, r.json()),
        (
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"status": "unavailable"})
        )
    );
}

// ------------------------------------------------------------------ 8. scoped relations

/// Test auth middleware: the scope is the user's tenant.
async fn scope_from_user(
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let tenant = req
        .extensions()
        .get::<CurrentUser>()
        .and_then(|u| u.0.get("tenant_id").cloned());
    if let Some(t) = tenant {
        req.extensions_mut().insert(Scope::new([("tenant_id", t)]));
    }
    next.run(req).await
}

/// Tenancy across relations: `?include=` only returns related rows inside the request's
/// scope, and a written `belongs_to` foreign key must point inside it.
#[tokio::test]
async fn includes_and_foreign_keys_stay_inside_the_scope() {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    for ddl in [
        "CREATE TABLE users (id INTEGER PRIMARY KEY, tenant_id TEXT, email TEXT)",
        "CREATE TABLE posts (id INTEGER PRIMARY KEY AUTOINCREMENT, tenant_id TEXT, title TEXT, author_id INTEGER)",
        "CREATE TABLE comments (id INTEGER PRIMARY KEY AUTOINCREMENT, tenant_id TEXT, post_id INTEGER, body TEXT)",
    ] {
        sqlx::query(ddl).execute(&pool).await.unwrap();
    }
    let db = Arc::new(SqliteAdapter::new(pool));
    let model = |t: &str, cols: &[&str]| {
        cols.iter()
            .fold(
                Model::builder(db.clone(), t).column("id", "integer"),
                |b, c| {
                    b.column(
                        c,
                        if c.ends_with("_id") && *c != "tenant_id" {
                            "nullable|integer"
                        } else {
                            "nullable|string"
                        },
                    )
                },
            )
            .build()
            .unwrap()
    };
    let users = model("users", &["tenant_id", "email"]);
    let posts = model("posts", &["tenant_id", "title", "author_id"]);
    let comments = model("comments", &["tenant_id", "post_id", "body"]);
    let comments_r = comments.belongs_to("post", &posts, "post_id");
    let posts_r = posts
        .belongs_to("author", &users, "author_id")
        .has_many("comments", &comments, "post_id");

    // Unscoped seed (a job with Context::new()): user 1 in A, user 7 in B, A's post 1.
    let ctx = Context::new();
    for (id, t) in [(1, "A"), (7, "B")] {
        users
            .insert(
                dbmr_core::types::row([("id", id.into()), ("tenant_id", t.into())]),
                &ctx,
            )
            .await
            .unwrap();
    }
    let seed = |v: Value| dbmr_core::types::json_to_row(&v);
    posts
        .insert(
            seed(json!({"tenant_id":"A","title":"a1","author_id":1})),
            &ctx,
        )
        .await
        .unwrap();

    let app = Router::new()
        .nest("/posts", create_model_router(posts_r))
        .nest("/comments", create_model_router(comments_r))
        .layer(axum::middleware::from_fn(scope_from_user));
    let alice = Some(json!({"tenant_id": "A"}));
    let bob = Some(json!({"tenant_id": "B"}));
    let invalid = |fk: &str| danger(&format!("Invalid reference: {fk}"));

    // Write: a foreign key into another tenant is rejected (single, bulk, patch).
    let r = send(
        &app,
        "POST",
        "/posts/0",
        Some(json!({"title":"x","author_id":7})),
        &[],
        alice.clone(),
    )
    .await;
    assert_eq!(
        (r.status, r.json()),
        (StatusCode::UNPROCESSABLE_ENTITY, invalid("author_id"))
    );
    let r = send(
        &app,
        "POST",
        "/posts/0",
        Some(json!({"title":"ok","author_id":1})),
        &[],
        alice.clone(),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text);
    let bulk = json!({"data":[{"title":"b0","author_id":1},{"title":"b1","author_id":7}]});
    let r = send(&app, "POST", "/posts", Some(bulk), &[], alice.clone()).await;
    assert_eq!(r.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(r.json()["message"], "data.1: Invalid reference: author_id");
    let r = send(
        &app,
        "PATCH",
        "/posts/1",
        Some(json!({"author_id":7})),
        &[],
        alice.clone(),
    )
    .await;
    assert_eq!(
        (r.status, r.json()),
        (StatusCode::UNPROCESSABLE_ENTITY, invalid("author_id"))
    );
    let r = send(
        &app,
        "POST",
        "/comments/0",
        Some(json!({"post_id":1,"body":"spam"})),
        &[],
        bob,
    )
    .await;
    assert_eq!(
        (r.status, r.json()),
        (StatusCode::UNPROCESSABLE_ENTITY, invalid("post_id"))
    );
    let r = send(
        &app,
        "POST",
        "/comments/0",
        Some(json!({"post_id":1,"body":"hi"})),
        &[],
        alice.clone(),
    )
    .await;
    assert!(r.status.is_success(), "{}", r.text);

    // Read: rows that crossed tenants anyway (written unscoped) are not included.
    posts
        .insert(
            seed(json!({"tenant_id":"A","title":"leak","author_id":7})),
            &ctx,
        )
        .await
        .unwrap();
    comments
        .insert(
            seed(json!({"tenant_id":"B","post_id":1,"body":"spam"})),
            &ctx,
        )
        .await
        .unwrap();
    let r = send(
        &app,
        "GET",
        "/posts?include=author,comments&sort=id",
        None,
        &[],
        alice,
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.text);
    let data = r.json()["data"].clone();
    let by_title = |t: &str| {
        data.as_array()
            .unwrap()
            .iter()
            .find(|p| p["title"] == t)
            .unwrap()
            .clone()
    };
    assert_eq!(by_title("a1")["author"]["id"], 1);
    let bodies: Vec<_> = by_title("a1")["comments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["body"].clone())
        .collect();
    assert_eq!(bodies, vec![json!("hi")], "B's comment must not leak");
    assert_eq!(
        by_title("leak")["author"],
        Value::Null,
        "B's user must not leak"
    );
}
