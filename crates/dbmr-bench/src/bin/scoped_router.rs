//! Compare equivalent tenant-scoped Axum endpoints over one SQLite pool.
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Method, Request, StatusCode};
use axum::routing::get;
use axum::{Extension, Json, Router};
use dbmr::adapters::SqliteAdapter;
use dbmr::http::{create_model_router_with, RouterOptions};
use dbmr::{Model, Scope};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tower::ServiceExt;

type Error = Box<dyn std::error::Error + Send + Sync>;
type Note = (i64, String, String);

fn note_json((id, tenant_id, title): Note) -> Value {
    json!({ "id": id, "tenant_id": tenant_id, "title": title })
}

fn tenant(scope: &Scope) -> Result<&str, StatusCode> {
    scope
        .0
        .get("tenant_id")
        .and_then(Value::as_str)
        .ok_or(StatusCode::UNAUTHORIZED)
}

async fn raw_get(
    State(pool): State<SqlitePool>,
    Extension(scope): Extension<Scope>,
    Path(id): Path<i64>,
) -> Result<Json<Value>, StatusCode> {
    let note = sqlx::query_as::<_, Note>(
        "SELECT id, tenant_id, title FROM notes WHERE id = ? AND tenant_id = ?",
    )
    .bind(id)
    .bind(tenant(&scope)?)
    .fetch_optional(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    note.map(note_json).map(Json).ok_or(StatusCode::NOT_FOUND)
}

async fn raw_list(
    State(pool): State<SqlitePool>,
    Extension(scope): Extension<Scope>,
) -> Result<Json<Value>, StatusCode> {
    let tenant = tenant(&scope)?;
    let notes = sqlx::query_as::<_, Note>(
        "SELECT id, tenant_id, title FROM notes WHERE tenant_id = ? ORDER BY id LIMIT 30",
    )
    .bind(tenant)
    .fetch_all(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM notes WHERE tenant_id = ?")
        .bind(tenant)
        .fetch_one(&pool)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(
        json!({"data": notes.into_iter().map(note_json).collect::<Vec<_>>(), "count": count}),
    ))
}

#[derive(Deserialize)]
struct Change {
    title: String,
}

async fn raw_patch(
    State(pool): State<SqlitePool>,
    Extension(scope): Extension<Scope>,
    Path(id): Path<i64>,
    Json(change): Json<Change>,
) -> Result<Json<Value>, StatusCode> {
    if change.title.is_empty() {
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let note = sqlx::query_as::<_, Note>(
        "UPDATE notes SET title = ? WHERE id = ? AND tenant_id = ? RETURNING id, tenant_id, title",
    )
    .bind(change.title)
    .bind(id)
    .bind(tenant(&scope)?)
    .fetch_optional(&pool)
    .await
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    note.map(note_json).map(Json).ok_or(StatusCode::NOT_FOUND)
}

#[derive(Clone, Copy, Debug)]
enum Scenario {
    Get,
    List,
    Patch,
}

impl Scenario {
    fn request(self, i: usize) -> Result<Request<Body>, axum::http::Error> {
        let id = i % 256 + 1;
        let request = Request::builder();
        match self {
            Self::Get => request.uri(format!("/notes/{id}")).body(Body::empty()),
            Self::List => request.uri("/notes?sort=id").body(Body::empty()),
            Self::Patch => request
                .method(Method::PATCH)
                .uri(format!("/notes/{id}"))
                .header("content-type", "application/json")
                .body(Body::from(r#"{"title":"updated"}"#)),
        }
    }
}

async fn dispatch(
    router: Router,
    scenario: Scenario,
    i: usize,
) -> Result<axum::body::Bytes, Error> {
    let response = router.oneshot(scenario.request(i)?).await?;
    if response.status() != StatusCode::OK {
        return Err(format!("{scenario:?} returned {}", response.status()).into());
    }
    Ok(axum::body::to_bytes(response.into_body(), 1 << 20).await?)
}

async fn measure(router: &Router, scenario: Scenario, concurrency: usize) -> Result<Value, Error> {
    const REQUESTS: usize = 2_000;
    for i in 0..100 {
        dispatch(router.clone(), scenario, i).await?;
    }
    let mut jobs = tokio::task::JoinSet::new();
    let wall = Instant::now();
    for worker in 0..concurrency {
        let router = router.clone();
        jobs.spawn(async move {
            let mut samples = Vec::new();
            for i in (worker..REQUESTS).step_by(concurrency) {
                let start = Instant::now();
                dispatch(router.clone(), scenario, i).await?;
                samples.push(start.elapsed());
            }
            Ok::<Vec<Duration>, Error>(samples)
        });
    }
    let mut samples = Vec::with_capacity(REQUESTS);
    while let Some(result) = jobs.join_next().await {
        samples.extend(result??);
    }
    let seconds = wall.elapsed().as_secs_f64();
    samples.sort_unstable();
    let ms = |percent: usize| samples[(samples.len() - 1) * percent / 100].as_secs_f64() * 1_000.0;
    Ok(json!({"p50_ms": ms(50), "p95_ms": ms(95), "p99_ms": ms(99),
        "requests_per_second": 2_000.0 / seconds}))
}

#[tokio::main]
async fn main() -> Result<(), Error> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await?;
    for sql in [
        "CREATE TABLE notes (id INTEGER PRIMARY KEY, tenant_id TEXT NOT NULL, title TEXT NOT NULL)",
        "CREATE INDEX notes_by_tenant ON notes (tenant_id, id)",
        "WITH RECURSIVE ids(id) AS (SELECT 1 UNION ALL SELECT id + 1 FROM ids WHERE id < 512)
         INSERT INTO notes SELECT id, CASE WHEN id <= 256 THEN 'a' ELSE 'b' END, 'hello' FROM ids",
    ] {
        sqlx::query(sql).execute(&pool).await?;
    }
    let model = Model::builder(Arc::new(SqliteAdapter::new(pool.clone())), "notes")
        .columns([
            ("id", "integer"),
            ("tenant_id", "required|string"),
            ("title", "required|string"),
        ])
        .require_scope(["tenant_id"])
        .build()?;
    let scope = Extension(Scope::new([("tenant_id", "a")]));
    let raw = Router::new()
        .route("/notes", get(raw_list))
        .route("/notes/{id}", get(raw_get).patch(raw_patch))
        .with_state(pool)
        .layer(scope.clone());
    let generated = Router::new()
        .nest(
            "/notes",
            create_model_router_with(
                model,
                RouterOptions {
                    patch: true,
                    export: false,
                    ..RouterOptions::read_only()
                },
            ),
        )
        .layer(scope);
    for scenario in [Scenario::Get, Scenario::List, Scenario::Patch] {
        let raw_body: Value = serde_json::from_slice(&dispatch(raw.clone(), scenario, 0).await?)?;
        let generated_body: Value =
            serde_json::from_slice(&dispatch(generated.clone(), scenario, 0).await?)?;
        if raw_body != generated_body {
            return Err(format!("{scenario:?} returned different JSON").into());
        }
    }
    let mut results = Vec::new();
    for concurrency in [1, 8] {
        for scenario in [Scenario::Get, Scenario::List, Scenario::Patch] {
            let raw = measure(&raw, scenario, concurrency).await?;
            let dbmr = measure(&generated, scenario, concurrency).await?;
            results.push(json!({"scenario": format!("{scenario:?}"), "concurrency": concurrency, "sqlx": raw, "dbmr": dbmr}));
        }
    }
    println!("{}", serde_json::to_string_pretty(&results)?);
    Ok(())
}
