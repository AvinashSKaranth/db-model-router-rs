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
//! | `OPTIONS /{id}` | allowed item methods + `Allow` header |
//! | `GET/QUERY /` | `list` → `{data, count}` |
//! | `OPTIONS /` | allowed collection methods + `Allow` header |
//! | `DISCOVER /` | `filters` → column discovery and enum values based on cardinality |
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
use crate::core::filter::{FilterExpr, Operator};
use crate::core::hooks::{Context, CurrentUser, Scope};
use crate::core::model::Model;
use crate::core::query::Query;
use crate::core::types::{json_to_row, row_to_json, DbValue, RowMap, WriteSummary};
use crate::core::{data_to_filter, CONFLICT_MESSAGE};
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

/// Maximum distinct values for a column to be treated as a fixed enum.
pub const CARDINALITY_FIXED_LIMIT: usize = 50;
/// Probing limit to check if cardinality exceeds CARDINALITY_FIXED_LIMIT.
pub const CARDINALITY_PROBE_LIMIT: usize = CARDINALITY_FIXED_LIMIT + 1;
/// Maximum number of columns in a model to automatically pre-warm on application start.
pub const DEFAULT_PREWARM_COLUMNS_LIMIT: usize = 100;

#[derive(Debug, Clone)]
pub struct CachedColumn {
    pub column: String,
    pub col_type: String,
    pub is_fixed: bool,
    pub cardinality: Value,
    pub values: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct DiscoveryCache {
    pub table: String,
    pub columns: Vec<CachedColumn>,
    pub full_response: Value,
    pub created_at: std::time::Instant,
}

pub struct ModelRouterState {
    pub model: Arc<Model>,
    pub options: RouterOptions,
    pub discovery_cache: tokio::sync::RwLock<Option<DiscoveryCache>>,
    warm_lock: tokio::sync::Mutex<()>,
}

/// Build discovery metadata and cache entry from a probed column's distinct values.
fn build_column_discovery(col: &str, col_type: &str, probe: Vec<DbValue>) -> (CachedColumn, Value) {
    let is_fixed = probe.len() <= CARDINALITY_FIXED_LIMIT;
    let mode = if is_fixed { "fixed" } else { "searchable" };
    let cardinality_val = if is_fixed {
        json!(probe.len())
    } else {
        json!(">50")
    };
    let values: Vec<Value> = if is_fixed {
        probe.into_iter().map(|v| v.to_json()).collect()
    } else {
        vec![]
    };

    let mut meta = json!({
        "column": col,
        "type": col_type,
        "filterable": true,
        "cardinality": cardinality_val,
        "mode": mode,
        "values": values
    });
    if !is_fixed {
        meta["min_query_length"] = json!(3);
    }

    let cached = CachedColumn {
        column: col.to_string(),
        col_type: col_type.to_string(),
        is_fixed,
        cardinality: cardinality_val,
        values,
    };
    (cached, meta)
}

fn encrypted_column_meta(col: &str, col_type: &str) -> Value {
    json!({
        "column": col,
        "type": col_type,
        "filterable": false,
        "cardinality": Value::Null,
        "mode": "unsupported",
        "values": []
    })
}

fn column_type(model: &Model, col: &str) -> String {
    model
        .structure()
        .get(col)
        .cloned()
        .unwrap_or_else(|| "string".into())
}

/// Collect visible (non-hidden, non-encrypted) columns including an implicit PK if not in `structure`.
fn discoverable_columns(model: &Model, hidden_lower: &[String]) -> Vec<(String, String)> {
    let mut cols: Vec<(String, String)> = model
        .structure()
        .iter()
        .filter(|(col, _)| {
            !hidden_lower.contains(&col.trim().to_lowercase()) && !model.is_encrypted_column(col)
        })
        .map(|(c, t)| (c.clone(), t.clone()))
        .collect();
    let pk = model.pk();
    if !model.structure().contains_key(pk)
        && !hidden_lower.contains(&pk.trim().to_lowercase())
        && !model.is_encrypted_column(pk)
    {
        cols.insert(0, (pk.to_string(), column_type(model, pk)));
    }
    cols
}

impl ModelRouterState {
    /// Construct a new router state.
    pub fn new(model: Arc<Model>, options: RouterOptions) -> Self {
        Self {
            model,
            options,
            discovery_cache: tokio::sync::RwLock::new(None),
            warm_lock: tokio::sync::Mutex::new(()),
        }
    }

