//! Model router (§20): Node `route.js` parity on axum.
//!
//! ```no_run
//! # fn demo(users: dbmr::Model) {
//! use axum::{extract::Request, middleware::{self, Next}, response::Response};
//! use dbmr::{CurrentUser, Scope};
//! use serde_json::json;
//!
//! // Your auth middleware decides who the caller is and which rows they may touch.
//! async fn auth(mut req: Request, next: Next) -> Response {
//!     let tenant = "A"; // from your session / token
//!     req.extensions_mut().insert(CurrentUser(json!({ "id": 7, "tenant_id": tenant })));
//!     req.extensions_mut().insert(Scope::new([("tenant_id", json!(tenant))]));
//!     next.run(req).await
//! }
//!
//! let app: axum::Router = axum::Router::new()
//!     .nest("/api/users", dbmr::create_model_router(users))
//!     .layer(middleware::from_fn(auth));
//! # }
//! ```
//!
//! | Route | Model call |
//! |---|---|
//! | `GET/QUERY /{id}` | `find` (pk = path) → record or 404 |
//! | `POST /{id}` | `insert` (path id ignored, PK stripped) |
//! | `PUT/PATCH/DELETE /{id}` | `find_one` pre-check (pk) → `update`/`patch`/`remove` |
//! | `GET/QUERY /` | `list` → `{data, count}` |
//! | `POST /` | `insert` for an object body, `insert_many` for `{data: [...]}` |
//! | `PUT /` | `update_many` on `{data: [...]}` |
//! | `DELETE /` | `remove` (non-empty body required) |
//!
//! The router copies a [`crate::core::CurrentUser`] request extension to `ctx.user` and a
//! [`crate::core::Scope`] extension to `ctx.scope`. The model enforces the scope on every
//! operation; see [`crate::core::Context::scope`].
//!
//! Hidden columns (`Model::hidden_columns`) are stripped from client writes, and filters,
//! sorts and projections naming them are `422 "Invalid filter Inputs"`. On a versioned
//! model (`Model::version_column`) single-record responses carry `ETag: "<version>"`, and
//! `If-Match` supplies the version for `PUT`/`PATCH`/`DELETE /{id}`.

// Handlers short-circuit with a ready `Response` as the error (cold path, not boxed).
#![allow(clippy::result_large_err)]

use crate::core::error::AppError;
use crate::core::hooks::{Context, CurrentUser, Scope};
use crate::core::model::Model;
use crate::core::query::Query;
use crate::core::types::{json_to_row, row_to_json, RowMap, WriteSummary};
use crate::core::{data_to_filter, json_safe_parse, CONFLICT_MESSAGE};
use crate::http::formatter::{format_csv, format_xml};
use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::MethodRouter;
use axum::{Json, Router};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::Arc;

/// Maximum request body accepted by the router.
pub const BODY_LIMIT: usize = 16 * 1024 * 1024;
/// Path parameter name used internally for the record id (avoids clashing with
/// parameters of an enclosing nested router).
const ID_PARAM: &str = "__dbmr_id";

pub struct ModelRouterState {
    pub model: Arc<Model>,
    pub options: RouterOptions,
}

/// Which operations a model router serves. A disabled operation answers
/// `405 {"message":"Method Not Allowed","type":"danger"}`; `QUERY` counts as `list` / `get`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)]
pub struct RouterOptions {
    /// `GET/QUERY /`
    pub list: bool,
    /// `GET/QUERY /{id}`
    pub get: bool,
    /// `POST /` with one object, and `POST /{id}` (single record)
    pub create: bool,
    /// `POST /` with `{data:[...]}`
    pub bulk_create: bool,
    /// `PUT /{id}` (single record)
    pub update: bool,
    /// `PUT /` with `{data:[...]}`
    pub bulk_update: bool,
    /// `PATCH /{id}`
    pub patch: bool,
    /// `DELETE /{id}`
    pub delete: bool,
    /// `DELETE /`
    pub bulk_delete: bool,
    /// `output_content_type=csv|xml`; when off those are `400 "Export is disabled"`.
    pub export: bool,
}

impl Default for RouterOptions {
    /// Everything enabled.
    fn default() -> Self {
        Self {
            list: true,
            get: true,
            create: true,
            bulk_create: true,
            update: true,
            bulk_update: true,
            patch: true,
            delete: true,
            bulk_delete: true,
            export: true,
        }
    }
}

