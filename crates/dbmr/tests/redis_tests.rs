#![cfg(feature = "redis")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args,
    clippy::many_single_char_names
)]

use dbmr::adapters::RedisAdapter;
use dbmr::core::adapter::DatabaseAdapter;
use dbmr::core::filter::{Condition, FilterExpr, Operator};
use dbmr::core::types::{DbValue, GetOptions, RowMap, SortDirection, SortKey};

async fn setup_redis() -> RedisAdapter {
    let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let adapter = RedisAdapter::connect(&url).expect("connect to redis");

    // Clean test namespace
    let _ = adapter
        .remove_rows("test_users", &FilterExpr::empty(), None)
        .await;

    (*adapter).clone()
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_redis_crud() {
    let adapter = setup_redis().await;

    // Single insert
    let mut row = RowMap::new();
    row.insert("id".into(), DbValue::Text("user_1".into()));
    row.insert("name".into(), DbValue::Text("Alice Redis".into()));
    row.insert("age".into(), DbValue::Int(30));
    row.insert("active".into(), DbValue::Bool(true));

    let summary = adapter
        .insert_rows("test_users", &[row], &[])
        .await
        .expect("insert");
    assert_eq!(summary.rows, 1);
    assert_eq!(summary.message, "1 Test Users is saved");

    // Get
    let filter = FilterExpr {
        groups: vec![vec![Condition {
            column: "id".into(),
            operator: Operator::Eq,
            value: DbValue::Text("user_1".into()),
        }]],
    };
    let res = adapter
        .get("test_users", &filter, &[], None, GetOptions::default())
        .await
        .expect("get");
    assert_eq!(res.data.len(), 1);
    assert_eq!(
        res.data[0].get("name").unwrap(),
        &DbValue::Text("Alice Redis".into())
    );

    // Bulk insert
    let mut bulk = Vec::new();
    for i in 2..=5 {
        let mut r = RowMap::new();
        r.insert("id".into(), DbValue::Text(format!("user_{i}")));
        r.insert("name".into(), DbValue::Text(format!("Bulk {i}")));
        r.insert("age".into(), DbValue::Int(i * 10));
        bulk.push(r);
    }
    let bulk_summary = adapter
        .insert_rows("test_users", &bulk, &[])
        .await
        .expect("bulk insert");
    assert_eq!(bulk_summary.rows, 4);

    // List with sorting
    let sorted = vec![SortKey {
        column: "age".into(),
        direction: SortDirection::Desc,
    }];
    let list_res = adapter
        .list("test_users", &FilterExpr::empty(), &sorted, None, 0, 10)
        .await
        .expect("list");
    assert_eq!(list_res.data.len(), 5);
    assert_eq!(list_res.count, 5);
    assert_eq!(list_res.data[0].get("age").unwrap(), &DbValue::Int(50));

    // Remove
    let del_summary = adapter
        .remove_rows("test_users", &filter, None)
        .await
        .expect("remove");
    assert_eq!(del_summary.rows, 1);
    assert_eq!(del_summary.message, "1 test_users removed");

    let count_after = adapter
        .count("test_users", &FilterExpr::empty(), None)
        .await
        .expect("count");
    assert_eq!(count_after, 4);
}

// ---- coverage: value round-trips, client-side filters, sorting, paging, errors ----

fn redis_url() -> String {
    std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string())
}

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

fn ids(rows: &[RowMap]) -> Vec<i128> {
    let mut v: Vec<i128> = rows
        .iter()
        .map(|r| match r.get("id") {
            Some(DbValue::Int(i)) => *i,
            other => panic!("unexpected id {other:?}"),
        })
        .collect();
    v.sort_unstable();
    v
}

async fn fresh(table: &str) -> RedisAdapter {
    let a = RedisAdapter::connect(&redis_url()).expect("connect");
    a.remove_rows(table, &FilterExpr::empty(), None)
        .await
        .expect("clean");
    (*a).clone()
}

#[test]
fn test_redis_invalid_url() {
    let Err(e) = RedisAdapter::connect("not a url") else {
        panic!("bad URL must fail")
    };
    assert!(e.to_string().contains("Invalid Redis URL"), "{e}");
}

