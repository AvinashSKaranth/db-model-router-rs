//! Health route backed by [`DatabaseAdapter::ping`].

use crate::core::DatabaseAdapter;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{json, Value};
use std::sync::Arc;

/// `GET /` → `200 {"status":"ok"}` when the database answers, else
/// `503 {"status":"unavailable"}` (the error is logged, never returned).
pub fn health_router(adapter: Arc<dyn DatabaseAdapter>) -> Router {
    Router::new().route("/", get(health)).with_state(adapter)
}

async fn health(State(db): State<Arc<dyn DatabaseAdapter>>) -> (StatusCode, Json<Value>) {
    match db.ping().await {
        Ok(()) => (StatusCode::OK, Json(json!({ "status": "ok" }))),
        Err(e) => {
            tracing::warn!("[db-model-router] health check failed: {e}");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({ "status": "unavailable" })),
            )
        }
    }
}