impl RouterOptions {
    /// `list`, `get` and `export` only.
    #[must_use]
    pub fn read_only() -> Self {
        Self {
            list: true,
            get: true,
            export: true,
            create: false,
            bulk_create: false,
            update: false,
            bulk_update: false,
            patch: false,
            delete: false,
            bulk_delete: false,
        }
    }
}

/// Build the router for `model` with every operation enabled. Mount it with
/// `Router::nest`. Row access follows the [`Scope`] request extension (set it in your
/// auth middleware); without one, every row is reachable.
pub fn create_model_router(model: impl Into<Arc<Model>>) -> Router {
    create_model_router_with(model, RouterOptions::default())
}

/// [`create_model_router`] serving only the operations enabled in `options`.
pub fn create_model_router_with(model: impl Into<Arc<Model>>, options: RouterOptions) -> Router {
    let state = Arc::new(ModelRouterState {
        model: model.into(),
        options,
    });
    Router::new()
        .route("/", MethodRouter::new().fallback(collection))
        .route(
            &format!("/{{{ID_PARAM}}}"),
            MethodRouter::new().fallback(item),
        )
        .with_state(state)
}

// ------------------------------------------------------------------ responses

/// `{type:"danger", message}`; 5xx messages are hidden unless `NODE_ENV` is
/// `development`, `test` or `TEST` (§20.3).
#[must_use]
pub fn error_response(err: &AppError) -> Response {
    let code = err.status_code();
    let status = StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let message = if (400..500).contains(&code) {
        if err.message().is_empty() {
            "Bad request"
        } else {
            err.message()
        }
        .to_string()
    } else if matches!(
        std::env::var("NODE_ENV").as_deref(),
        Ok("development" | "test" | "TEST")
    ) && !err.message().is_empty()
    {
        err.message().to_string()
    } else {
        "Internal server error".to_string()
    };
    (
        status,
        Json(json!({ "type": "danger", "message": message })),
    )
        .into_response()
}

fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "message": "Not Found", "type": "danger" })),
    )
        .into_response()
}

fn method_not_allowed() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({ "message": "Method Not Allowed", "type": "danger" })),
    )
        .into_response()
}

fn invalid_filter() -> Response {
    error_response(&AppError::UnprocessableEntity(
        "Invalid filter Inputs".into(),
    ))
}

fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "type": "danger", "message": message })),
    )
        .into_response()
}

fn ok_json(v: Value) -> Response {
    (StatusCode::OK, Json(v)).into_response()
}

/// `ETag: "<version>"` when the model is versioned and `rec` carries the version.
fn with_etag(mut resp: Response, m: &Model, rec: &Value) -> Response {
    let tag = m
        .version_column()
        .and_then(|vc| rec.get(vc))
        .and_then(|v| match v {
            Value::Number(n) => Some(n.to_string()),
            Value::String(s) => Some(s.clone()),
            _ => None,
        })
        .and_then(|v| HeaderValue::from_str(&format!("\"{v}\"")).ok());
    if let Some(tag) = tag {
        resp.headers_mut().insert(header::ETAG, tag);
    }
    resp
}

fn record_json(m: &Model, rec: Option<RowMap>) -> Response {
    let v = rec.map_or(Value::Null, |r| row_to_json(&r));
    with_etag(ok_json(v.clone()), m, &v)
}

fn summary_json(s: &WriteSummary) -> Response {
    let mut m = Map::new();
    m.insert("rows".into(), json!(s.rows));
    m.insert("message".into(), json!(s.message));
    m.insert("type".into(), json!("success"));
    if let Some(id) = &s.id {
        m.insert("id".into(), id.to_json());
    }
    ok_json(Value::Object(m))
}

/// JSON, or CSV / XML when `output_content_type` asks for it (GET-family only).
fn formatted(data: Value, output: Option<&str>) -> Response {
    let rows = || match &data {
        Value::Array(a) => a.clone(),
        Value::Object(o) => match o.get("data") {
            Some(Value::Array(a)) => a.clone(),
            _ => vec![data.clone()],
        },
        other => vec![other.clone()],
    };
    match output {
        Some("csv") => ([(header::CONTENT_TYPE, "text/csv")], format_csv(&rows())).into_response(),
        Some("xml") => (
            [(header::CONTENT_TYPE, "application/xml")],
            format_xml(&rows()),
        )
            .into_response(),
        _ => ok_json(data),
    }
}

