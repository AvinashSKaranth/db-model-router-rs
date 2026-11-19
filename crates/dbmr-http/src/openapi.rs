//! OpenAPI 3.1 description of model routers, built from each model's `structure()` rules.
//!
//! ```no_run
//! # fn demo(users: dbmr_core::Model) {
//! use dbmr_http::{create_model_router, openapi, openapi_route, RouterOptions};
//! let opts = RouterOptions::default();
//! let spec = openapi("My API", "1.0.0", &[("/users", &users, &opts)]);
//! let app: axum::Router = axum::Router::new()
//!     .nest("/users", create_model_router(users))
//!     .nest("/openapi.json", openapi_route(spec));
//! # }
//! ```
//!
//! `QUERY` routes are not described (OpenAPI 3.1 has no `QUERY` operation).

// The helpers hand owned JSON fragments to `json!`, which only borrows them.
#![allow(clippy::needless_pass_by_value)]

use crate::router::RouterOptions;
use axum::routing::get;
use axum::{Json, Router};
use dbmr_core::Model;
use serde_json::{json, Map, Value};
use std::sync::Arc;

/// Build the OpenAPI 3.1 document for `resources` = `(mount path, model, options)`.
/// Only operations enabled in the options get a path entry.
#[must_use]
pub fn openapi(title: &str, version: &str, resources: &[(&str, &Model, &RouterOptions)]) -> Value {
    let mut paths = Map::new();
    let mut schemas = Map::new();
    schemas.insert(
        "Error".into(),
        json!({
            "type": "object",
            "required": ["message", "type"],
            "properties": {
                "message": {"type": "string"},
                "type": {"type": "string", "const": "danger"}
            }
        }),
    );
    schemas.insert(
        "BulkSummary".into(),
        json!({
            "type": "object",
            "required": ["rows", "message", "type"],
            "properties": {
                "rows": {"type": "integer"},
                "message": {"type": "string"},
                "type": {"type": "string", "const": "success"},
                "id": {}
            }
        }),
    );
    for (mount, model, opts) in resources {
        let name = model.table_name().to_string();
        let (record, create) = schemas_for(model);
        schemas.insert(name.clone(), record);
        schemas.insert(format!("{name}Create"), create);
        let base = format!("/{}", mount.trim_matches('/'));
        let collection = collection_ops(&name, model, opts, mount_params(mount));
        if !collection.is_empty() {
            paths.insert(base.clone(), Value::Object(collection));
        }
        let mut item_params = mount_params(mount);
        item_params.push(path_param("id"));
        let item = item_ops(&name, model, opts, item_params);
        if !item.is_empty() {
            let p = format!("{}/{{id}}", base.trim_end_matches('/'));
            paths.insert(p, Value::Object(item));
        }
    }
    json!({
        "openapi": "3.1.0",
        "info": {"title": title, "version": version},
        "paths": paths,
        "components": {"schemas": schemas}
    })
}

/// `GET /` serving `spec` as JSON. Mount it with `Router::nest`.
pub fn openapi_route(spec: Value) -> Router {
    let spec = Arc::new(spec);
    Router::new().route(
        "/",
        get(move || {
            let spec = Arc::clone(&spec);
            async move { Json((*spec).clone()) }
        }),
    )
}

// ------------------------------------------------------------------ schemas

/// `(record schema, create schema)`; hidden columns and the soft-delete flag are left out.
fn schemas_for(model: &Model) -> (Value, Value) {
    let opts = model.options();
    let mut props = Map::new();
    let mut create_props = Map::new();
    let mut required = Vec::new();
    for (col, rules) in model.structure() {
        if model.hidden_columns().contains(col) || opts.safe_delete.as_deref() == Some(col) {
            continue;
        }
        let (mut schema, req) = column_schema(rules);
        let server_managed = col == model.pk()
            || model.version_column() == Some(col.as_str())
            || opts.auto_created_at.as_deref() == Some(col)
            || opts.auto_modified_at.as_deref() == Some(col);
        if model.version_column() == Some(col.as_str()) {
            schema.insert(
                "description".into(),
                json!("Optimistic-lock version. Send it back on update (or use If-Match); set by the server on insert."),
            );
        }
        if !server_managed {
            create_props.insert(col.clone(), Value::Object(schema.clone()));
            if req {
                required.push(json!(col));
            }
        }
        props.insert(col.clone(), Value::Object(schema));
    }
    (
        json!({"type": "object", "properties": props}),
        json!({"type": "object", "properties": create_props, "required": required}),
    )
}

