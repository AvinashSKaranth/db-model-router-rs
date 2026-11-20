#![cfg(feature = "dynamodb")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args,
    clippy::many_single_char_names,
    clippy::needless_raw_string_hashes
)]

use compact_str::CompactString;
use dbmr::adapters::DynamoDbAdapter;
use dbmr::core::adapter::DatabaseAdapter;
use dbmr::core::filter::{Condition, FilterExpr, Operator};
use dbmr::core::types::{DbValue, GetOptions, RowMap, SortDirection, SortKey};
use std::sync::Arc;

fn get_dynamodb() -> Arc<DynamoDbAdapter> {
    DynamoDbAdapter::new("http://127.0.0.1:8000")
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_dynamodb_crud() {
    let adapter = get_dynamodb();

    let table = "test_dynamo_users";
    if let Err(e) = adapter.ensure_table(table, "id").await {
        panic!("DynamoDB local container not ready or reachable: {e}");
    }

    // 1. Insert rows
    let mut row1 = RowMap::new();
    row1.insert(
        CompactString::new("id"),
        DbValue::Text("user_1".to_string()),
    );
    row1.insert(
        CompactString::new("name"),
        DbValue::Text("Alice".to_string()),
    );
    row1.insert(CompactString::new("score"), DbValue::Int(100));

    let mut row2 = RowMap::new();
    row2.insert(
        CompactString::new("id"),
        DbValue::Text("user_2".to_string()),
    );
    row2.insert(CompactString::new("name"), DbValue::Text("Bob".to_string()));
    row2.insert(CompactString::new("score"), DbValue::Int(80));

    let summary = adapter
        .insert_rows(table, &[row1, row2], &[])
        .await
        .unwrap();
    assert_eq!(summary.rows, 2);

    // 2. Count
    let total = adapter
        .count(table, &FilterExpr::default(), None)
        .await
        .unwrap();
    assert!(total >= 2);

    // 3. Filter query
    let mut filter = FilterExpr::default();
    filter.groups.push(vec![Condition {
        column: CompactString::new("name"),
        operator: Operator::Eq,
        value: DbValue::Text("Alice".to_string()),
    }]);

    let res = adapter
        .get(
            table,
            &filter,
            &[SortKey {
                column: CompactString::new("score"),
                direction: SortDirection::Desc,
            }],
            None,
            GetOptions::default(),
        )
        .await
        .unwrap();

    assert_eq!(res.count, 1);
    assert_eq!(
        res.data[0].get("name").unwrap(),
        &DbValue::Text("Alice".to_string())
    );

    // 4. Remove row
    let del_summary = adapter.remove_rows(table, &filter, None).await.unwrap();
    assert_eq!(del_summary.rows, 1);
}

// ---- coverage: value round-trips, client-side filters, sorting, paging, errors ----

fn cond(column: &str, operator: Operator, value: DbValue) -> Condition {
    Condition {
        column: column.into(),
        operator,
        value,
    }
}

fn one(c: Condition) -> FilterExpr {
    FilterExpr {
        groups: vec![vec![c]],
    }
}

fn text_ids(rows: &[RowMap]) -> Vec<String> {
    let mut v: Vec<String> = rows
        .iter()
        .map(|r| match r.get("id") {
            Some(DbValue::Text(s)) => s.clone(),
            other => panic!("unexpected id {other:?}"),
        })
        .collect();
    v.sort();
    v
}

async fn fresh(table: &str) -> Arc<DynamoDbAdapter> {
    let a = get_dynamodb();
    a.ensure_table(table, "id").await.unwrap();
    a.remove_rows(table, &FilterExpr::empty(), None)
        .await
        .unwrap();
    a
}

async fn seed_people(t: &str) -> Arc<DynamoDbAdapter> {
    let a = fresh(t).await;
    let people = [
        ("p1", "Alice", 30, 1.5, Some("x")),
        ("p2", "bob", 20, 2.5, None),
        ("p3", "Carol", 40, 10.5, Some("y")),
        ("p4", "dave", 10, 3.5, None),
    ];
    let rows: Vec<RowMap> = people
        .iter()
        .map(|(id, name, age, score, tag)| {
            let mut r = RowMap::new();
            r.insert("id".into(), DbValue::Text((*id).into()));
            r.insert("name".into(), DbValue::Text((*name).into()));
            r.insert("age".into(), DbValue::Int(*age));
            r.insert("score".into(), DbValue::Float(*score));
            if let Some(tag) = tag {
                r.insert("tag".into(), DbValue::Text((*tag).into()));
            }
            r
        })
        .collect();
    a.insert_rows(t, &rows, &[]).await.unwrap();
    a
}

async fn ids_for(a: &DynamoDbAdapter, t: &str, f: FilterExpr) -> Vec<String> {
    text_ids(
        &a.get(t, &f, &[], None, GetOptions::default())
            .await
            .unwrap()
            .data,
    )
}

#[tokio::test]
async fn test_dynamodb_unreachable_endpoint() {
    let a = DynamoDbAdapter::new("http://127.0.0.1:1/");
    let e = a
        .count("cov_x", &FilterExpr::empty(), None)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("DynamoDB request error"), "{e}");
    // ensure_table swallows errors by design (table may already exist).
    assert!(a.ensure_table("cov_x", "id").await.is_ok());
}

