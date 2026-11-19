// Shared by every OQ-E acceptance test. Not shown to the agent.
#![allow(dead_code, clippy::unwrap_used)]
use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::Value;
use tower::ServiceExt;

pub struct Res {
    pub status: StatusCode,
    pub json: Value,
    pub text: String,
    pub content_type: String,
}

pub async fn call(app: &Router, method: &str, uri: &str, headers: &[(&str, &str)], body: Option<Value>) -> Res {
    let mut b = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        b = b.header(*k, *v);
    }
    let req = match body {
        Some(v) => b.header("content-type", "application/json").body(Body::from(v.to_string())),
        None => b.body(Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let content_type = res
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 22).await.unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let json = serde_json::from_str(&text).unwrap_or(Value::Null);
    Res { status, json, text, content_type }
}