/// JSON schema of one column rule string (`"required|string|maxLength:20"`) and whether
/// it is `required`.
fn column_schema(rules: &str) -> (Map<String, Value>, bool) {
    let mut s = Map::new();
    let (mut required, mut nullable) = (false, false);
    let mut ty = None;
    for token in rules.split('|') {
        let (name, arg) = token.split_once(':').unwrap_or((token, ""));
        let num = || arg.parse::<f64>().ok().map(Value::from);
        let int = || arg.parse::<u64>().ok().map(Value::from);
        match name {
            "required" => required = true,
            "nullable" => nullable = true,
            "string" => ty = Some("string"),
            "integer" | "auto_increment" => ty = Some("integer"),
            "numeric" => ty = Some("number"),
            "boolean" => ty = Some("boolean"),
            "object" => ty = Some("object"),
            "array" => ty = Some("array"),
            "decimal" | "datetime" | "email" => {
                ty = Some("string");
                let format = match name {
                    "decimal" => "decimal",
                    "datetime" => "date-time",
                    _ => "email",
                };
                s.insert("format".into(), json!(format));
            }
            "maxLength" | "minLength" => {
                if let Some(v) = int() {
                    s.insert(name.into(), v);
                }
            }
            "max" | "min" => {
                if let Some(v) = num() {
                    let key = if name == "max" { "maximum" } else { "minimum" };
                    s.insert(key.into(), v);
                }
            }
            "in" => {
                s.insert("enum".into(), json!(arg.split(',').collect::<Vec<_>>()));
            }
            "regex" => {
                s.insert("pattern".into(), json!(arg));
            }
            _ => {}
        }
    }
    if let Some(t) = ty {
        s.insert(
            "type".into(),
            if nullable {
                json!([t, "null"])
            } else {
                json!(t)
            },
        );
    }
    (s, required)
}

// ------------------------------------------------------------------ paths

fn path_param(name: &str) -> Value {
    json!({"name": name, "in": "path", "required": true, "schema": {"type": "string"}})
}

/// `{param}` segments of a mount path such as `/products/{product_id}/comments`.
fn mount_params(mount: &str) -> Vec<Value> {
    mount
        .split('/')
        .filter_map(|s| s.strip_prefix('{')?.strip_suffix('}'))
        .map(path_param)
        .collect()
}

fn query_param(name: &str, schema: Value, description: &str) -> Value {
    json!({"name": name, "in": "query", "required": false, "schema": schema, "description": description})
}

fn read_params(model: &Model, opts: &RouterOptions, list: bool) -> Vec<Value> {
    let mut p = Vec::new();
    if list {
        p.push(query_param(
            "page",
            json!({"type": "integer", "minimum": 0}),
            "Page index, from 0.",
        ));
        p.push(query_param(
            "size",
            json!({"type": "integer", "minimum": 1, "maximum": 200, "default": 30}),
            "Page size.",
        ));
        p.push(query_param(
            "sort",
            json!({"type": "string"}),
            "`-age,name` (prefix `-` for descending).",
        ));
        p.push(query_param(
            "search",
            json!({"type": "string"}),
            "Free-text search over the search columns.",
        ));
    }
    p.push(query_param(
        "select_columns",
        json!({"type": "string"}),
        "Comma-separated columns to return.",
    ));
    let rels: Vec<&str> = model.relations().iter().map(|r| r.name.as_str()).collect();
    if !rels.is_empty() {
        p.push(json!({
            "name": "include", "in": "query", "required": false,
            "style": "form", "explode": false,
            "schema": {"type": "array", "items": {"type": "string", "enum": rels}},
            "description": "Relations to attach."
        }));
    }
    if opts.export {
        p.push(query_param(
            "output_content_type",
            json!({"type": "string", "enum": ["json", "csv", "xml"]}),
            "Response format.",
        ));
    }
    p
}

fn schema_ref(name: &str) -> Value {
    json!({"$ref": format!("#/components/schemas/{name}")})
}

fn json_body(schema: Value) -> Value {
    json!({"required": true, "content": {"application/json": {"schema": schema}}})
}

fn bulk_body(item: Value) -> Value {
    json_body(
        json!({"type": "object", "required": ["data"], "properties": {"data": {"type": "array", "items": item}}}),
    )
}