// ------------------------------------------------------------------ request

/// The request as seen by hooks (Node `req`).
struct Req {
    method: Method,
    id: Option<String>,
    params: Map<String, Value>,
    query: Map<String, Value>,
    body: Value,
    /// `If-Match` version (quotes and `W/` stripped; `*` ignored).
    if_match: Option<Value>,
    ctx: Context,
}

async fn read_request(params: HashMap<String, String>, req: Request) -> Result<Req, Response> {
    let (parts, body) = req.into_parts();
    let mut id = None;
    let mut path_params = Map::new();
    for (k, v) in params {
        if k == ID_PARAM {
            path_params.insert("id".into(), Value::String(v.clone()));
            id = Some(v);
        } else {
            path_params.insert(k, Value::String(v));
        }
    }

    let pairs: Vec<(String, String)> = axum::extract::Query::try_from_uri(&parts.uri)
        .map(|q| q.0)
        .unwrap_or_default();
    let mut query = Map::new();
    for (k, v) in pairs {
        match query.get_mut(&k) {
            Some(Value::Array(a)) => a.push(Value::String(v)),
            Some(prev) => *prev = Value::Array(vec![prev.take(), Value::String(v)]),
            None => {
                query.insert(k, Value::String(v));
            }
        }
    }

    let if_match = parts
        .headers
        .get(header::IF_MATCH)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.trim()
                .trim_start_matches("W/")
                .trim_matches('"')
                .to_string()
        })
        .filter(|v| !v.is_empty() && v != "*")
        .map(|v| v.parse::<i64>().map_or(Value::String(v), Value::from));

    let headers: Map<String, Value> = parts
        .headers
        .iter()
        .filter_map(|(k, v)| {
            Some((
                k.as_str().to_string(),
                Value::String(v.to_str().ok()?.to_string()),
            ))
        })
        .collect();

    let bytes = axum::body::to_bytes(body, BODY_LIMIT)
        .await
        .map_err(|_| bad_request("Request body too large"))?;
    let body = if bytes.iter().all(u8::is_ascii_whitespace) {
        Value::Object(Map::new())
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            // QUERY: malformed JSON is ignored (§20.1).
            Err(_) if parts.method.as_str() == "QUERY" => Value::Object(Map::new()),
            Err(_) => return Err(bad_request("Malformed JSON body")),
        }
    };

    let user = parts
        .extensions
        .get::<CurrentUser>()
        .map_or(Value::Null, |u| u.0.clone());
    let scope = parts
        .extensions
        .get::<Scope>()
        .map(|s| s.0.clone())
        .unwrap_or_default();
    let json = json!({
        "method": parts.method.as_str(),
        "path": parts.uri.path(),
        "params": path_params,
        "query": query,
        "headers": headers,
        "body": body,
        "user": user,
    });
    Ok(Req {
        method: parts.method,
        id,
        params: path_params,
        query,
        body,
        if_match,
        ctx: Context {
            user,
            request: json,
            scope,
            system: false,
        },
    })
}

impl Req {
    fn body_map(&self) -> Map<String, Value> {
        match &self.body {
            Value::Object(m) => m.clone(),
            _ => Map::new(),
        }
    }

    /// Client write body without hidden columns, plus the `If-Match` version when the
    /// body does not carry one.
    fn write_body(&self, m: &Model) -> Value {
        let mut body = self.body.clone();
        strip_hidden(&mut body, m.hidden_columns());
        if let (Some(vc), Some(v), Value::Object(o)) =
            (m.version_column(), &self.if_match, &mut body)
        {
            if o.get(vc).is_none_or(Value::is_null) {
                o.insert(vc.to_string(), v.clone());
            }
        }
        body
    }
}

