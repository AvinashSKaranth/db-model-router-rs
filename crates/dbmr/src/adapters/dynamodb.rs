use crate::adapters::compiler::{format_bulk_saved_message, format_remove_message};
use crate::core::adapter::DatabaseAdapter;
use crate::core::error::{AppError, Result};
use crate::core::filter::{FilterExpr, Operator};
use crate::core::types::{
    DbValue, FindResult, GetOptions, RowMap, SortDirection, SortKey, WriteSummary,
};
use async_trait::async_trait;
use base64::Engine;
use compact_str::CompactString;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fmt::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// AWS credentials + region used to sign every request with Signature V4.
struct SigV4 {
    access_key: String,
    secret_key: String,
    session_token: Option<String>,
    region: String,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// HMAC-SHA256 (RFC 2104) over `sha2`.
fn hmac(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let inner = Sha256::new()
        .chain_update(k.map(|b| b ^ 0x36))
        .chain_update(msg)
        .finalize();
    Sha256::new()
        .chain_update(k.map(|b| b ^ 0x5c))
        .chain_update(inner)
        .finalize()
        .into()
}

impl SigV4 {
    /// `Authorization` header value. `headers` are (lowercase name, trimmed value) and must
    /// include `host` and `x-amz-date`; all of them are signed. `path` must already be
    /// URI-encoded and the query string empty (every DynamoDB call is `POST /`).
    fn authorization(
        &self,
        service: &str,
        method: &str,
        path: &str,
        amz_date: &str,
        headers: &[(&str, &str)],
        payload: &[u8],
    ) -> String {
        let mut headers = headers.to_vec();
        headers.sort_unstable();
        let canonical_headers = headers.iter().fold(String::new(), |mut s, (k, v)| {
            let _ = writeln!(s, "{k}:{v}");
            s
        });
        let signed_headers = headers
            .iter()
            .map(|(k, _)| *k)
            .collect::<Vec<_>>()
            .join(";");
        let canonical_request = format!(
            "{method}\n{path}\n\n{canonical_headers}\n{signed_headers}\n{}",
            hex(&Sha256::digest(payload))
        );
        let date = &amz_date[..8];
        let scope = format!("{date}/{}/{service}/aws4_request", self.region);
        let to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex(&Sha256::digest(canonical_request.as_bytes()))
        );
        let mut key = hmac(
            format!("AWS4{}", self.secret_key).as_bytes(),
            date.as_bytes(),
        );
        for part in [self.region.as_str(), service, "aws4_request"] {
            key = hmac(&key, part.as_bytes());
        }
        format!(
            "AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={signed_headers}, Signature={}",
            self.access_key,
            hex(&hmac(&key, to_sign.as_bytes()))
        )
    }
}

pub struct DynamoDbAdapter {
    endpoint: String,
    client: reqwest::Client,
    creds: SigV4,
    connected: Arc<AtomicBool>,
}

impl DynamoDbAdapter {
    /// Local/dev endpoint (DynamoDB Local): region `us-east-1`, dummy credentials, still signed.
    pub fn new(endpoint: &str) -> Arc<Self> {
        Self::with_credentials(endpoint, "us-east-1", "dummy", "dummy", None)
    }

    pub fn with_credentials(
        endpoint: &str,
        region: &str,
        access_key: &str,
        secret_key: &str,
        session_token: Option<&str>,
    ) -> Arc<Self> {
        Arc::new(Self {
            endpoint: endpoint.trim_end_matches('/').to_string(),
            client: reqwest::Client::new(),
            creds: SigV4 {
                access_key: access_key.to_string(),
                secret_key: secret_key.to_string(),
                session_token: session_token.map(str::to_string),
                region: region.to_string(),
            },
            connected: Arc::new(AtomicBool::new(true)),
        })
    }

    /// Standard AWS env: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, optional
    /// `AWS_SESSION_TOKEN`, `AWS_REGION` (or `AWS_DEFAULT_REGION`), optional endpoint
    /// override `AWS_ENDPOINT_URL_DYNAMODB` / `AWS_ENDPOINT_URL`.
    pub fn from_env() -> Result<Arc<Self>> {
        let var = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let need =
            |k: &str| var(k).ok_or_else(|| AppError::Internal(format!("DynamoDB: {k} is not set")));
        let access_key = need("AWS_ACCESS_KEY_ID")?;
        let secret_key = need("AWS_SECRET_ACCESS_KEY")?;
        let region = match var("AWS_REGION") {
            Some(r) => r,
            None => need("AWS_DEFAULT_REGION")?,
        };
        let endpoint = var("AWS_ENDPOINT_URL_DYNAMODB")
            .or_else(|| var("AWS_ENDPOINT_URL"))
            .unwrap_or_else(|| format!("https://dynamodb.{region}.amazonaws.com"));
        Ok(Self::with_credentials(
            &endpoint,
            &region,
            &access_key,
            &secret_key,
            var("AWS_SESSION_TOKEN").as_deref(),
        ))
    }

