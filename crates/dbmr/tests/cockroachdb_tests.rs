#![cfg(feature = "cockroachdb")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args,
    clippy::needless_raw_string_hashes
)]

use dbmr::adapters::CockroachAdapter;
use dbmr::core::adapter::DatabaseAdapter;
use dbmr::core::filter::{Condition, FilterExpr, Operator};
use dbmr::core::types::{DbValue, GetOptions, RowMap, SortDirection, SortKey};

mod common_sql;
use dbmr::core::adapter::PoolConfig;

async fn setup_cockroach() -> CockroachAdapter {
    let url = std::env::var("COCKROACH_URL").unwrap_or_else(|_| {
        "postgres://root@127.0.0.1:26257/defaultdb?sslmode=disable".to_string()
    });
    let adapter = CockroachAdapter::connect(&url)
        .await
        .expect("connect to cockroachdb");

    adapter
        .query(
            r#"
            CREATE TABLE IF NOT EXISTS test_users (
                id SERIAL PRIMARY KEY,
                name VARCHAR(255) NOT NULL,
                email VARCHAR(255) NOT NULL UNIQUE,
                age INT,
                active BOOLEAN DEFAULT TRUE,
                deleted_at BOOLEAN DEFAULT FALSE
            )
            "#,
            &[],
        )
        .await
        .expect("create table");

    adapter
        .query("TRUNCATE TABLE test_users", &[])
        .await
        .expect("truncate table");

    (*adapter).clone()
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_cockroach_crud() {
    let adapter = setup_cockroach().await;

    // Single insert
    let mut row = RowMap::new();
    row.insert("name".into(), DbValue::Text("Alice CRDB".into()));
    row.insert("email".into(), DbValue::Text("alice@crdb.com".into()));
    row.insert("age".into(), DbValue::Int(28));
    row.insert("active".into(), DbValue::Bool(true));
    row.insert("deleted_at".into(), DbValue::Bool(false));

    let summary = adapter
        .insert_rows("test_users", &[row], &[])
        .await
        .expect("crdb insert");
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
        &DbValue::Text("Alice CRDB".into())
    );

    // Bulk insert
    let mut bulk = Vec::new();
    for i in 1..=4 {
        let mut r = RowMap::new();
        r.insert("name".into(), DbValue::Text(format!("Bulk {i}")));
        r.insert("email".into(), DbValue::Text(format!("bulk{i}@crdb.com")));
        r.insert("age".into(), DbValue::Int(i * 10));
        r.insert("deleted_at".into(), DbValue::Bool(false));
        bulk.push(r);
    }
    let bulk_summary = adapter
        .insert_rows("test_users", &bulk, &[])
        .await
        .expect("bulk insert");
    assert_eq!(bulk_summary.rows, 4);
    assert_eq!(bulk_summary.message, "4 Test Userss are saved");

    // List
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

    // Upsert
    let mut upsert_row = RowMap::new();
    upsert_row.insert("name".into(), DbValue::Text("Alice Updated".into()));
    upsert_row.insert("email".into(), DbValue::Text("alice@crdb.com".into()));
    upsert_row.insert("age".into(), DbValue::Int(29));
    upsert_row.insert("deleted_at".into(), DbValue::Bool(false));

    let upsert_res = adapter
        .upsert_rows("test_users", &[upsert_row], &[vec!["email".to_string()]])
        .await
        .expect("upsert");
    assert_eq!(upsert_res.rows, 1);

    // Remove
    let del_summary = adapter
        .remove_rows("test_users", &filter, None)
        .await
        .expect("remove");
    assert_eq!(del_summary.rows, 1);
    assert_eq!(del_summary.message, "1 test_users removed");
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn cockroach_update_where_timestamps_tx_connect_with() {
    let a = CockroachAdapter::connect_with(
        &std::env::var("COCKROACH_URL").unwrap_or_else(|_| {
            "postgres://root@127.0.0.1:26257/defaultdb?sslmode=disable".to_string()
        }),
        PoolConfig {
            max_connections: 2,
            ..PoolConfig::default()
        },
    )
    .await
    .unwrap();
    let _ = a.query("DROP TABLE IF EXISTS uw_crdb", &[]).await;
    a.query("CREATE TABLE uw_crdb (id INT PRIMARY KEY, name TEXT, version INT, deleted INT DEFAULT 0, ts TIMESTAMPTZ, ts_text TEXT)", &[]).await.unwrap();
    common_sql::update_where_and_timestamps(a.as_ref(), "uw_crdb").await;
    common_sql::transactions(a.as_ref(), "uw_crdb").await;
    common_sql::concurrent_pings(a.as_ref()).await;
}