fn merge(parts: &[&Map<String, Value>]) -> Value {
    let mut out = Map::new();
    for p in parts {
        out.extend(p.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    Value::Object(out)
}

fn rows_of(v: &Value) -> Vec<RowMap> {
    v.as_array()
        .map(|a| a.iter().map(json_to_row).collect())
        .unwrap_or_default()
}

fn data_array(body: &Value) -> Option<&Value> {
    body.get("data").filter(|d| d.is_array())
}

/// Remove hidden columns from an object, each array item, and each `data` item.
fn strip_hidden(v: &mut Value, hidden: &[String]) {
    match v {
        Value::Array(a) => a.iter_mut().for_each(|x| strip_hidden(x, hidden)),
        Value::Object(o) => {
            for c in hidden {
                o.remove(c);
            }
            if let Some(d) = o.get_mut("data").filter(|d| d.is_array()) {
                strip_hidden(d, hidden);
            }
        }
        _ => {}
    }
}

/// True when a client column reference names a hidden column.
fn touches_hidden(st: &ModelRouterState, cols: impl IntoIterator<Item = String>) -> bool {
    let hidden = st.model.hidden_columns();
    cols.into_iter().any(|c| hidden.contains(&c))
}

/// Parse a read payload; hidden columns are `422`, a disabled export is `400`.
fn read_query(st: &ModelRouterState, payload: &Value) -> Result<(Query, Option<String>), Response> {
    let (q, output) = Query::from_json(payload, st.model.pk()).map_err(|e| error_response(&e))?;
    let cols = q
        .filter
        .groups
        .iter()
        .flatten()
        .map(|c| c.column.to_string())
        .chain(q.sort.iter().map(|s| s.column.to_string()))
        .chain(q.select_columns.iter().flatten().cloned());
    if touches_hidden(st, cols) {
        return Err(invalid_filter());
    }
    if !st.options.export && matches!(output.as_deref(), Some("csv" | "xml")) {
        return Err(bad_request("Export is disabled"));
    }
    Ok((q, output))
}

/// Delete criteria must not filter on hidden columns either (row counts would leak them).
fn check_delete_criteria(st: &ModelRouterState, criteria: &Value) -> Result<(), Response> {
    if let Ok(f) = data_to_filter(&json_safe_parse(criteria), st.model.pk()) {
        if touches_hidden(st, f.groups.iter().flatten().map(|c| c.column.to_string())) {
            return Err(invalid_filter());
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ handlers

async fn collection(
    State(st): State<Arc<ModelRouterState>>,
    params: Option<Path<HashMap<String, String>>>,
    req: Request,
) -> Response {
    collection_inner(&st, params.map(|p| p.0).unwrap_or_default(), req)
        .await
        .unwrap_or_else(|r| r)
}

async fn collection_inner(
    st: &ModelRouterState,
    params: HashMap<String, String>,
    req: Request,
) -> Result<Response, Response> {
    let req = read_request(params, req).await?;
    let (m, ctx, opts) = (&st.model, &req.ctx, &st.options);
    let res = match req.method.as_str() {
        "GET" | "QUERY" => {
            if !opts.list {
                return Err(method_not_allowed());
            }
            let input = if req.method == Method::GET {
                merge(&[&req.query, &req.params])
            } else {
                merge(&[&req.query, &req.body_map()])
            };
            let (q, output) = read_query(st, &input)?;
            m.list(q, ctx).await.map(|r| {
                let data: Vec<Value> = r.data.iter().map(row_to_json).collect();
                formatted(json!({ "data": data, "count": r.count }), output.as_deref())
            })
        }
        "POST" | "PUT" => {
            let post = req.method == Method::POST;
            let mut body = req.body.clone();
            strip_hidden(&mut body, m.hidden_columns());
            let single = post && body.is_object() && data_array(&body).is_none();
            let allowed = match (post, single) {
                (true, true) => opts.create,
                (true, false) => opts.bulk_create,
                _ => opts.bulk_update,
            };
            if !allowed {
                return Err(method_not_allowed());
            }
            if single {
                let mut payload = body;
                if let Value::Object(o) = &mut payload {
                    o.remove(m.pk());
                }
                return Ok(m
                    .insert(json_to_row(&payload), ctx)
                    .await
                    .map_or_else(|e| error_response(&e), |r| record_json(m, r)));
            }
            let Some(data) = data_array(&body) else {
                return Err(bad_request("Request body must contain a 'data' array"));
            };
            let rows = rows_of(data);
            let r = if post {
                m.insert_many(rows, ctx).await
            } else {
                m.update_many(rows, ctx).await
            };
            r.map(|s| summary_json(&s))
        }
        "DELETE" => {
            if !opts.bulk_delete {
                return Err(method_not_allowed());
            }
            if req.body.as_object().is_none_or(Map::is_empty) {
                return Err(bad_request("Request body must contain filter criteria"));
            }
            let payload = data_array(&req.body).unwrap_or(&req.body).clone();
            check_delete_criteria(st, &payload)?;
            m.remove(payload, ctx).await.map(|s| summary_json(&s))
        }
        _ => return Err(method_not_allowed()),
    };
    Ok(res.unwrap_or_else(|e| error_response(&e)))
}

async fn item(
    State(st): State<Arc<ModelRouterState>>,
    params: Option<Path<HashMap<String, String>>>,
    req: Request,
) -> Response {
    item_inner(&st, params.map(|p| p.0).unwrap_or_default(), req)
        .await
        .unwrap_or_else(|r| r)
}

async fn item_inner(
    st: &ModelRouterState,
    params: HashMap<String, String>,
    req: Request,
) -> Result<Response, Response> {
    let req = read_request(params, req).await?;
    let (m, ctx, opts) = (&st.model, &req.ctx, &st.options);
    if matches!(req.method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE")
        && data_array(&req.body).is_some()
    {
        return Err(bad_request(
            "Bulk writes ({\"data\": [...]}) use the collection route",
        ));
    }
    let id = Value::String(req.id.clone().unwrap_or_default());
    let pk = m.pk().to_string();
    let with_pk = |mut v: Value| {
        if let Value::Object(o) = &mut v {
            o.insert(pk.clone(), id.clone());
        }
        v
    };

    let res = match req.method.as_str() {
        "GET" | "QUERY" => {
            if !opts.get {
                return Err(method_not_allowed());
            }
            let input = if req.method == Method::GET {
                merge(&[&req.query, &req.params])
            } else {
                merge(&[&req.query, &req.body_map(), &req.params])
            };
            let (q, output) = read_query(st, &with_pk(input))?;
            m.find(q, ctx)
                .await
                .map(|r| match r.data.into_iter().next() {
                    Some(rec) if r.count > 0 => {
                        let v = row_to_json(&rec);
                        with_etag(formatted(v.clone(), output.as_deref()), m, &v)
                    }
                    _ => not_found(),
                })
        }
        "POST" => {
            if !opts.create {
                return Err(method_not_allowed());
            }
            let mut payload = req.write_body(m);
            if let Value::Object(o) = &mut payload {
                o.remove(m.pk());
            }
            m.insert(json_to_row(&payload), ctx)
                .await
                .map(|r| record_json(m, r))
        }
        "PUT" | "PATCH" | "DELETE" => {
            let body = req.write_body(m);
            let allowed = match req.method.as_str() {
                "PUT" => opts.update,
                "PATCH" => opts.patch,
                _ => opts.delete,
            };
            if !allowed {
                return Err(method_not_allowed());
            }
            // Existence pre-check inside the scope (§20.2).
            let check = with_pk(Value::Object(Map::new()));
            let exists = || async {
                let (q, _) = Query::from_json(&check, m.pk()).map_err(|e| error_response(&e))?;
                m.find_one(q, ctx)
                    .await
                    .map(|r| r.is_some())
                    .map_err(|e| error_response(&e))
            };
            if !exists().await? {
                return Err(not_found());
            }
            let payload = with_pk(body);
            match req.method.as_str() {
                "PUT" => m
                    .update(json_to_row(&payload), ctx)
                    .await
                    .map(|r| record_json(m, r)),
                "PATCH" => m
                    .patch(json_to_row(&payload), ctx)
                    .await
                    .map(|r| record_json(m, r)),
                _ => {
                    check_delete_criteria(st, &payload)?;
                    let versioned = m
                        .version_column()
                        .is_some_and(|vc| payload.get(vc).is_some_and(|v| !v.is_null()));
                    match m.remove(payload, ctx).await {
                        // The version guard matched nothing, but the row is still there.
                        Ok(s) if s.rows == 0 && versioned && exists().await? => {
                            Err(AppError::Conflict(CONFLICT_MESSAGE.into()))
                        }
                        r => r.map(|s| summary_json(&s)),
                    }
                }
            }
        }
        _ => return Err(method_not_allowed()),
    };
    Ok(res.unwrap_or_else(|e| error_response(&e)))
}