    /// Signed request; returns the HTTP status and parsed JSON body, success or not.
    async fn send_raw(
        &self,
        action: &str,
        payload: &Value,
    ) -> Result<(reqwest::StatusCode, Value)> {
        let req_err = |e: String| AppError::Database(format!("DynamoDB request error: {e}"));
        let url = reqwest::Url::parse(&self.endpoint).map_err(|e| req_err(e.to_string()))?;
        let host = match (url.host_str(), url.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_string(),
            (None, _) => return Err(req_err(format!("no host in {url}"))),
        };
        let body = serde_json::to_vec(payload).map_err(|e| req_err(e.to_string()))?;
        let amz_date = chrono::Utc::now().format("%Y%m%dT%H%M%SZ").to_string();
        let target = format!("DynamoDB_20120810.{action}");
        let mut headers = vec![
            ("host", host.as_str()),
            ("x-amz-date", amz_date.as_str()),
            ("x-amz-target", target.as_str()),
        ];
        if let Some(t) = &self.creds.session_token {
            headers.push(("x-amz-security-token", t.as_str()));
        }
        let auth =
            self.creds
                .authorization("dynamodb", "POST", url.path(), &amz_date, &headers, &body);

        // reqwest derives `host` from the URL; the other signed headers are sent as-is.
        let mut req = self
            .client
            .post(url.clone())
            .header("content-type", "application/x-amz-json-1.0")
            .header("authorization", auth);
        for (k, v) in headers.iter().filter(|(k, _)| *k != "host") {
            req = req.header(*k, *v);
        }
        let res = req
            .body(body)
            .send()
            .await
            .map_err(|e| req_err(e.to_string()))?;

        let status = res.status();
        let body: Value = res
            .json()
            .await
            .map_err(|e| AppError::Database(format!("Failed to parse DynamoDB response: {e}")))?;
        Ok((status, body))
    }

    async fn send_action(&self, action: &str, payload: Value) -> Result<Value> {
        let (status, body) = self.send_raw(action, &payload).await?;
        if !status.is_success() {
            return Err(dynamo_error(status, &body));
        }
        Ok(body)
    }

    pub async fn ensure_table(&self, table: &str, pk: &str) -> Result<()> {
        let payload = json!({
            "TableName": table,
            "AttributeDefinitions": [
                { "AttributeName": pk, "AttributeType": "S" }
            ],
            "KeySchema": [
                { "AttributeName": pk, "KeyType": "HASH" }
            ],
            "BillingMode": "PAY_PER_REQUEST"
        });

        // Ignore ResourceInUseException (table already exists)
        let _ = self.send_action("CreateTable", payload).await;
        Ok(())
    }
}

fn dbvalue_to_dynamo(val: &DbValue) -> Value {
    match val {
        DbValue::Null => json!({ "NULL": true }),
        DbValue::Bool(b) => json!({ "BOOL": b }),
        DbValue::Int(i) => json!({ "N": i.to_string() }),
        DbValue::Float(f) => json!({ "N": f.to_string() }),
        DbValue::Decimal(d) => json!({ "N": d.to_string() }),
        DbValue::Text(s) => json!({ "S": s }),
        DbValue::Bytes(b) => {
            json!({ "B": base64::engine::general_purpose::STANDARD.encode(b) })
        }
        DbValue::Json(j) => json!({ "S": j.to_string() }),
    }
}

fn dynamo_to_dbvalue(val: &Value) -> DbValue {
    if let Some(s) = val.get("S").and_then(Value::as_str) {
        DbValue::Text(s.to_string())
    } else if let Some(n) = val.get("N").and_then(Value::as_str) {
        if let Ok(i) = n.parse::<i128>() {
            DbValue::Int(i)
        } else if let Ok(f) = n.parse::<f64>() {
            DbValue::Float(f)
        } else {
            DbValue::Text(n.to_string())
        }
    } else if let Some(b) = val.get("BOOL").and_then(Value::as_bool) {
        DbValue::Bool(b)
    } else if val.get("NULL").is_some() {
        DbValue::Null
    } else if let Some(b) = val.get("B").and_then(Value::as_str) {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(b)
            .unwrap_or_default();
        DbValue::Bytes(bytes)
    } else {
        DbValue::Null
    }
}

