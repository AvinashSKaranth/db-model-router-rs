#![cfg(feature = "mongodb")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args,
    clippy::many_single_char_names
)]

use dbmr::adapters::MongoAdapter;
use dbmr::core::adapter::DatabaseAdapter;
use dbmr::core::filter::{Condition, FilterExpr, Operator};
use dbmr::core::types::{DbValue, GetOptions, RowMap, SortDirection, SortKey};

async fn setup_mongo() -> MongoAdapter {
    let url =
        std::env::var("MONGO_URL").unwrap_or_else(|_| "mongodb://127.0.0.1:27017".to_string());
    let adapter = MongoAdapter::connect(&url, "test_db")
        .await
        .expect("connect to mongo");

    // Clean test collection
    let _ = adapter
        .remove_rows("test_users", &FilterExpr::empty(), None)
        .await;

    (*adapter).clone()
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_mongo_crud() {
    let adapter = setup_mongo().await;

    // Single insert
    let mut row = RowMap::new();
    row.insert("name".into(), DbValue::Text("Alice Mongo".into()));
    row.insert("email".into(), DbValue::Text("alice@mongo.com".into()));
    row.insert("age".into(), DbValue::Int(30));
    row.insert("active".into(), DbValue::Bool(true));

    let summary = adapter
        .insert_rows("test_users", &[row], &[])
        .await
        .expect("insert");
    assert_eq!(summary.rows, 1);
    assert_eq!(summary.message, "1 Test Users is saved");
    let id = summary.id.expect("inserted id");

    // Get
    let filter = FilterExpr {
        groups: vec![vec![Condition {
            column: "id".into(),
            operator: Operator::Eq,
            value: id.clone(),
        }]],
    };
    let res = adapter
        .get("test_users", &filter, &[], None, GetOptions::default())
        .await
        .expect("get");
    assert_eq!(res.data.len(), 1);
    assert_eq!(
        res.data[0].get("name").unwrap(),
        &DbValue::Text("Alice Mongo".into())
    );

    // Bulk insert
    let mut bulk = Vec::new();
    for i in 2..=5 {
        let mut r = RowMap::new();
        r.insert("name".into(), DbValue::Text(format!("Bulk {i}")));
        r.insert("email".into(), DbValue::Text(format!("bulk{i}@mongo.com")));
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

// ---- coverage: value round-trips, operators, upsert, soft delete, errors ----

fn mongo_url() -> String {
    std::env::var("MONGO_URL").unwrap_or_else(|_| "mongodb://127.0.0.1:27017".to_string())
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

async fn fresh(table: &str) -> MongoAdapter {
    let a = MongoAdapter::connect(&mongo_url(), "test_db")
        .await
        .unwrap();
    a.remove_rows(table, &FilterExpr::empty(), None)
        .await
        .unwrap();
    (*a).clone()
}

fn skus(rows: &[RowMap]) -> Vec<String> {
    let mut v: Vec<String> = rows
        .iter()
        .map(|r| r["sku"].as_str().unwrap().to_string())
        .collect();
    v.sort();
    v
}

async fn seed_people(t: &str) -> MongoAdapter {
    let a = fresh(t).await;
    let people = [
        ("p1", "Alice", 30, 1.5, Some("x")),
        ("p2", "bob", 20, 2.5, None),
        ("p3", "Carol", 40, 10.5, Some("y")),
        ("p4", "dave", 10, 3.5, None),
    ];
    let rows: Vec<RowMap> = people
        .iter()
        .map(|(sku, name, age, score, tag)| {
            let mut r = RowMap::new();
            r.insert("sku".into(), DbValue::Text((*sku).into()));
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

async fn skus_for(a: &MongoAdapter, t: &str, f: FilterExpr) -> Vec<String> {
    skus(
        &a.get(t, &f, &[], None, GetOptions::default())
            .await
            .unwrap()
            .data,
    )
}

#[tokio::test]
async fn test_mongo_invalid_url() {
    let Err(e) = MongoAdapter::connect("http://not-mongo", "test_db").await else {
        panic!("bad URL must fail")
    };
    assert!(e.to_string().contains("Invalid Mongo URL"), "{e}");
}

#[tokio::test]
async fn test_mongo_unreachable_server_errors() {
    let a = MongoAdapter::connect(
        "mongodb://127.0.0.1:1/?serverSelectionTimeoutMS=300&connectTimeoutMS=300",
        "test_db",
    )
    .await
    .unwrap();
    assert!(!a.is_connected().await);
    let all = FilterExpr::empty();
    let t = "cov_unreachable";
    let e = a.list(t, &all, &[], None, 0, 10).await.unwrap_err();
    assert!(e.to_string().contains("Mongo find error"), "{e}");
    let e = a.count(t, &all, None).await.unwrap_err();
    assert!(e.to_string().contains("Mongo count error"), "{e}");
    let mut r = RowMap::new();
    r.insert("sku".into(), DbValue::Text("s".into()));
    let e = a.insert_rows(t, &[r.clone()], &[]).await.unwrap_err();
    assert!(e.to_string().contains("Mongo insert error"), "{e}");
    let e = a
        .upsert_rows(t, &[r], &[vec!["sku".to_string()]])
        .await
        .unwrap_err();
    assert!(e.to_string().contains("Mongo upsert error"), "{e}");
    let e = a.remove_rows(t, &all, None).await.unwrap_err();
    assert!(e.to_string().contains("Mongo delete error"), "{e}");
    let e = a.remove_rows(t, &all, Some("deleted")).await.unwrap_err();
    assert!(e.to_string().contains("Mongo delete error"), "{e}");
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_mongo_cov_misc() {
    let client = mongodb::Client::with_uri_str(mongo_url()).await.unwrap();
    let a = MongoAdapter::new(client.database("test_db"));
    assert!(a.is_connected().await);
    a.disconnect().await.unwrap();
    assert_eq!(a.escape_id("x y"), "x y");
    let e = a.query("SELECT 1", &[]).await.unwrap_err();
    assert!(e.to_string().contains("not supported"), "{e}");

    let t = "cov_mongo_misc";
    assert_eq!(a.insert_rows(t, &[], &[]).await.unwrap().rows, 0);
    let s = a.upsert_rows(t, &[], &[]).await.unwrap();
    assert_eq!(s.rows, 0);
    assert_eq!(s.message, "0 Cov Mongo Miscs are saved");

    // Operator-looking field names are rejected everywhere.
    let bad = one(cond("$where", Operator::Eq, DbValue::Int(1)));
    assert!(matches!(
        a.count(t, &bad, None).await,
        Err(dbmr::core::error::AppError::UnprocessableEntity(_))
    ));
    let sort = [SortKey {
        column: "$x".into(),
        direction: SortDirection::Asc,
    }];
    assert!(a
        .list(t, &FilterExpr::empty(), &sort, None, 0, 1)
        .await
        .is_err());
    assert!(a
        .remove_rows(t, &FilterExpr::empty(), Some(""))
        .await
        .is_err());
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_mongo_cov_value_roundtrip() {
    let t = "cov_mongo_values";
    let a = fresh(t).await;

    let decimal =
        DbValue::from(serde_json::from_str::<serde_json::Value>("12.345678901234567890").unwrap());
    let huge = DbValue::Int(1_i128 << 70);
    let mut row = RowMap::new();
    row.insert("id".into(), DbValue::Text("custom-key".into()));
    row.insert("nothing".into(), DbValue::Null);
    row.insert("flag".into(), DbValue::Bool(true));
    row.insert("ratio".into(), DbValue::Float(1.5));
    row.insert("money".into(), decimal);
    row.insert("huge".into(), huge);
    row.insert("blob".into(), DbValue::Bytes(vec![0, 1, 2, 255]));
    row.insert(
        "doc".into(),
        DbValue::Json(serde_json::json!({"a": [1, 2], "b": "c"})),
    );

    let s = a.insert_rows(t, &[row], &[]).await.unwrap();
    assert_eq!(s.id, Some(DbValue::Text("custom-key".into())));

    let r = a
        .get(
            t,
            &FilterExpr::eq("id", "custom-key"),
            &[],
            None,
            GetOptions::default(),
        )
        .await
        .unwrap();
    let r = &r.data[0];
    assert_eq!(r["nothing"], DbValue::Null);
    assert_eq!(r["flag"], DbValue::Bool(true));
    assert_eq!(r["ratio"], DbValue::Float(1.5));
    // Values beyond BSON's native range are stored as strings, losslessly.
    assert_eq!(r["money"], DbValue::Text("12.345678901234567890".into()));
    assert_eq!(r["huge"], DbValue::Text((1_i128 << 70).to_string()));
    assert_eq!(r["blob"], DbValue::Bytes(vec![0, 1, 2, 255]));
    assert_eq!(
        r["doc"],
        DbValue::Json(serde_json::json!({"a": [1, 2], "b": "c"}))
    );
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_mongo_cov_foreign_bson_types() {
    // Documents written by another client: Int32 and nested types.
    let t = "cov_mongo_foreign";
    let a = fresh(t).await;
    let client = mongodb::Client::with_uri_str(mongo_url()).await.unwrap();
    client
        .database("test_db")
        .collection::<mongodb::bson::Document>(t)
        .insert_one(mongodb::bson::doc! { "sku": "f1", "small": 7_i32, "arr": [1_i32, "two"] })
        .await
        .unwrap();
    let r = a
        .get(t, &FilterExpr::empty(), &[], None, GetOptions::default())
        .await
        .unwrap();
    assert_eq!(r.data[0]["small"], DbValue::Int(7));
    assert_eq!(
        r.data[0]["arr"],
        DbValue::Json(serde_json::json!([1, "two"]))
    );
    // A generated ObjectId _id comes back as its hex string.
    assert!(matches!(&r.data[0]["id"], DbValue::Text(s) if s.len() == 24));
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_mongo_cov_filter_operators() {
    use serde_json::json;
    use DbValue::{Float, Int, Json, Text};
    use Operator::{Eq, Gt, Gte, In, IsNotNull, IsNull, Like, Lt, Lte, NotEq, NotIn, NotLike};
    let t = "cov_mongo_filters";
    let a = seed_people(t).await;
    let q = |c: Condition| skus_for(&a, t, one(c));
    let v = |s: &[&str]| s.iter().map(ToString::to_string).collect::<Vec<_>>();

    assert_eq!(q(cond("age", NotEq, Int(30))).await, v(&["p2", "p3", "p4"]));
    assert_eq!(q(cond("age", Gt, Int(20))).await, v(&["p1", "p3"]));
    assert_eq!(q(cond("age", Lt, Int(20))).await, v(&["p4"]));
    assert_eq!(q(cond("age", Gte, Int(30))).await, v(&["p1", "p3"]));
    assert_eq!(q(cond("age", Lte, Int(20))).await, v(&["p2", "p4"]));
    assert_eq!(q(cond("score", Gt, Int(9))).await, v(&["p3"]));
    assert_eq!(q(cond("age", Lt, Float(10.5))).await, v(&["p4"]));

    assert_eq!(
        q(cond("name", Like, Text("%A%".into()))).await,
        v(&["p1", "p3", "p4"])
    );
    assert_eq!(
        q(cond("name", Like, Text("c%r%l".into()))).await,
        v(&["p3"])
    );
    assert_eq!(
        q(cond("name", NotLike, Text("%o%".into()))).await,
        v(&["p1", "p4"])
    );
    // Regex metacharacters in the pattern are literal.
    assert!(q(cond("name", Like, Text("%.*%".into()))).await.is_empty());
    // A non-text LIKE pattern matches everything (empty regex).
    assert_eq!(q(cond("name", Like, Int(5))).await.len(), 4);

    assert_eq!(
        q(cond("name", In, Json(json!(["bob", "dave"])))).await,
        v(&["p2", "p4"])
    );
    assert_eq!(q(cond("age", In, Int(10))).await, v(&["p4"]));
    assert_eq!(
        q(cond("age", NotIn, Json(json!([10, 40])))).await,
        v(&["p1", "p2"])
    );

    assert_eq!(
        q(cond("tag", IsNull, DbValue::Null)).await,
        v(&["p2", "p4"])
    );
    assert_eq!(q(cond("tag", Eq, DbValue::Null)).await, v(&["p2", "p4"]));
    assert_eq!(
        q(cond("tag", IsNotNull, DbValue::Null)).await,
        v(&["p1", "p3"])
    );
    assert_eq!(q(cond("tag", NotEq, DbValue::Null)).await, v(&["p1", "p3"]));

    // OR of AND-groups, with and without soft delete; empty groups are ignored.
    let f = FilterExpr {
        groups: vec![
            vec![cond("age", Gte, Int(30)), cond("score", Gt, Float(5.0))],
            vec![],
            vec![cond("name", Eq, Text("bob".into()))],
        ],
    };
    assert_eq!(skus_for(&a, t, f.clone()).await, v(&["p2", "p3"]));
    let r = a
        .get(t, &f, &[], Some("deleted"), GetOptions::default())
        .await
        .unwrap();
    assert_eq!(skus(&r.data), v(&["p2", "p3"]));
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_mongo_cov_paging_upsert_soft_delete() {
    let t = "cov_mongo_soft";
    let a = seed_people(t).await;
    let all = FilterExpr::empty();
    let asc = |c: &str| SortKey {
        column: c.into(),
        direction: SortDirection::Asc,
    };

    // Empty sort column is ignored; get() with count reports the total.
    let opts = GetOptions {
        count: true,
        limit: Some(2),
        offset: Some(1),
    };
    let g = a
        .get(t, &all, &[asc(""), asc("age")], None, opts)
        .await
        .unwrap();
    assert_eq!(g.count, 4);
    assert_eq!(
        g.data.iter().map(|r| r["age"].clone()).collect::<Vec<_>>(),
        vec![DbValue::Int(20), DbValue::Int(30)]
    );

    // Upsert: matched target updates, unmatched inserts, incomplete target inserts.
    let unique = vec![vec!["sku".to_string()]];
    let mut patch = RowMap::new();
    patch.insert("sku".into(), DbValue::Text("p1".into()));
    patch.insert("age".into(), DbValue::Int(31));
    let mut fresh_row = RowMap::new();
    fresh_row.insert("sku".into(), DbValue::Text("p9".into()));
    fresh_row.insert("age".into(), DbValue::Int(99));
    let mut no_sku = RowMap::new();
    no_sku.insert("name".into(), DbValue::Text("anon".into()));
    let s = a
        .upsert_rows(t, &[patch, fresh_row, no_sku], &unique)
        .await
        .unwrap();
    assert_eq!(s.rows, 3);
    assert_eq!(a.count(t, &all, None).await.unwrap(), 6);
    let r = a
        .get(
            t,
            &FilterExpr::eq("sku", "p1"),
            &[],
            None,
            GetOptions::default(),
        )
        .await
        .unwrap();
    assert_eq!(r.data[0]["age"], DbValue::Int(31));
    assert_eq!(r.data[0]["name"], DbValue::Text("Alice".into()));

    // Soft delete sets the flag; live reads hide the rows.
    let young = one(cond("age", Operator::Lte, DbValue::Int(20)));
    let s = a.remove_rows(t, &young, Some("deleted")).await.unwrap();
    assert_eq!(s.rows, 2);
    assert_eq!(a.count(t, &all, Some("deleted")).await.unwrap(), 4);
    assert_eq!(a.count(t, &young, Some("deleted")).await.unwrap(), 0);
    assert_eq!(a.count(t, &all, None).await.unwrap(), 6);
    let page = a
        .list(t, &all, &[asc("age")], Some("deleted"), 0, 2)
        .await
        .unwrap();
    assert_eq!(page.count, 4);
    assert_eq!(page.data.len(), 2);

    let s = a.remove_rows(t, &all, None).await.unwrap();
    assert_eq!(s.rows, 6);
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
async fn test_mongo_update_where_and_ping() {
    let a = MongoAdapter::connect(&mongo_url(), "test_db")
        .await
        .unwrap();
    update_where_contract(&*a, "cov_mongo_update_where").await;
}