    /// Invalidate the in-memory discovery cache.
    pub async fn invalidate_discovery_cache(&self) {
        let mut cache = self.discovery_cache.write().await;
        *cache = None;
    }

    /// Pre-warm or refresh the in-memory discovery cache.
    pub async fn warm_discovery_cache(&self, ctx: &Context) -> Result<(), AppError> {
        let model = &self.model;
        if !model.options().require_scope.is_empty() || model.has_before_find() {
            return Ok(());
        }

        let hidden_lower: Vec<String> = model
            .hidden_columns()
            .iter()
            .map(|h| h.trim().to_lowercase())
            .collect();

        let visible_cols = discoverable_columns(model, &hidden_lower);
        let mut cached_cols = Vec::with_capacity(visible_cols.len());
        let mut col_metas = Vec::with_capacity(model.structure().len() + 1);

        for (col, col_type) in visible_cols {
            let probe = model
                .distinct_values(&col, None, CARDINALITY_PROBE_LIMIT, ctx)
                .await?;
            let (cached, meta) = build_column_discovery(&col, &col_type, probe);
            col_metas.push(meta);
            cached_cols.push(cached);
        }

        for (col, col_type) in model.structure() {
            if !hidden_lower.contains(&col.trim().to_lowercase()) && model.is_encrypted_column(col)
            {
                col_metas.push(encrypted_column_meta(col, col_type));
            }
        }

        let full_resp = json!({
            "table": model.table_name(),
            "columns": col_metas
        });

        let mut cache = self.discovery_cache.write().await;
        *cache = Some(DiscoveryCache {
            table: model.table_name().to_string(),
            columns: cached_cols,
            full_response: full_resp,
            created_at: std::time::Instant::now(),
        });

        Ok(())
    }
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
    /// `DISCOVER /` column discovery and filter values.
    pub filters: bool,
    /// TTL for in-memory discovery cache. Default 5 minutes (300s). None for no expiry.
    pub discovery_cache_ttl: Option<std::time::Duration>,
    /// Maximum number of columns to pre-warm on start (default 100).
    pub prewarm_columns_limit: usize,
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
            filters: true,
            discovery_cache_ttl: Some(std::time::Duration::from_secs(300)),
            prewarm_columns_limit: DEFAULT_PREWARM_COLUMNS_LIMIT,
        }
    }
}

impl RouterOptions {
    /// `list`, `get`, `export` and `filters` only.
    #[must_use]
    pub const fn read_only() -> Self {
        Self {
            list: true,
            get: true,
            export: true,
            filters: true,
            create: false,
            bulk_create: false,
            update: false,
            bulk_update: false,
            patch: false,
            delete: false,
            bulk_delete: false,
            discovery_cache_ttl: Some(std::time::Duration::from_secs(300)),
            prewarm_columns_limit: DEFAULT_PREWARM_COLUMNS_LIMIT,
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
    let state = Arc::new(ModelRouterState::new(
        Arc::new(model.into().public_view()),
        options,
    ));

    if state.options.filters
        && state.model.options().require_scope.is_empty()
        && !state.model.has_before_find()
        && state.model.structure().len() <= state.options.prewarm_columns_limit
    {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let st = Arc::clone(&state);
            handle.spawn(async move {
                let _ = st.warm_discovery_cache(&Context::system()).await;
            });
        }
    }

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

pub fn allowed_collection_methods(opts: &RouterOptions) -> Vec<&'static str> {
    let mut methods = Vec::new();
    methods.push("OPTIONS");
    if opts.list {
        methods.push("GET");
        methods.push("QUERY");
    }
    if opts.create || opts.bulk_create {
        methods.push("POST");
    }
    if opts.bulk_update {
        methods.push("PUT");
    }
    if opts.bulk_delete {
        methods.push("DELETE");
    }
    if opts.filters {
        methods.push("DISCOVER");
    }
    methods
}

pub fn allowed_item_methods(opts: &RouterOptions) -> Vec<&'static str> {
    let mut methods = Vec::new();
    methods.push("OPTIONS");
    if opts.get {
        methods.push("GET");
        methods.push("QUERY");
    }
    if opts.create {
        methods.push("POST");
    }
    if opts.update {
        methods.push("PUT");
    }
    if opts.patch {
        methods.push("PATCH");
    }
    if opts.delete {
        methods.push("DELETE");
    }
    methods
}