#[tokio::test]
async fn test_redis_unreachable_server() {
    let a = RedisAdapter::connect("redis://127.0.0.1:1").expect("url parses");
    assert!(!a.is_connected().await);
    let e = a
        .list("cov_x", &FilterExpr::empty(), &[], None, 0, 10)
        .await
        .unwrap_err();
    assert!(e.to_string().contains("Redis connection failed"), "{e}");
    let mut r = RowMap::new();
    r.insert("id".into(), DbValue::Int(1));
    assert!(a.insert_rows("cov_x", &[r], &[]).await.is_err());
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_redis_cov_misc() {
    let client = redis::Client::open(redis_url().as_str()).unwrap();
    let a = RedisAdapter::new(client);
    assert!(a.is_connected().await);
    a.disconnect().await.unwrap();
    assert_eq!(a.escape_id("weird name"), "weird name");
    let e = a.query("SELECT 1", &[]).await.unwrap_err();
    assert!(e.to_string().contains("not supported"), "{e}");

    // Empty insert is a no-op summary.
    let s = a.insert_rows("cov_redis_misc", &[], &[]).await.unwrap();
    assert_eq!(s.rows, 0);
    assert_eq!(s.message, "0 Cov Redis Miscs are saved");
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_redis_cov_value_roundtrip_and_generated_ids() {
    let t = "cov_redis_values";
    let a = fresh(t).await;

    let decimal =
        DbValue::from(serde_json::from_str::<serde_json::Value>("12.345678901234567890").unwrap());
    assert!(matches!(decimal, DbValue::Decimal(_)));
    let mut row = RowMap::new();
    row.insert("nothing".into(), DbValue::Null);
    row.insert("flag".into(), DbValue::Bool(false));
    row.insert("ratio".into(), DbValue::Float(1.5));
    row.insert("money".into(), decimal);
    row.insert("blob".into(), DbValue::Bytes(b"hello".to_vec()));
    row.insert(
        "doc".into(),
        DbValue::Json(serde_json::json!({"a": [1, 2]})),
    );
    row.insert("name".into(), DbValue::Text("plain text".into()));

    // No id: generated from the per-table counter; single insert returns id + record.
    let s = a.insert_rows(t, &[row.clone()], &[]).await.unwrap();
    assert_eq!(s.rows, 1);
    let Some(DbValue::Int(first_id)) = s.id else {
        panic!("generated id must be an Int, got {:?}", s.id)
    };
    assert_eq!(s.record.unwrap().get("id"), Some(&DbValue::Int(first_id)));

    // Null id is also replaced; bulk insert returns no id/record.
    row.insert("id".into(), DbValue::Null);
    let s = a.insert_rows(t, &[row.clone(), row], &[]).await.unwrap();
    assert_eq!(s.rows, 2);
    assert!(s.id.is_none() && s.record.is_none());

    let res = a
        .list(t, &FilterExpr::empty(), &[], None, 0, 10)
        .await
        .unwrap();
    assert_eq!(res.count, 3);
    let got = ids(&res.data);
    assert!(got.windows(2).all(|w| w[0] != w[1]), "ids collide: {got:?}");

    let r = res
        .data
        .iter()
        .find(|r| r["id"] == DbValue::Int(first_id))
        .unwrap();
    assert_eq!(r["nothing"], DbValue::Null);
    assert_eq!(r["flag"], DbValue::Bool(false));
    assert_eq!(r["ratio"], DbValue::Float(1.5));
    // Hash fields are type-tagged: the decimal comes back as a decimal, exactly.
    assert_eq!(r["money"].to_string(), "12.345678901234567890");
    assert!(matches!(r["money"], DbValue::Decimal(_)));
    assert_eq!(r["blob"], DbValue::Text("hello".into()));
    assert_eq!(r["doc"], DbValue::Json(serde_json::json!({"a": [1, 2]})));
    assert_eq!(r["name"], DbValue::Text("plain text".into()));
}

async fn seed_people(t: &str) -> RedisAdapter {
    let a = fresh(t).await;
    let people = [
        (1, "Alice", 30, 1.5, Some("x")),
        (2, "bob", 20, 2.5, None),
        (3, "Carol", 40, 0.5, Some("y")),
        (4, "dave", 10, 3.5, None),
    ];
    let rows: Vec<RowMap> = people
        .iter()
        .map(|(id, name, age, score, tag)| {
            let mut r = RowMap::new();
            r.insert("id".into(), DbValue::Int(*id));
            r.insert("name".into(), DbValue::Text((*name).into()));
            r.insert("age".into(), DbValue::Int(*age));
            r.insert("score".into(), DbValue::Float(*score));
            r.insert(
                "tag".into(),
                tag.map_or(DbValue::Null, |t| DbValue::Text(t.into())),
            );
            r
        })
        .collect();
    a.insert_rows(t, &rows, &[]).await.unwrap();
    a
}

async fn ids_for(a: &RedisAdapter, t: &str, f: FilterExpr) -> Vec<i128> {
    ids(&a.list(t, &f, &[], None, 0, 100).await.unwrap().data)
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_redis_cov_filter_operators() {
    use serde_json::json;
    use DbValue::{Float, Int, Json, Text};
    use Operator::{Eq, Gt, Gte, In, IsNotNull, IsNull, Like, Lt, Lte, NotEq, NotIn, NotLike};
    let t = "cov_redis_filters";
    let a = seed_people(t).await;
    let q = |c: Condition| ids_for(&a, t, one(c));

    assert_eq!(q(cond("age", NotEq, Int(30))).await, vec![2, 3, 4]);
    assert_eq!(q(cond("age", Gt, Int(20))).await, vec![1, 3]);
    assert_eq!(q(cond("age", Lt, Int(20))).await, vec![4]);
    assert_eq!(q(cond("age", Gte, Int(30))).await, vec![1, 3]);
    assert_eq!(q(cond("age", Lte, Int(20))).await, vec![2, 4]);
    assert_eq!(q(cond("score", Gt, Float(2.0))).await, vec![2, 4]);
    assert_eq!(q(cond("score", Lt, Float(1.0))).await, vec![3]);
    assert_eq!(q(cond("score", Gte, Float(2.5))).await, vec![2, 4]);
    assert_eq!(q(cond("score", Lte, Float(1.5))).await, vec![1, 3]);
    // Mixed Int/Float comparisons compare numerically (`?score=>2` parses as Int).
    assert_eq!(q(cond("score", Gt, Int(2))).await, vec![2, 4]);
    assert_eq!(q(cond("age", Lte, Float(20.0))).await, vec![2, 4]);
    // Text compares lexicographically; mismatched types never match.
    assert_eq!(q(cond("name", Gt, Text("a".into()))).await, vec![2, 4]);
    assert!(q(cond("name", Gt, Int(1))).await.is_empty());
    assert!(q(cond("age", Lt, Text("z".into()))).await.is_empty());

    // LIKE is a case-insensitive substring; non-text never matches LIKE, always NOT LIKE.
    assert_eq!(
        q(cond("name", Like, Text("%A%".into()))).await,
        vec![1, 3, 4]
    );
    assert_eq!(
        q(cond("name", NotLike, Text("%o%".into()))).await,
        vec![1, 4]
    );
    assert!(q(cond("age", Like, Text("%3%".into()))).await.is_empty());
    assert_eq!(
        q(cond("age", NotLike, Text("%3%".into()))).await,
        vec![1, 2, 3, 4]
    );

    // IN with strings, ints, unsupported items, and a non-array value.
    assert_eq!(
        q(cond("name", In, Json(json!(["bob", "dave"])))).await,
        vec![2, 4]
    );
    assert_eq!(q(cond("age", In, Json(json!([10, 40])))).await, vec![3, 4]);
    assert!(q(cond("age", In, Json(json!([true])))).await.is_empty());
    assert!(q(cond("age", In, Int(10))).await.is_empty());
    assert_eq!(
        q(cond("age", NotIn, Json(json!([10, 40])))).await,
        vec![1, 2]
    );

    // NULL handling: stored Null and missing columns.
    assert_eq!(q(cond("tag", IsNull, DbValue::Null)).await, vec![2, 4]);
    assert_eq!(q(cond("tag", IsNotNull, DbValue::Null)).await, vec![1, 3]);
    assert_eq!(
        q(cond("missing", IsNull, DbValue::Null)).await,
        vec![1, 2, 3, 4]
    );
    assert!(q(cond("missing", Eq, Int(1))).await.is_empty());

    // OR of AND-groups.
    let f = FilterExpr {
        groups: vec![
            vec![cond("age", Gte, Int(30)), cond("score", Lt, Float(1.0))],
            vec![cond("name", Eq, Text("bob".into()))],
        ],
    };
    assert_eq!(ids_for(&a, t, f).await, vec![2, 3]);
}

fn col(rows: &[RowMap], c: &str) -> Vec<DbValue> {
    rows.iter().map(|r| r[c].clone()).collect()
}

fn asc(c: &str) -> SortKey {
    SortKey {
        column: c.into(),
        direction: SortDirection::Asc,
    }
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_redis_cov_sort_and_paging() {
    let t = "cov_redis_sort";
    let a = seed_people(t).await;
    let all = FilterExpr::empty();

    let by_name = a.list(t, &all, &[asc("name")], None, 0, 10).await.unwrap();
    assert_eq!(
        col(&by_name.data, "name"),
        ["Alice", "Carol", "bob", "dave"].map(|s| DbValue::Text(s.into()))
    );
    let by_score = a.list(t, &all, &[asc("score")], None, 0, 10).await.unwrap();
    assert_eq!(
        col(&by_score.data, "score"),
        [0.5, 1.5, 2.5, 3.5].map(DbValue::Float)
    );

    // Paging: count is the total, data is one page.
    let page = a.list(t, &all, &[asc("age")], None, 1, 3).await.unwrap();
    assert_eq!(page.count, 4);
    assert_eq!(col(&page.data, "age"), vec![DbValue::Int(40)]);

    // get(): offset/limit, with and without total count.
    let opts = GetOptions {
        count: true,
        limit: Some(2),
        offset: Some(1),
    };
    let g = a.get(t, &all, &[asc("age")], None, opts).await.unwrap();
    assert_eq!(g.count, 4);
    assert_eq!(
        col(&g.data, "age"),
        vec![DbValue::Int(20), DbValue::Int(30)]
    );
    let opts = GetOptions {
        count: false,
        limit: Some(2),
        offset: Some(3),
    };
    let g = a.get(t, &all, &[asc("age")], None, opts).await.unwrap();
    assert_eq!(g.count, 1);
    assert_eq!(col(&g.data, "age"), vec![DbValue::Int(40)]);
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_redis_cov_upsert_and_soft_delete() {
    let t = "cov_redis_soft";
    let a = seed_people(t).await;
    let all = FilterExpr::empty();
    let unique = vec![vec!["id".to_string()]];

    // Upsert merges into the existing row; unmatched rows are inserted.
    let mut patch = RowMap::new();
    patch.insert("id".into(), DbValue::Int(1));
    patch.insert("age".into(), DbValue::Int(31));
    let mut new_row = RowMap::new();
    new_row.insert("id".into(), DbValue::Int(9));
    new_row.insert("name".into(), DbValue::Text("Zed".into()));
    let s = a.upsert_rows(t, &[patch, new_row], &unique).await.unwrap();
    assert_eq!(s.rows, 2);
    let r = a
        .get(
            t,
            &FilterExpr::eq("id", 1),
            &[],
            None,
            GetOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(r.data[0]["age"], DbValue::Int(31));
    assert_eq!(r.data[0]["name"], DbValue::Text("Alice".into()));
    assert_eq!(a.count(t, &all, None).await.unwrap(), 5);

    // Soft delete flags rows; they disappear from soft-delete-aware reads only.
    let young = one(cond("age", Operator::Lte, DbValue::Int(20)));
    let s = a.remove_rows(t, &young, Some("deleted")).await.unwrap();
    assert_eq!(s.rows, 2);
    assert_eq!(s.message, "2 cov_redis_softs removed");
    assert_eq!(a.count(t, &all, Some("deleted")).await.unwrap(), 3);
    assert_eq!(a.count(t, &all, None).await.unwrap(), 5);
    // Already-flagged rows are not counted again.
    let s = a.remove_rows(t, &all, Some("deleted")).await.unwrap();
    assert_eq!(s.rows, 3);
    assert_eq!(a.count(t, &all, Some("deleted")).await.unwrap(), 0);

    // Hard delete removes everything.
    let s = a.remove_rows(t, &all, None).await.unwrap();
    assert_eq!(s.rows, 5);
    assert_eq!(a.count(t, &all, None).await.unwrap(), 0);
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
async fn test_redis_update_where_and_ping() {
    let a = RedisAdapter::connect(&redis_url()).unwrap();
    update_where_contract(&*a, "cov_redis_update_where").await;
}