/// One-shot HTTP peer that answers any request with `status` and `body`.
async fn fake_endpoint(status: &'static str, body: &'static str) -> String {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    const CRLF: &str = "\u{d}\u{a}";
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 8192];
        let _ = sock.read(&mut buf).await;
        let resp = format!(
            "HTTP/1.1 {status}{CRLF}Content-Type: application/json{CRLF}Content-Length: {}{CRLF}Connection: close{CRLF}{CRLF}{body}",
            body.len()
        );
        sock.write_all(resp.as_bytes()).await.unwrap();
    });
    format!("http://{addr}")
}

#[tokio::test]
async fn test_dynamodb_malformed_responses() {
    let all = FilterExpr::empty();
    let a = DynamoDbAdapter::new(&fake_endpoint("200 OK", "<html>not json</html>").await);
    let e = a.count("cov_x", &all, None).await.unwrap_err();
    assert!(
        e.to_string().contains("Failed to parse DynamoDB response"),
        "{e}"
    );

    let a = DynamoDbAdapter::new(&fake_endpoint("500 Internal Server Error", "{}").await);
    let e = a.count("cov_x", &all, None).await.unwrap_err();
    assert!(
        e.to_string()
            .contains("(500 Internal Server Error): Unknown DynamoDB error"),
        "{e}"
    );

    let a = DynamoDbAdapter::new(
        &fake_endpoint("400 Bad Request", r#"{"Message":"capitalised"}"#).await,
    );
    let e = a.count("cov_x", &all, None).await.unwrap_err();
    assert!(e.to_string().ends_with("capitalised"), "{e}");
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_dynamodb_cov_misc_and_errors() {
    let a = get_dynamodb();
    assert!(a.is_connected().await);
    a.disconnect().await.unwrap();
    assert!(!a.is_connected().await);
    assert_eq!(a.escape_id("x y"), "x y");
    let e = a.query("SELECT 1", &[]).await.unwrap_err();
    assert!(e.to_string().contains("not supported"), "{e}");

    // Missing table: DynamoDB's 400 message is surfaced.
    let e = a
        .count("cov_dynamo_no_such_table", &FilterExpr::empty(), None)
        .await
        .unwrap_err();
    let msg = e.to_string();
    assert!(msg.contains("DynamoDB error (400"), "{msg}");
    assert!(msg.contains("non-existent table"), "{msg}");

    let s = a.insert_rows("cov_dynamo_misc", &[], &[]).await.unwrap();
    assert_eq!(s.rows, 0);
    assert_eq!(s.message, "0 Cov Dynamo Miscs are saved");
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_dynamodb_cov_value_roundtrip_and_generated_ids() {
    let t = "cov_dynamo_values";
    let a = fresh(t).await;

    let decimal =
        DbValue::from(serde_json::from_str::<serde_json::Value>("12.345678901234567890").unwrap());
    assert!(matches!(decimal, DbValue::Decimal(_)));
    let mut row = RowMap::new();
    row.insert("nothing".into(), DbValue::Null);
    row.insert("flag".into(), DbValue::Bool(true));
    row.insert("ratio".into(), DbValue::Float(1.5));
    row.insert("money".into(), decimal);
    row.insert("blob".into(), DbValue::Bytes(vec![0, 159, 146, 150]));
    row.insert(
        "doc".into(),
        DbValue::Json(serde_json::json!({"a": [1, 2]})),
    );

    let s = a.insert_rows(t, &[row.clone()], &[]).await.unwrap();
    assert_eq!(s.rows, 1);
    let Some(DbValue::Text(id)) = s.id else {
        panic!("generated id must be text, got {:?}", s.id)
    };
    assert_eq!(s.record.unwrap()["id"], DbValue::Text(id.clone()));

    row.insert("id".into(), DbValue::Null);
    let s = a.insert_rows(t, &[row.clone(), row], &[]).await.unwrap();
    assert_eq!(s.rows, 2);
    assert!(s.id.is_none() && s.record.is_none());
    assert_eq!(a.count(t, &FilterExpr::empty(), None).await.unwrap(), 3);

    let res = a
        .get(
            t,
            &FilterExpr::eq("id", id.as_str()),
            &[],
            None,
            GetOptions::default(),
        )
        .await
        .unwrap();
    let r = &res.data[0];
    assert_eq!(r["nothing"], DbValue::Null);
    assert_eq!(r["flag"], DbValue::Bool(true));
    assert_eq!(r["ratio"], DbValue::Float(1.5));
    assert!(matches!(r["money"], DbValue::Float(f) if (f - 12.345_678_901_234_567).abs() < 1e-9));
    assert_eq!(r["blob"], DbValue::Bytes(vec![0, 159, 146, 150]));
    // JSON is stored as a string attribute.
    assert_eq!(r["doc"], DbValue::Text(r#"{"a":[1,2]}"#.into()));
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_dynamodb_cov_unknown_attribute_type_reads_as_null() {
    // Another writer stored a Map attribute, which the adapter does not model.
    let t = "cov_dynamo_foreign";
    let a = fresh(t).await;
    let res = reqwest::Client::new()
        .post("http://127.0.0.1:8000")
        .header("Content-Type", "application/x-amz-json-1.0")
        .header("X-Amz-Target", "DynamoDB_20120810.PutItem")
        .header("Authorization", "AWS4-HMAC-SHA256 Credential=dummy/20260101/us-east-1/dynamodb/aws4_request, SignedHeaders=host, Signature=00")
        .body(r#"{"TableName":"cov_dynamo_foreign","Item":{"id":{"S":"m1"},"meta":{"M":{"k":{"S":"v"}}}}}"#)
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success(), "{:?}", res.text().await);
    let r = a
        .get(t, &FilterExpr::empty(), &[], None, GetOptions::default())
        .await
        .unwrap();
    assert_eq!(r.data.len(), 1);
    assert_eq!(r.data[0]["meta"], DbValue::Null);
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_dynamodb_cov_filter_operators() {
    use serde_json::json;
    use DbValue::{Float, Int, Json, Text};
    use Operator::{Eq, Gt, Gte, In, IsNotNull, IsNull, Like, Lt, Lte, NotEq, NotIn, NotLike};
    let t = "cov_dynamo_filters";
    let a = seed_people(t).await;
    let q = |c: Condition| ids_for(&a, t, one(c));
    let v = |s: &[&str]| s.iter().map(ToString::to_string).collect::<Vec<_>>();

    assert_eq!(q(cond("age", NotEq, Int(30))).await, v(&["p2", "p3", "p4"]));
    assert_eq!(q(cond("age", Gt, Int(20))).await, v(&["p1", "p3"]));
    assert_eq!(q(cond("age", Lt, Int(20))).await, v(&["p4"]));
    assert_eq!(q(cond("age", Gte, Int(30))).await, v(&["p1", "p3"]));
    assert_eq!(q(cond("age", Lte, Int(20))).await, v(&["p2", "p4"]));
    assert_eq!(q(cond("score", Gt, Float(3.0))).await, v(&["p3", "p4"]));
    // Mixed Int/Float compare numerically, not as strings ("10.5" > "9").
    assert_eq!(q(cond("score", Gt, Int(9))).await, v(&["p3"]));
    assert_eq!(q(cond("age", Lt, Float(10.5))).await, v(&["p4"]));
    assert_eq!(
        q(cond("name", Gte, Text("b".into()))).await,
        v(&["p2", "p4"])
    );
    // Missing values never satisfy ordering comparisons.
    assert_eq!(q(cond("tag", Gt, Text("a".into()))).await, v(&["p1", "p3"]));
    assert_eq!(q(cond("tag", Lt, Text("z".into()))).await, v(&["p1", "p3"]));

    assert_eq!(
        q(cond("name", Like, Text("%A%".into()))).await,
        v(&["p1", "p3", "p4"])
    );
    assert_eq!(
        q(cond("name", NotLike, Text("%o%".into()))).await,
        v(&["p1", "p4"])
    );

    assert_eq!(
        q(cond("name", In, Json(json!(["bob", "dave"])))).await,
        v(&["p2", "p4"])
    );
    assert_eq!(
        q(cond("age", In, Json(json!([10, 40])))).await,
        v(&["p3", "p4"])
    );
    assert!(q(cond("age", In, Int(10))).await.is_empty());
    assert_eq!(
        q(cond("age", NotIn, Json(json!([10, 40])))).await,
        v(&["p1", "p2"])
    );
    assert_eq!(q(cond("age", NotIn, Int(10))).await.len(), 4);

    assert_eq!(
        q(cond("tag", IsNull, DbValue::Null)).await,
        v(&["p2", "p4"])
    );
    assert_eq!(
        q(cond("tag", IsNotNull, DbValue::Null)).await,
        v(&["p1", "p3"])
    );

    let f = FilterExpr {
        groups: vec![
            vec![cond("age", Gte, Int(30)), cond("score", Gt, Float(5.0))],
            vec![cond("name", Eq, Text("bob".into()))],
        ],
    };
    assert_eq!(ids_for(&a, t, f).await, v(&["p2", "p3"]));
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_dynamodb_cov_sort_paging_upsert_soft_delete() {
    let t = "cov_dynamo_sort";
    let a = seed_people(t).await;
    let all = FilterExpr::empty();
    let key = |c: &str, direction| SortKey {
        column: c.into(),
        direction,
    };
    let col = |rows: &[RowMap], c: &str| rows.iter().map(|r| r[c].clone()).collect::<Vec<_>>();

    let by_score = a
        .get(
            t,
            &all,
            &[key("score", SortDirection::Asc)],
            None,
            GetOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        col(&by_score.data, "score"),
        [1.5, 2.5, 3.5, 10.5].map(DbValue::Float)
    );
    let by_name = a
        .get(
            t,
            &all,
            &[key("name", SortDirection::Asc)],
            None,
            GetOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(
        col(&by_name.data, "name"),
        ["Alice", "Carol", "bob", "dave"].map(|s| DbValue::Text(s.into()))
    );

    // list(): count is the total, data is one page; past-the-end pages are empty.
    let page = a
        .list(t, &all, &[key("age", SortDirection::Desc)], None, 1, 3)
        .await
        .unwrap();
    assert_eq!(page.count, 4);
    assert_eq!(col(&page.data, "age"), vec![DbValue::Int(10)]);
    let past = a.list(t, &all, &[], None, 5, 3).await.unwrap();
    assert_eq!(past.count, 4);
    assert!(past.data.is_empty());
    let opts = GetOptions {
        count: true,
        limit: Some(1),
        offset: Some(0),
    };
    let g = a.get(t, &all, &[], None, opts).await.unwrap();
    assert_eq!((g.data.len(), g.count), (1, 4));

    // Upsert merges into the matched row.
    let mut patch = RowMap::new();
    patch.insert("id".into(), DbValue::Text("p1".into()));
    patch.insert("age".into(), DbValue::Int(31));
    let s = a
        .upsert_rows(t, &[patch], &[vec!["id".to_string()]])
        .await
        .unwrap();
    assert_eq!(s.rows, 1);
    let r = a
        .get(
            t,
            &FilterExpr::eq("id", "p1"),
            &[],
            None,
            GetOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(r.data[0]["age"], DbValue::Int(31));
    assert_eq!(r.data[0]["name"], DbValue::Text("Alice".into()));

    // Soft delete flags rows, hiding them from soft-delete-aware reads.
    let young = one(cond("age", Operator::Lte, DbValue::Int(20)));
    let s = a.remove_rows(t, &young, Some("deleted")).await.unwrap();
    assert_eq!(s.rows, 2);
    assert_eq!(s.message, "2 cov_dynamo_sorts removed");
    assert_eq!(a.count(t, &all, Some("deleted")).await.unwrap(), 2);
    assert_eq!(a.count(t, &all, None).await.unwrap(), 4);
    let flagged = a
        .get(
            t,
            &FilterExpr::eq("deleted", 1),
            &[],
            None,
            GetOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(text_ids(&flagged.data), vec!["p2", "p4"]);
    // A Bool flag also hides rows.
    let mut b = RowMap::new();
    b.insert("id".into(), DbValue::Text("p5".into()));
    b.insert("deleted".into(), DbValue::Bool(true));
    a.insert_rows(t, &[b], &[]).await.unwrap();
    assert_eq!(a.count(t, &all, Some("deleted")).await.unwrap(), 2);
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_dynamodb_cov_scan_pagination() {
    // Scan returns at most 1 MB per page; ~350 KB items force several pages.
    let t = "cov_dynamo_pages";
    let a = fresh(t).await;
    let big = "x".repeat(350 * 1024);
    let rows: Vec<RowMap> = (0..5)
        .map(|i| {
            let mut r = RowMap::new();
            r.insert("id".into(), DbValue::Text(format!("big{i}")));
            r.insert("payload".into(), DbValue::Text(big.clone()));
            r
        })
        .collect();
    a.insert_rows(t, &rows, &[]).await.unwrap();
    let res = a
        .get(t, &FilterExpr::empty(), &[], None, GetOptions::default())
        .await
        .unwrap();
    assert_eq!(
        text_ids(&res.data),
        vec!["big0", "big1", "big2", "big3", "big4"]
    );
    a.remove_rows(t, &FilterExpr::empty(), None).await.unwrap();
}

/// update_where contract shared by the NoSQL adapters: match, no match, soft delete,
/// optimistic-lock compare-and-set (sequential and concurrent), empty set, ping.
async fn update_where_contract(a: &dyn DatabaseAdapter, t: &str) {
    use dbmr::core::filter::condition;
    let all = FilterExpr::empty();
    a.remove_rows(t, &all, None).await.unwrap();
    let rows: Vec<RowMap> = ["u1", "u2"]
        .iter()
        .map(|id| {
            let mut r = RowMap::new();
            r.insert("id".into(), DbValue::Text((*id).into()));
            r.insert("name".into(), DbValue::Text("old".into()));
            r.insert("version".into(), DbValue::Int(1));
            r.insert("deleted".into(), DbValue::Int(0));
            r
        })
        .collect();
    a.insert_rows(t, &rows, &[]).await.unwrap();
    let set = |k: &str, v: DbValue| {
        let mut r = RowMap::new();
        r.insert(k.into(), v);
        r
    };
    let by_id = |id: &str| FilterExpr::eq("id", id);
    let read = |id: &'static str| async move {
        a.get(t, &by_id(id), &[], None, GetOptions::default())
            .await
            .unwrap()
            .data
            .remove(0)
    };

    a.ping().await.unwrap();
    assert_eq!(
        a.update_where(t, &RowMap::new(), &by_id("u1"), None)
            .await
            .unwrap(),
        0
    );

    // Match / no match.
    let name = set("name", DbValue::Text("new".into()));
    assert_eq!(
        a.update_where(t, &name, &by_id("u1"), None).await.unwrap(),
        1
    );
    assert_eq!(read("u1").await["name"], DbValue::Text("new".into()));
    assert_eq!(read("u1").await["version"], DbValue::Int(1));
    assert_eq!(
        a.update_where(t, &name, &by_id("nope"), None)
            .await
            .unwrap(),
        0
    );

    // Soft-deleted rows are skipped.
    a.remove_rows(t, &by_id("u2"), Some("deleted"))
        .await
        .unwrap();
    assert_eq!(
        a.update_where(t, &name, &by_id("u2"), Some("deleted"))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        a.update_where(t, &name, &all, Some("deleted"))
            .await
            .unwrap(),
        1
    );
    assert_eq!(read("u2").await["name"], DbValue::Text("old".into()));

    // Optimistic lock: two writers with the same old version, first wins.
    let cas = |v: i128| FilterExpr {
        groups: vec![vec![
            condition("id", Operator::Eq, DbValue::Text("u1".into())),
            condition("version", Operator::Eq, DbValue::Int(v)),
        ]],
    };
    let bump = |v: i128| set("version", DbValue::Int(v));
    assert_eq!(a.update_where(t, &bump(2), &cas(1), None).await.unwrap(), 1);
    assert_eq!(a.update_where(t, &bump(2), &cas(1), None).await.unwrap(), 0);
    assert_eq!(read("u1").await["version"], DbValue::Int(2));

    // Concurrent: both may pass the scan, the server-side check lets exactly one win.
    let (b3, b30, c2) = (bump(3), bump(30), cas(2));
    let (x, y) = tokio::join!(
        a.update_where(t, &b3, &c2, None),
        a.update_where(t, &b30, &c2, None)
    );
    assert_eq!(x.unwrap() + y.unwrap(), 1);
    let v = read("u1").await["version"].clone();
    assert!(v == DbValue::Int(3) || v == DbValue::Int(30), "{v:?}");
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_dynamodb_update_where_and_ping() {
    let t = "cov_dynamo_update_where";
    let a = fresh(t).await;
    update_where_contract(&*a, t).await;
}
