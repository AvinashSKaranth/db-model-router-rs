//! `error_response` message policy. Own test binary with a single test because it mutates
//! the process-wide `NODE_ENV`.
#![allow(clippy::unwrap_used)]

use axum::http::StatusCode;
use dbmr::core::AppError;
use dbmr::http::error_response;
use serde_json::{json, Value};

async fn body(err: &AppError) -> (StatusCode, Value) {
    let res = error_response(err);
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 16)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn messages_follow_status_and_node_env() {
    // 4xx with empty message falls back to "Bad request".
    assert_eq!(
        body(&AppError::BadRequest(String::new())).await,
        (
            StatusCode::BAD_REQUEST,
            json!({"type": "danger", "message": "Bad request"})
        )
    );
    assert_eq!(
        body(&AppError::NotFound("gone".into())).await.1["message"],
        "gone"
    );

    let db = AppError::Database("no such table: x".into());
    for env in ["development", "test", "TEST"] {
        std::env::set_var("NODE_ENV", env);
        assert_eq!(
            body(&db).await,
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"type": "danger", "message": "no such table: x"})
            ),
            "NODE_ENV={env}"
        );
        // Empty 5xx message is still scrubbed in dev.
        assert_eq!(
            body(&AppError::Internal(String::new())).await.1["message"],
            "Internal server error"
        );
    }

    std::env::set_var("NODE_ENV", "production");
    assert_eq!(body(&db).await.1["message"], "Internal server error");
    std::env::remove_var("NODE_ENV");
    assert_eq!(body(&db).await.1["message"], "Internal server error");
}