/// Success response plus the standard error responses.
fn responses(ok: Value, versioned: bool) -> Value {
    let mut r = Map::new();
    r.insert("200".into(), ok);
    let mut codes = vec![
        ("400", "Malformed input"),
        ("401", "Unauthorized"),
        ("404", "Not found"),
        ("405", "Method not allowed"),
        ("422", "Validation failed"),
        ("500", "Database error"),
    ];
    if versioned {
        codes.extend([("409", "Version conflict"), ("428", "Version required")]);
    }
    for (code, d) in codes {
        r.insert(
            code.into(),
            json!({"description": d, "content": {"application/json": {"schema": schema_ref("Error")}}}),
        );
    }
    Value::Object(r)
}

fn ok(schema: Value, etag: bool) -> Value {
    let mut v = json!({"description": "OK", "content": {"application/json": {"schema": schema}}});
    if etag {
        v["headers"] = json!({"ETag": {"description": "Quoted record version.", "schema": {"type": "string"}}});
    }
    v
}

fn op(summary: String, params: &[Value], body: Option<Value>, responses: Value) -> Value {
    let mut v = json!({"summary": summary, "parameters": params, "responses": responses});
    if let Some(b) = body {
        v["requestBody"] = b;
    }
    v
}

fn collection_ops(
    name: &str,
    m: &Model,
    o: &RouterOptions,
    params: Vec<Value>,
) -> Map<String, Value> {
    let versioned = m.version_column().is_some();
    let summary = || ok(schema_ref("BulkSummary"), false);
    let mut ops = Map::new();
    if o.list {
        let mut p = params.clone();
        p.extend(read_params(m, o, true));
        let list = json!({"type": "object", "properties": {
            "data": {"type": "array", "items": schema_ref(name)},
            "count": {"type": "integer"}
        }});
        ops.insert(
            "get".into(),
            op(
                format!("List {name}"),
                &p,
                None,
                responses(ok(list, false), false),
            ),
        );
    }
    if o.bulk_create {
        let b = bulk_body(schema_ref(&format!("{name}Create")));
        ops.insert(
            "post".into(),
            op(
                format!("Insert many {name}"),
                &params,
                Some(b),
                responses(summary(), false),
            ),
        );
    }
    if o.bulk_update {
        let b = bulk_body(schema_ref(name));
        ops.insert(
            "put".into(),
            op(
                format!("Bulk update {name}"),
                &params,
                Some(b),
                responses(summary(), versioned),
            ),
        );
    }
    if o.bulk_delete {
        let b = json_body(json!({"type": "object", "description": "Filter criteria (non-empty)."}));
        ops.insert(
            "delete".into(),
            op(
                format!("Delete {name} by filter"),
                &params,
                Some(b),
                responses(summary(), false),
            ),
        );
    }
    ops
}

fn item_ops(name: &str, m: &Model, o: &RouterOptions, params: Vec<Value>) -> Map<String, Value> {
    let versioned = m.version_column().is_some();
    let record = || ok(schema_ref(name), versioned);
    let mut write_params = params.clone();
    if versioned {
        write_params.push(json!({
            "name": "If-Match", "in": "header", "required": false,
            "schema": {"type": "string"},
            "description": "Record version (from ETag); used when the body has no version."
        }));
    }
    let mut ops = Map::new();
    if o.get {
        let mut p = params.clone();
        p.extend(read_params(m, o, false));
        ops.insert(
            "get".into(),
            op(
                format!("Get one {name}"),
                &p,
                None,
                responses(record(), false),
            ),
        );
    }
    if o.create {
        let b = json_body(schema_ref(&format!("{name}Create")));
        ops.insert(
            "post".into(),
            op(
                format!("Insert one {name} (path id ignored)"),
                &params,
                Some(b),
                responses(record(), false),
            ),
        );
    }
    if o.update {
        let b = json_body(schema_ref(name));
        ops.insert(
            "put".into(),
            op(
                format!("Replace {name}"),
                &write_params,
                Some(b),
                responses(record(), versioned),
            ),
        );
    }
    if o.patch {
        let b = json_body(schema_ref(name));
        ops.insert(
            "patch".into(),
            op(
                format!("Patch {name}"),
                &write_params,
                Some(b),
                responses(record(), versioned),
            ),
        );
    }
    if o.delete {
        let s = ok(schema_ref("BulkSummary"), false);
        ops.insert(
            "delete".into(),
            op(
                format!("Delete {name}"),
                &write_params,
                None,
                responses(s, versioned),
            ),
        );
    }
    ops
}