fn cmp_dbvalue(a: &DbValue, b: &DbValue) -> std::cmp::Ordering {
    match (a, b) {
        (DbValue::Int(x), DbValue::Int(y)) => x.cmp(y),
        (DbValue::Float(x), DbValue::Float(y)) => x.total_cmp(y),
        (DbValue::Int(x), DbValue::Float(y)) => (*x as f64).total_cmp(y),
        (DbValue::Float(x), DbValue::Int(y)) => x.total_cmp(&(*y as f64)),
        (DbValue::Text(x), DbValue::Text(y)) => x.cmp(y),
        _ => a.to_string().cmp(&b.to_string()),
    }
}

fn item_to_rowmap(item: &Value) -> RowMap {
    let mut map = RowMap::new();
    if let Some(obj) = item.as_object() {
        for (k, v) in obj {
            map.insert(CompactString::new(k), dynamo_to_dbvalue(v));
        }
    }
    map
}

/// Key for a row inserted without an `id`: time + process-wide counter.
// ponytail: unique per process, collision-free across processes only in practice
// (nanosecond clock); switch to UUIDs if several writers insert into one table.
fn generate_id() -> String {
    static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{nanos:x}-{:x}", SEQ.fetch_add(1, Ordering::Relaxed))
}

fn rowmap_to_dynamo_item(row: &RowMap) -> Value {
    let mut item = serde_json::Map::new();
    for (k, v) in row {
        item.insert(k.to_string(), dbvalue_to_dynamo(v));
    }
    Value::Object(item)
}

fn is_ordering(op: &Operator) -> bool {
    matches!(
        op,
        Operator::Gt | Operator::Gte | Operator::Lt | Operator::Lte
    )
}

fn matches_filter(row: &RowMap, filter: &FilterExpr, safe_delete: Option<&str>) -> bool {
    if let Some(sd) = safe_delete {
        if let Some(val) = row.get(sd) {
            match val {
                DbValue::Int(i) if *i != 0 => return false,
                DbValue::Bool(b) if *b => return false,
                _ => {}
            }
        }
    }

    if filter.groups.is_empty() {
        return true;
    }

    for group in &filter.groups {
        let mut group_match = true;
        for cond in group {
            let row_val = row.get(cond.column.as_str()).unwrap_or(&DbValue::Null);
            let matches = match cond.operator {
                Operator::Eq => row_val == &cond.value,
                Operator::NotEq => row_val != &cond.value,
                // SQL semantics: NULL never satisfies an ordering comparison.
                _ if row_val.is_null() && is_ordering(&cond.operator) => false,
                Operator::Gt => cmp_dbvalue(row_val, &cond.value).is_gt(),
                Operator::Gte => cmp_dbvalue(row_val, &cond.value).is_ge(),
                Operator::Lt => cmp_dbvalue(row_val, &cond.value).is_lt(),
                Operator::Lte => cmp_dbvalue(row_val, &cond.value).is_le(),
                Operator::IsNull => matches!(row_val, DbValue::Null),
                Operator::IsNotNull => !matches!(row_val, DbValue::Null),
                Operator::Like => {
                    let s_row = row_val.to_string().to_lowercase();
                    let s_val = cond.value.to_string().trim_matches('%').to_lowercase();
                    s_row.contains(&s_val)
                }
                Operator::NotLike => {
                    let s_row = row_val.to_string().to_lowercase();
                    let s_val = cond.value.to_string().trim_matches('%').to_lowercase();
                    !s_row.contains(&s_val)
                }
                Operator::In => {
                    if let DbValue::Json(Value::Array(arr)) = &cond.value {
                        arr.iter().any(|item| {
                            row_val.to_string() == item.as_str().unwrap_or(&item.to_string())
                        })
                    } else {
                        false
                    }
                }
                Operator::NotIn => {
                    if let DbValue::Json(Value::Array(arr)) = &cond.value {
                        !arr.iter().any(|item| {
                            row_val.to_string() == item.as_str().unwrap_or(&item.to_string())
                        })
                    } else {
                        true
                    }
                }
            };
            if !matches {
                group_match = false;
                break;
            }
        }
        if group_match {
            return true;
        }
    }

    false
}