fn method_not_allowed(allow: &[&str]) -> Response {
    let mut resp = (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({ "message": "Method Not Allowed", "type": "danger" })),
    )
        .into_response();
    if !allow.is_empty() {
        let allow_str = allow.join(", ");
        if let Ok(val) = HeaderValue::try_from(allow_str) {
            resp.headers_mut().insert(header::ALLOW, val);
        }
    }
    resp
}

fn options_response(resource: &str, allow: &[&str]) -> Response {
    let allow_str = allow.join(", ");
    let mut resp = (
        StatusCode::OK,
        Json(json!({
            "resource": resource,
            "methods": allow
        })),
    )
        .into_response();
    if let Ok(val) = HeaderValue::try_from(allow_str) {
        resp.headers_mut().insert(header::ALLOW, val);
    }
    resp
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
        .filter(|(name, _)| {
            !matches!(
                name.as_str(),
                "authorization" | "proxy-authorization" | "cookie" | "x-api-key"
            )
        })
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
    let mut hook_params = path_params.clone();
    if let Some(id) = &id {
        hook_params.insert("id".into(), Value::String(id.clone()));
    }
    let json = json!({
        "method": parts.method.as_str(),
        "path": parts.uri.path(),
        "params": hook_params,
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
    let hidden: Vec<String> = hidden.iter().map(|s| s.trim().to_lowercase()).collect();
    strip_hidden_normalized(v, &hidden);
}

fn strip_hidden_normalized(v: &mut Value, hidden: &[String]) {
    match v {
        Value::Array(a) => a
            .iter_mut()
            .for_each(|x| strip_hidden_normalized(x, hidden)),
        Value::Object(o) => {
            o.retain(|k, _| !hidden.contains(&k.trim().to_lowercase()));
            if let Some(d) = o.get_mut("data").filter(|d| d.is_array()) {
                strip_hidden_normalized(d, hidden);
            }
        }
        _ => {}
    }
}

/// Accept declared column names, plus the implicit primary key. Reject aliases before
/// the database can resolve them to a different column.
fn check_client_columns<'a>(
    st: &ModelRouterState,
    cols: impl IntoIterator<Item = &'a str>,
) -> Result<(), Response> {
    let model = &st.model;
    let hidden: Vec<String> = model
        .hidden_columns()
        .iter()
        .map(|s| s.trim().to_lowercase())
        .collect();
    if cols.into_iter().any(|col| {
        col.contains('\0')
            || (col != model.pk() && !model.structure().contains_key(col))
            || hidden.contains(&col.trim().to_lowercase())
    }) {
        return Err(invalid_filter());
    }
    Ok(())
}

/// Parse a read payload; hidden columns are `422`, a disabled export is `400`.
fn read_query(st: &ModelRouterState, payload: &Value) -> Result<(Query, Option<String>), Response> {
    let (q, output) = Query::from_json(payload, st.model.pk()).map_err(|e| error_response(&e))?;
    let cols = q
        .filter
        .groups
        .iter()
        .flatten()
        .map(|c| c.column.as_str())
        .chain(q.sort.iter().map(|s| s.column.as_str()))
        .chain(q.select_columns.iter().flatten().map(String::as_str));
    check_client_columns(st, cols)?;
    if q.search.as_deref().is_some_and(|s| !s.is_empty()) {
        check_client_columns(
            st,
            st.model.options().search_columns.iter().map(String::as_str),
        )?;
    }
    if !st.options.export && matches!(output.as_deref(), Some("csv" | "xml")) {
        return Err(bad_request("Export is disabled"));
    }
    Ok((q, output))
}

/// Delete criteria must not filter on hidden columns either (row counts would leak them).
fn check_delete_criteria(st: &ModelRouterState, criteria: &Value) -> Result<(), Response> {
    let f = data_to_filter(criteria, st.model.pk()).map_err(|e| error_response(&e))?;
    check_client_columns(st, f.groups.iter().flatten().map(|c| c.column.as_str()))
}

// ------------------------------------------------------------------ handlers

async fn filters_inner(st: &ModelRouterState, req: &Req) -> Result<Response, Response> {
    let model = &st.model;

    // Enforce required scope and evaluate row-level scope authorization upfront
    let _ = model.scope(&req.ctx).map_err(|e| error_response(&e))?;

    let hidden_lower: Vec<String> = model
        .hidden_columns()
        .iter()
        .map(|h| h.trim().to_lowercase())
        .collect();

    // 1. Determine if a specific column is requested
    let body_map = req.body.as_object();

    let mut target_col = req
        .query
        .get("column")
        .and_then(|v| v.as_str())
        .or_else(|| {
            body_map
                .and_then(|b| b.get("column"))
                .and_then(|v| v.as_str())
        })
        .map(str::to_string);

    let mut search_val: Option<String> = None;

    // Extract search query if present: ?query=, ?q=, ?search= or JSON body
    if let Some(q) = req
        .query
        .get("query")
        .or_else(|| req.query.get("q"))
        .or_else(|| req.query.get("search"))
        .and_then(|v| v.as_str())
        .or_else(|| {
            body_map.and_then(|b| {
                b.get("query")
                    .or_else(|| b.get("q"))
                    .or_else(|| b.get("search"))
                    .and_then(|v| v.as_str())
            })
        })
    {
        search_val = Some(q.to_string());
    }

    // Also support passing column directly as a query parameter or body key: e.g. ?author=ali or {"author": "ali"}
    let is_known_col = |k: &str| model.structure().contains_key(k) || k == model.pk();
    if target_col.is_none() {
        for (k, v) in &req.query {
            if k != "column" && k != "q" && k != "query" && k != "search" && is_known_col(k) {
                target_col = Some(k.clone());
                if search_val.is_none() {
                    if let Some(s) = v.as_str() {
                        search_val = Some(s.to_string());
                    }
                }
                break;
            }
        }
        if target_col.is_none() {
            if let Some(b) = body_map {
                for (k, v) in b {
                    if k != "column" && k != "q" && k != "query" && k != "search" && is_known_col(k)
                    {
                        target_col = Some(k.clone());
                        if search_val.is_none() {
                            if let Some(s) = v.as_str() {
                                search_val = Some(s.to_string());
                            }
                        }
                        break;
                    }
                }
            }
        }
    }

    // Ensure hidden or encrypted column access is rejected with 422 before checking cache
    if let Some(ref col) = target_col {
        if hidden_lower.contains(&col.trim().to_lowercase()) || model.is_encrypted_column(col) {
            return Err(invalid_filter());
        }
        if !is_known_col(col) {
            return Err(not_found());
        }
    }

    // Carry nested path parameters into filter criteria (e.g. /authors/{author_id}/books)
    let mut path_filter = FilterExpr::empty();
    for (k, v) in &req.params {
        if let Some(col_type) = model.structure().get(k) {
            if let Some(s) = v.as_str() {
                let db_val = coerce_path_param(col_type, s);
                path_filter = path_filter.and(k, Operator::Eq, db_val);
            }
        }
    }
    let has_path_filter = !path_filter.is_empty();
    let extra_filter = if has_path_filter {
        Some(&path_filter)
    } else {
        None
    };

    // Shared cache can only be used if model requires no scope, has no before_find hook,
    // request has no tenant scope, and there are no nested path parameters filtering the table.
    let can_use_shared_cache = model.options().require_scope.is_empty()
        && !model.has_before_find()
        && req.ctx.scope.is_empty()
        && !has_path_filter;

    if can_use_shared_cache {
        let is_cache_fresh = |c: &DiscoveryCache| match st.options.discovery_cache_ttl {
            Some(ttl) => c.created_at.elapsed() < ttl,
            None => true,
        };
        let is_valid = {
            let cache_read = st.discovery_cache.read().await;
            cache_read.as_ref().is_some_and(is_cache_fresh)
        };

        if !is_valid {
            let _guard = st.warm_lock.lock().await;
            let still_invalid = {
                let cache_read = st.discovery_cache.read().await;
                !cache_read.as_ref().is_some_and(is_cache_fresh)
            };
            if still_invalid {
                st.warm_discovery_cache(&req.ctx)
                    .await
                    .map_err(|e| error_response(&e))?;
            }
        }

        let cache_read = st.discovery_cache.read().await;
        if let Some(ref cache) = *cache_read {
            // Case A: Specific column requested
            if let Some(ref col) = target_col {
                if let Some(cached_col) = cache.columns.iter().find(|c| &c.column == col) {
                    if cached_col.is_fixed {
                        // Fixed enum column: completely served from in-memory cache!
                        if let Some(ref query_str) = search_val {
                            let query_lower = query_str.to_lowercase();
                            let filtered: Vec<Value> = cached_col
                                .values
                                .iter()
                                .filter(|v| match v {
                                    Value::String(s) => s.to_lowercase().contains(&query_lower),
                                    other => {
                                        other.to_string().to_lowercase().contains(&query_lower)
                                    }
                                })
                                .cloned()
                                .collect();
                            return Ok(ok_json(json!({
                                "column": cached_col.column,
                                "type": cached_col.col_type,
                                "filterable": true,
                                "cardinality": cached_col.cardinality,
                                "mode": "fixed",
                                "query": query_str,
                                "values": filtered
                            })));
                        }
                        return Ok(ok_json(json!({
                            "column": cached_col.column,
                            "type": cached_col.col_type,
                            "filterable": true,
                            "cardinality": cached_col.cardinality,
                            "mode": "fixed",
                            "values": cached_col.values.clone()
                        })));
                    }

                    // Searchable column
                    if let Some(ref query_str) = search_val {
                        if query_str.chars().count() < 3 {
                            return Ok(ok_json(json!({
                                "column": cached_col.column,
                                "type": cached_col.col_type,
                                "filterable": true,
                                "cardinality": cached_col.cardinality,
                                "mode": "searchable",
                                "query": query_str,
                                "min_query_length": 3,
                                "message": "Minimum 3 characters required to search values",
                                "values": []
                            })));
                        }
                        // >= 3 letters: targeted search on DB with limit 20
                        let matches = model
                            .distinct_values(&cached_col.column, Some(query_str), 20, &req.ctx)
                            .await
                            .map_err(|e| error_response(&e))?;
                        let values: Vec<Value> = matches.into_iter().map(|v| v.to_json()).collect();
                        return Ok(ok_json(json!({
                            "column": cached_col.column,
                            "type": cached_col.col_type,
                            "filterable": true,
                            "cardinality": cached_col.cardinality,
                            "mode": "searchable",
                            "query": query_str,
                            "values": values
                        })));
                    }
                    return Ok(ok_json(json!({
                        "column": cached_col.column,
                        "type": cached_col.col_type,
                        "filterable": true,
                        "cardinality": cached_col.cardinality,
                        "mode": "searchable",
                        "min_query_length": 3,
                        "values": []
                    })));
                }
            } else {
                // Case B: Full table discovery -> serve full JSON from cache!
                return Ok(ok_json(cache.full_response.clone()));
            }
        }
    }

    // Dynamic database query fallback (when scoped, hooked, path-filtered, or cache missed)
    if let Some(col) = target_col {
        let col_type = column_type(model, &col);

        // Probe cardinality with limit CARDINALITY_PROBE_LIMIT
        let probe = model
            .distinct_values_with_filter(
                &col,
                extra_filter,
                None,
                CARDINALITY_PROBE_LIMIT,
                &req.ctx,
            )
            .await
            .map_err(|e| error_response(&e))?;

        let (cached_col, meta) = build_column_discovery(&col, &col_type, probe);

        if let Some(query_str) = search_val {
            if !cached_col.is_fixed {
                if query_str.chars().count() < 3 {
                    return Ok(ok_json(json!({
                        "column": col,
                        "type": col_type,
                        "filterable": true,
                        "cardinality": cached_col.cardinality,
                        "mode": "searchable",
                        "query": query_str,
                        "min_query_length": 3,
                        "message": "Minimum 3 characters required to search values",
                        "values": []
                    })));
                }

                let matches = model
                    .distinct_values_with_filter(&col, extra_filter, Some(&query_str), 20, &req.ctx)
                    .await
                    .map_err(|e| error_response(&e))?;

                let values: Vec<Value> = matches.into_iter().map(|v| v.to_json()).collect();
                return Ok(ok_json(json!({
                    "column": col,
                    "type": col_type,
                    "filterable": true,
                    "cardinality": cached_col.cardinality,
                    "mode": "searchable",
                    "query": query_str,
                    "values": values
                })));
            }

            let query_lower = query_str.to_lowercase();
            let filtered: Vec<Value> = cached_col
                .values
                .into_iter()
                .filter(|v| match v {
                    Value::String(s) => s.to_lowercase().contains(&query_lower),
                    other => other.to_string().to_lowercase().contains(&query_lower),
                })
                .collect();

            return Ok(ok_json(json!({
                "column": col,
                "type": col_type,
                "filterable": true,
                "cardinality": cached_col.cardinality,
                "mode": "fixed",
                "query": query_str,
                "values": filtered
            })));
        }

        return Ok(ok_json(meta));
    }

    let visible_cols = discoverable_columns(model, &hidden_lower);
    let mut col_metas = Vec::with_capacity(model.structure().len() + 1);
    for (col, col_type) in visible_cols {
        let probe = model
            .distinct_values_with_filter(
                &col,
                extra_filter,
                None,
                CARDINALITY_PROBE_LIMIT,
                &req.ctx,
            )
            .await
            .map_err(|e| error_response(&e))?;

        let (_, meta) = build_column_discovery(&col, &col_type, probe);
        col_metas.push(meta);
    }
    for (col, col_type) in model.structure() {
        if !hidden_lower.contains(&col.trim().to_lowercase()) && model.is_encrypted_column(col) {
            col_metas.push(encrypted_column_meta(col, col_type));
        }
    }

    let full_resp = json!({
        "table": model.table_name(),
        "columns": col_metas
    });

    Ok(ok_json(full_resp))
}

fn coerce_path_param(col_type: &str, raw: &str) -> DbValue {
    let lower = col_type.to_ascii_lowercase();
    if lower.contains("int") {
        if let Ok(n) = raw.parse::<i128>() {
            return DbValue::Int(n);
        }
    } else if lower.contains("bool") {
        if let Ok(b) = raw.parse::<bool>() {
            return DbValue::Bool(b);
        }
    }
    DbValue::Text(raw.to_string())
}

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
    let allowed = allowed_collection_methods(opts);
    let res = match req.method.as_str() {
        "OPTIONS" => {
            return Ok(options_response("collection", &allowed));
        }
        "DISCOVER" => {
            if !opts.filters {
                return Err(method_not_allowed(&allowed));
            }
            return filters_inner(st, &req).await;
        }
        "GET" | "QUERY" => {
            if !opts.list {
                return Err(method_not_allowed(&allowed));
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
            let allowed_op = match (post, single) {
                (true, true) => opts.create,
                (true, false) => opts.bulk_create,
                _ => opts.bulk_update,
            };
            if !allowed_op {
                return Err(method_not_allowed(&allowed));
            }
            if single {
                let mut payload = body;
                if let Value::Object(o) = &mut payload {
                    o.remove(m.pk());
                }
                let res = m.insert(json_to_row(&payload), ctx).await;
                if res.is_ok() {
                    st.invalidate_discovery_cache().await;
                }
                return Ok(res.map_or_else(|e| error_response(&e), |r| record_json(m, r)));
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
            if r.is_ok() {
                st.invalidate_discovery_cache().await;
            }
            r.map(|s| summary_json(&s))
        }
        "DELETE" => {
            if !opts.bulk_delete {
                return Err(method_not_allowed(&allowed));
            }
            if req.body.as_object().is_none_or(Map::is_empty) {
                return Err(bad_request("Request body must contain filter criteria"));
            }
            let payload = data_array(&req.body).unwrap_or(&req.body).clone();
            check_delete_criteria(st, &payload)?;
            let r = m.remove(payload, ctx).await;
            if r.is_ok() {
                st.invalidate_discovery_cache().await;
            }
            r.map(|s| summary_json(&s))
        }
        _ => return Err(method_not_allowed(&allowed)),
    };
    Ok(res.unwrap_or_else(|e| error_response(&e)))
}

async fn item(
    State(st): State<Arc<ModelRouterState>>,
    params: Option<Path<HashMap<String, String>>>,
    req: Request,
) -> Response {
    let allowed = allowed_item_methods(&st.options);
    if req.method().as_str() == "DISCOVER" {
        return method_not_allowed(&allowed);
    }
    item_inner(&st, params.map(|p| p.0).unwrap_or_default(), req)
        .await
        .unwrap_or_else(|r| r)
}

async fn item_inner(
    st: &ModelRouterState,
    params: HashMap<String, String>,
    req: Request,
) -> Result<Response, Response> {
    let allowed = allowed_item_methods(&st.options);
    if req.method().as_str() == "DISCOVER" {
        return Err(method_not_allowed(&allowed));
    }
    let req = read_request(params, req).await?;
    let (m, ctx, opts) = (&st.model, &req.ctx, &st.options);

    if matches!(req.method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE")
        && data_array(&req.body).is_some()
    {
        return Err(bad_request(
            "Bulk writes ({\"data\": [...]}) use the collection route",
        ));
    }
    let pk_name = m.pk();
    let item_id = || {
        req.id
            .as_deref()
            .and_then(|id| m.path_id(id))
            .ok_or_else(not_found)
    };

    let res = match req.method.as_str() {
        "OPTIONS" => {
            return Ok(options_response("item", &allowed));
        }
        "GET" | "QUERY" => {
            if !opts.get {
                return Err(method_not_allowed(&allowed));
            }
            let mut input = if req.method == Method::GET {
                merge(&[&req.query, &req.params])
            } else {
                merge(&[&req.query, &req.body_map(), &req.params])
            };
            if let Value::Object(o) = &mut input {
                o.remove(pk_name);
            }
            let (q, output) = read_query(st, &input)?;
            m.find_item(q, item_id()?, ctx)
                .await
                .map(|r| match r.data.into_iter().next() {
                    Some(rec) => {
                        let v = row_to_json(&rec);
                        with_etag(formatted(v.clone(), output.as_deref()), m, &v)
                    }
                    _ => not_found(),
                })
        }
        "POST" => {
            if !opts.create {
                return Err(method_not_allowed(&allowed));
            }
            let mut payload = req.write_body(m);
            if let Value::Object(o) = &mut payload {
                o.remove(m.pk());
            }
            let r = m.insert(json_to_row(&payload), ctx).await;
            if r.is_ok() {
                st.invalidate_discovery_cache().await;
            }
            r.map(|r| record_json(m, r))
        }
        "PUT" | "PATCH" => {
            let body = req.write_body(m);
            let allowed_op = if req.method.as_str() == "PUT" {
                opts.update
            } else {
                opts.patch
            };
            if !allowed_op {
                return Err(method_not_allowed(&allowed));
            }
            // Existence pre-check inside the scope (§20.2).
            let id = item_id()?;
            let found = m
                .find_item(Query::default(), id.clone(), ctx)
                .await
                .map_err(|e| error_response(&e))?;
            if found.data.is_empty() {
                return Err(not_found());
            }
            let mut payload = body;
            if let Value::Object(o) = &mut payload {
                o.insert(pk_name.to_string(), id.to_json());
            }
            let r = if req.method.as_str() == "PUT" {
                m.update(json_to_row(&payload), ctx).await
            } else {
                m.patch(json_to_row(&payload), ctx).await
            };
            if r.is_ok() {
                st.invalidate_discovery_cache().await;
            }
            r.map(|r| record_json(m, r))
        }
        "DELETE" => {
            if !opts.delete {
                return Err(method_not_allowed(&allowed));
            }
            let payload = req.write_body(m);
            check_delete_criteria(st, &payload)?;
            let id = item_id()?;
            let exists = || async {
                m.find_item(Query::default(), id.clone(), ctx)
                    .await
                    .map(|r| !r.data.is_empty())
                    .map_err(|e| error_response(&e))
            };
            if !exists().await? {
                return Err(not_found());
            }
            let mut delete_filter = FilterExpr::eq(pk_name, id.clone());
            let version = m
                .version_column()
                .and_then(|col| payload.get(col).filter(|v| !v.is_null()).map(|v| (col, v)));
            if let Some((col, value)) = version {
                let version = Model::version_value(&DbValue::from(value.clone()))
                    .map_err(|e| error_response(&e))?;
                delete_filter = delete_filter.and(col, Operator::Eq, DbValue::Int(version));
            }
            match m.remove_item(&delete_filter, ctx).await {
                // The version guard matched nothing, but the row is still there.
                Ok(s) if s.rows == 0 && version.is_some() && exists().await? => {
                    Err(AppError::Conflict(CONFLICT_MESSAGE.into()))
                }
                Ok(s) => {
                    st.invalidate_discovery_cache().await;
                    Ok(summary_json(&s))
                }
                Err(e) => Err(e),
            }
        }
        _ => return Err(method_not_allowed(&allowed)),
    };
    Ok(res.unwrap_or_else(|e| error_response(&e)))
}
