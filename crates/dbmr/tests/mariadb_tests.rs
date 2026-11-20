#![cfg(feature = "mariadb")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args,
    clippy::needless_raw_string_hashes
)]

use dbmr::adapters::MariaDbAdapter;
use dbmr::core::adapter::DatabaseAdapter;
use dbmr::core::filter::{Condition, FilterExpr, Operator};
use dbmr::core::types::{DbValue, GetOptions, RowMap, SortDirection, SortKey};

mod common_sql;
use dbmr::core::adapter::PoolConfig;

async fn setup_mariadb() -> MariaDbAdapter {
    let url = std::env::var("MARIADB_URL")
        .unwrap_or_else(|_| "mysql://root:password@127.0.0.1:3309/test_db".to_string());
    let adapter = MariaDbAdapter::connect(&url)
        .await
        .expect("connect to mariadb");

    adapter
        .query(
            r#"
            CREATE TABLE IF NOT EXISTS test_users (
                id INT AUTO_INCREMENT PRIMARY KEY,
                name VARCHAR(255) NOT NULL,
                email VARCHAR(255) NOT NULL UNIQUE,
                age INT,
                active BOOLEAN DEFAULT TRUE,
                deleted_at TINYINT DEFAULT 0
            ) ENGINE=InnoDB
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
async fn test_mariadb_crud() {
    let adapter = setup_mariadb().await;

    // Single insert
    let mut row = RowMap::new();
    row.insert("name".into(), DbValue::Text("Alice MariaDB".into()));
    row.insert("email".into(), DbValue::Text("alice@mariadb.com".into()));
    row.insert("age".into(), DbValue::Int(28));
    row.insert("active".into(), DbValue::Bool(true));
    row.insert("deleted_at".into(), DbValue::Int(0));

    let summary = adapter
        .insert_rows("test_users", &[row], &[])
        .await
        .expect("mariadb insert");
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
        &DbValue::Text("Alice MariaDB".into())
    );

    // Bulk insert
    let mut bulk = Vec::new();
    for i in 1..=4 {
        let mut r = RowMap::new();
        r.insert("name".into(), DbValue::Text(format!("Bulk {i}")));
        r.insert(
            "email".into(),
            DbValue::Text(format!("bulk{i}@mariadb.com")),
        );
        r.insert("age".into(), DbValue::Int(i * 10));
        r.insert("deleted_at".into(), DbValue::Int(0));
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
    upsert_row.insert("email".into(), DbValue::Text("alice@mariadb.com".into()));
    upsert_row.insert("age".into(), DbValue::Int(29));
    upsert_row.insert("deleted_at".into(), DbValue::Int(0));

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
async fn mariadb_update_where_timestamps_tx_connect_with() {
    let a = MariaDbAdapter::connect_with(
        &std::env::var("MARIADB_URL")
            .unwrap_or_else(|_| "mysql://root:password@127.0.0.1:3309/test_db".to_string()),
        PoolConfig {
            max_connections: 2,
            ..PoolConfig::default()
        },
    )
    .await
    .unwrap();
    let _ = a.query("DROP TABLE IF EXISTS uw_mariadb", &[]).await;
    a.query("CREATE TABLE uw_mariadb (id INT PRIMARY KEY, name VARCHAR(50), version INT, deleted TINYINT DEFAULT 0, ts DATETIME(3), ts_text VARCHAR(50)) ENGINE=InnoDB", &[]).await.unwrap();
    common_sql::update_where_and_timestamps(a.as_ref(), "uw_mariadb").await;
    common_sql::transactions(a.as_ref(), "uw_mariadb").await;
    common_sql::concurrent_pings(a.as_ref()).await;
}