#[async_trait]
impl DatabaseAdapter for DynamoDbAdapter {
    async fn disconnect(&self) -> Result<()> {
        self.connected.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    async fn get(
        &self,
        table: &str,
        filter: &FilterExpr,
        sort: &[SortKey],
        safe_delete: Option<&str>,
        options: GetOptions,
    ) -> Result<FindResult> {
        // Scan pages until LastEvaluatedKey is absent (1 MB per page), then filter in memory.
        // ponytail: full-table scan per query; add Query/GSI support for large tables.
        let mut rows: Vec<RowMap> = Vec::new();
        let mut start_key = Value::Null;
        loop {
            let mut payload = json!({ "TableName": table });
            if !start_key.is_null() {
                payload["ExclusiveStartKey"] = start_key;
            }
            let res = self.send_action("Scan", payload).await?;
            rows.extend(
                res["Items"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(item_to_rowmap)
                    .filter(|r| matches_filter(r, filter, safe_delete)),
            );
            start_key = res["LastEvaluatedKey"].clone();
            if start_key.is_null() {
                break;
            }
        }

        if let Some(k) = sort.first() {
            rows.sort_by(|a, b| {
                let va = a.get(k.column.as_str()).unwrap_or(&DbValue::Null);
                let vb = b.get(k.column.as_str()).unwrap_or(&DbValue::Null);
                if k.direction == SortDirection::Desc {
                    crate::adapters::compiler::sort_order(vb, va)
                } else {
                    crate::adapters::compiler::sort_order(va, vb)
                }
            });
        }

        let total = rows.len() as u64;
        if let Some(offset) = options.offset {
            if (offset as usize) < rows.len() {
                rows = rows.split_off(offset as usize);
            } else {
                rows.clear();
            }
        }

        if let Some(limit) = options.limit {
            rows.truncate(limit as usize);
        }

        let count = if options.count {
            total
        } else {
            rows.len() as u64
        };
        Ok(FindResult { data: rows, count })
    }

    async fn list(
        &self,
        table: &str,
        filter: &FilterExpr,
        sort: &[SortKey],
        safe_delete: Option<&str>,
        page: u64,
        size: u64,
    ) -> Result<FindResult> {
        let total = self.count(table, filter, safe_delete).await?;
        let options = GetOptions {
            limit: Some(size),
            offset: Some(page.saturating_mul(size)),
            count: false,
        };
        let mut res = self.get(table, filter, sort, safe_delete, options).await?;
        res.count = total;
        Ok(res)
    }

    async fn count(
        &self,
        table: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64> {
        let res = self
            .get(table, filter, &[], safe_delete, GetOptions::default())
            .await?;
        Ok(res.data.len() as u64)
    }

    async fn insert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        _unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        if data.is_empty() {
            return Ok(WriteSummary {
                rows: 0,
                message: format_bulk_saved_message(table, 0),
                id: None,
                record: None,
            });
        }

        let mut last_id = None;
        let mut last_record = None;
        for row in data {
            let mut row = row.clone();
            if row.get("id").is_none_or(DbValue::is_null) {
                row.insert("id".into(), DbValue::Text(generate_id()));
            }
            let item = rowmap_to_dynamo_item(&row);
            let payload = json!({
                "TableName": table,
                "Item": item
            });

            self.send_action("PutItem", payload).await?;
            last_id = row.get("id").cloned();
            last_record = Some(row);
        }

        let count = data.len() as u64;
        Ok(WriteSummary {
            rows: count,
            message: format_bulk_saved_message(table, count),
            id: if count == 1 { last_id } else { None },
            record: if count == 1 { last_record } else { None },
        })
    }

    async fn upsert_rows(
        &self,
        table: &str,
        data: &[RowMap],
        unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        let mut rows = Vec::with_capacity(data.len());
        for row in data {
            rows.push(crate::adapters::compiler::merge_existing(self, table, row, unique).await?);
        }
        self.insert_rows(table, &rows, unique).await
    }

    async fn remove_rows(
        &self,
        table: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<WriteSummary> {
        let matches = self
            .get(table, filter, &[], safe_delete, GetOptions::default())
            .await?;
        let count = matches.data.len() as u64;

        for row in &matches.data {
            if let Some(id_val) = row.get("id") {
                if let Some(sd) = safe_delete {
                    let mut updated = row.clone();
                    updated.insert(CompactString::new(sd), DbValue::Int(1));
                    let item = rowmap_to_dynamo_item(&updated);
                    let payload = json!({
                        "TableName": table,
                        "Item": item
                    });
                    self.send_action("PutItem", payload).await?;
                } else {
                    let payload = json!({
                        "TableName": table,
                        "Key": {
                            "id": dbvalue_to_dynamo(id_val)
                        }
                    });
                    self.send_action("DeleteItem", payload).await?;
                }
            }
        }

        Ok(WriteSummary {
            rows: count,
            message: format_remove_message(table, count),
            id: None,
            record: None,
        })
    }

    /// Scan + match, then one conditional `UpdateItem` per matched item. The condition
    /// re-checks that the item still exists, the Eq conditions of the group that matched
    /// (e.g. `id` and `version`) and the soft-delete flag, so a concurrent change makes that
    /// item fail its condition and not count. `id` (the hash key) is immutable and skipped.
    // ponytail: only non-null Eq conditions are re-checked server-side; range/LIKE are
    // checked at scan time only. Translate them into the ConditionExpression if needed.
    async fn update_where(
        &self,
        table: &str,
        set: &RowMap,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64> {
        let set: Vec<_> = set.iter().filter(|(k, _)| k.as_str() != "id").collect();
        if set.is_empty() {
            return Ok(0);
        }
        let matches = self
            .get(table, filter, &[], safe_delete, GetOptions::default())
            .await?;
        let mut changed = 0;
        for row in &matches.data {
            let Some(id) = row.get("id") else { continue };
            let mut names = serde_json::Map::new();
            let mut values = serde_json::Map::new();
            let mut sets = Vec::new();
            for (i, (k, v)) in set.iter().enumerate() {
                names.insert(format!("#s{i}"), json!(k.as_str()));
                values.insert(format!(":s{i}"), dbvalue_to_dynamo(v));
                sets.push(format!("#s{i} = :s{i}"));
            }
            names.insert("#pk".into(), json!("id"));
            let mut conds = vec!["attribute_exists(#pk)".to_string()];
            let group = filter.groups.iter().find(|g| {
                let one = FilterExpr {
                    groups: vec![(*g).clone()],
                };
                matches_filter(row, &one, None)
            });
            let eqs = group
                .into_iter()
                .flatten()
                .filter(|c| c.operator == Operator::Eq && !c.value.is_null());
            for (i, c) in eqs.enumerate() {
                names.insert(format!("#c{i}"), json!(c.column.as_str()));
                values.insert(format!(":c{i}"), dbvalue_to_dynamo(&c.value));
                conds.push(format!("#c{i} = :c{i}"));
            }
            if let Some(sd) = safe_delete {
                names.insert("#sd".into(), json!(sd));
                values.insert(":sd1".into(), json!({ "N": "1" }));
                values.insert(":sdt".into(), json!({ "BOOL": true }));
                conds.push("NOT (#sd = :sd1 OR #sd = :sdt)".into());
            }
            let payload = json!({
                "TableName": table,
                "Key": { "id": dbvalue_to_dynamo(id) },
                "UpdateExpression": format!("SET {}", sets.join(", ")),
                "ConditionExpression": conds.join(" AND "),
                "ExpressionAttributeNames": names,
                "ExpressionAttributeValues": values,
            });
            let (status, body) = self.send_raw("UpdateItem", &payload).await?;
            if status.is_success() {
                changed += 1;
            } else if !body["__type"]
                .as_str()
                .is_some_and(|t| t.ends_with("ConditionalCheckFailedException"))
            {
                return Err(dynamo_error(status, &body));
            }
        }
        Ok(changed)
    }

    async fn ping(&self) -> Result<()> {
        self.send_action("ListTables", json!({ "Limit": 1 }))
            .await
            .map(|_| ())
    }

    async fn query(&self, _sql: &str, _params: &[DbValue]) -> Result<Vec<RowMap>> {
        Err(AppError::Database(
            "Raw SQL queries are not supported on DynamoDB".to_string(),
        ))
    }

    fn escape_id(&self, name: &str) -> String {
        name.to_string()
    }
}

fn dynamo_error(status: reqwest::StatusCode, body: &Value) -> AppError {
    let msg = body["message"]
        .as_str()
        .or_else(|| body["Message"].as_str())
        .unwrap_or("Unknown DynamoDB error");
    AppError::Database(format!("DynamoDB error ({status}): {msg}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AWS SigV4 test suite, "get-vanilla".
    #[test]
    fn sigv4_get_vanilla() {
        let creds = SigV4 {
            access_key: "AKIDEXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: None,
            region: "us-east-1".into(),
        };
        let date = "20150830T123600Z";
        let auth = creds.authorization(
            "service",
            "GET",
            "/",
            date,
            &[("x-amz-date", date), ("host", "example.amazonaws.com")],
            b"",
        );
        assert_eq!(
            auth,
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, \
             SignedHeaders=host;x-amz-date, \
             Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
    }

    /// RFC 4231 test case 2.
    #[test]
    fn hmac_sha256() {
        assert_eq!(
            hex(&hmac(b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
