#![cfg(feature = "sqlite")]
#![allow(
    clippy::unwrap_used,
    clippy::many_single_char_names,
    clippy::expect_used,
    clippy::uninlined_format_args,
    clippy::needless_raw_string_hashes
)]

use dbmr_adapters::SqliteAdapter;
use dbmr_core::adapter::DatabaseAdapter;
use dbmr_core::filter::{Condition, FilterExpr, Operator};
use dbmr_core::types::{DbValue, GetOptions, RowMap, SortDirection, SortKey};
use sqlx::SqlitePool;

mod common_sql;
use dbmr_core::adapter::PoolConfig;

async fn setup_db() -> SqliteAdapter {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("in-memory db");
    sqlx::query(
        r#"
        CREATE TABLE users (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            name TEXT NOT NULL,
            email TEXT NOT NULL UNIQUE,
            age INTEGER,
            active BOOLEAN DEFAULT 1
        );
        "#,
    )
    .execute(&pool)
    .await
    .expect("create table");

    SqliteAdapter::new(pool)
}

#[tokio::test]
async fn test_sqlite_insert_and_get() {
    let adapter = setup_db().await;

    let mut row = RowMap::new();
    row.insert("name".into(), DbValue::Text("Alice".into()));
    row.insert("email".into(), DbValue::Text("alice@example.com".into()));
    row.insert("age".into(), DbValue::Int(30));
    row.insert("active".into(), DbValue::Bool(true));

    let summary = adapter
        .insert_rows("users", &[row], &[])
        .await
        .expect("insert");
    assert_eq!(summary.rows, 1);
    assert_eq!(summary.message, "1 Users is saved");
    assert!(summary.id.is_some());
    let id = summary.id.unwrap();

    let filter = FilterExpr {
        groups: vec![vec![Condition {
            column: "id".into(),
            operator: Operator::Eq,
            value: id,
        }]],
    };

    let result = adapter
        .get("users", &filter, &[], None, GetOptions::default())
        .await
        .expect("get");
    assert_eq!(result.data.len(), 1);
    let fetched = &result.data[0];
    assert_eq!(fetched.get("name").unwrap(), &DbValue::Text("Alice".into()));
    assert_eq!(
        fetched.get("email").unwrap(),
        &DbValue::Text("alice@example.com".into())
    );
    assert_eq!(fetched.get("age").unwrap(), &DbValue::Int(30));
}

#[tokio::test]
async fn test_sqlite_bulk_insert_and_list() {
    let adapter = setup_db().await;

    let mut rows = Vec::new();
    for i in 1..=5 {
        let mut r = RowMap::new();
        r.insert("name".into(), DbValue::Text(format!("User {i}")));
        r.insert(
            "email".into(),
            DbValue::Text(format!("user{i}@example.com")),
        );
        r.insert("age".into(), DbValue::Int(i * 10));
        r.insert("active".into(), DbValue::Bool(true));
        rows.push(r);
    }

    let summary = adapter
        .insert_rows("users", &rows, &[])
        .await
        .expect("bulk insert");
    assert_eq!(summary.rows, 5);
    assert_eq!(summary.message, "5 Userss are saved");

    // List all
    let result = adapter
        .list("users", &FilterExpr::empty(), &[], None, 0, 10)
        .await
        .expect("list");
    assert_eq!(result.data.len(), 5);
    assert_eq!(result.count, 5);

    // List with filter age > 25
    let filter = FilterExpr {
        groups: vec![vec![Condition {
            column: "age".into(),
            operator: Operator::Gt,
            value: DbValue::Int(25),
        }]],
    };
    let sorted = vec![SortKey {
        column: "age".into(),
        direction: SortDirection::Desc,
    }];
    let filtered_res = adapter
        .list("users", &filter, &sorted, None, 0, 10)
        .await
        .expect("filtered list");
    assert_eq!(filtered_res.data.len(), 3);
    assert_eq!(filtered_res.data[0].get("age").unwrap(), &DbValue::Int(50));
    assert_eq!(filtered_res.data[1].get("age").unwrap(), &DbValue::Int(40));
    assert_eq!(filtered_res.data[2].get("age").unwrap(), &DbValue::Int(30));
}

#[tokio::test]
async fn test_sqlite_upsert_and_remove() {
    let adapter = setup_db().await;

    let mut row = RowMap::new();
    row.insert("name".into(), DbValue::Text("Bob".into()));
    row.insert("email".into(), DbValue::Text("bob@example.com".into()));
    row.insert("age".into(), DbValue::Int(25));
    let summary = adapter
        .insert_rows("users", &[row], &[])
        .await
        .expect("insert");
    let id = summary.id.unwrap();

    // Upsert (update on conflict email)
    let mut upsert_row = RowMap::new();
    upsert_row.insert("id".into(), id.clone());
    upsert_row.insert("name".into(), DbValue::Text("Bobby".into()));
    upsert_row.insert("email".into(), DbValue::Text("bob@example.com".into()));
    upsert_row.insert("age".into(), DbValue::Int(26));

    let upsert_res = adapter
        .upsert_rows("users", &[upsert_row], &[vec!["email".to_string()]])
        .await
        .expect("upsert");
    assert_eq!(upsert_res.rows, 1);

    let filter = FilterExpr {
        groups: vec![vec![Condition {
            column: "id".into(),
            operator: Operator::Eq,
            value: id.clone(),
        }]],
    };
    let fetched_res = adapter
        .get("users", &filter, &[], None, GetOptions::default())
        .await
        .expect("get");
    assert_eq!(
        fetched_res.data[0].get("name").unwrap(),
        &DbValue::Text("Bobby".into())
    );

    // Delete
    let del_summary = adapter
        .remove_rows("users", &filter, None)
        .await
        .expect("delete");
    assert_eq!(del_summary.rows, 1);
    assert_eq!(del_summary.message, "1 users removed");

    let deleted = adapter
        .get("users", &filter, &[], None, GetOptions::default())
        .await
        .expect("get");
    assert_eq!(deleted.data.len(), 0);
}

#[tokio::test]
async fn test_sqlite_transactions() {
    let adapter = setup_db().await;

    // Begin tx
    let tx = adapter.begin_tx().await.expect("begin_tx");

    tx.query(
        "INSERT INTO users (name, email, age) VALUES (?, ?, ?)",
        &[
            DbValue::Text("Charlie".into()),
            DbValue::Text("charlie@example.com".into()),
            DbValue::Int(40),
        ],
    )
    .await
    .expect("insert in tx");

    // Rollback
    tx.rollback().await.expect("rollback");

    // Verify row not committed
    let count = adapter
        .count("users", &FilterExpr::empty(), None)
        .await
        .expect("count");
    assert_eq!(count, 0);
}

// ---- coverage: decoding, binds, transactions, error paths ----

fn err<T>(r: dbmr_core::error::Result<T>) -> String {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => e.message().to_string(),
    }
}

#[tokio::test]
async fn cov_sqlite_decodes_and_binds_every_value_kind() {
    let a = SqliteAdapter::connect("sqlite::memory:").await.unwrap();
    a.query(
        "CREATE TABLE cov_types (id INTEGER PRIMARY KEY, b BOOLEAN, i INTEGER, f REAL,
         by BLOB, txt TEXT, dec TEXT, j TEXT, big TEXT)",
        &[],
    )
    .await
    .unwrap();
    let dec = rust_decimal::Decimal::new(12345, 2);
    let big = i128::from(i64::MAX) + 1;
    let s = a
        .insert_rows(
            "cov_types",
            &[dbmr_core::types::row([
                ("b", DbValue::Bool(true)),
                ("i", DbValue::Int(-5)),
                ("f", DbValue::Float(1.5)),
                ("by", DbValue::Bytes(vec![0, 255])),
                ("txt", DbValue::Text("hi".into())),
                ("dec", DbValue::Decimal(dec)),
                ("j", DbValue::Json(serde_json::json!({"k": 1}))),
                ("big", DbValue::Int(big)),
            ])],
            &[],
        )
        .await
        .unwrap();
    let rec = s.record.unwrap();
    assert_eq!(s.id, Some(DbValue::Int(1)));
    assert_eq!(rec.get("b"), Some(&DbValue::Bool(true)));
    assert_eq!(rec.get("i"), Some(&DbValue::Int(-5)));
    assert_eq!(rec.get("f"), Some(&DbValue::Float(1.5)));
    assert_eq!(rec.get("by"), Some(&DbValue::Bytes(vec![0, 255])));
    assert_eq!(rec.get("txt"), Some(&DbValue::Text("hi".into())));
    assert_eq!(rec.get("dec"), Some(&DbValue::Text("123.45".into())));
    assert_eq!(rec.get("j"), Some(&DbValue::Text(r#"{"k":1}"#.into())));
    assert_eq!(rec.get("big"), Some(&DbValue::Text(big.to_string())));

    let rows = a
        .query(
            "SELECT ? AS n, ? AS b, ? AS f, ? AS by",
            &[
                DbValue::Null,
                DbValue::Bool(false),
                DbValue::Float(0.25),
                DbValue::Bytes(vec![1]),
            ],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].get("n"), Some(&DbValue::Null));
    assert_eq!(rows[0].get("b"), Some(&DbValue::Int(0)));
    assert_eq!(rows[0].get("f"), Some(&DbValue::Float(0.25)));
    assert_eq!(rows[0].get("by"), Some(&DbValue::Bytes(vec![1])));

    let r = a
        .get(
            "cov_types",
            &FilterExpr::empty(),
            &[],
            None,
            GetOptions {
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(r.data.len(), 1);
    assert_eq!(a.escape_id("a\"b"), "\"a\"\"b\"");
}

#[tokio::test]
async fn cov_sqlite_file_db_transactions_and_errors() {
    use dbmr_core::adapter::TransactionAdapter;

    let path = std::env::temp_dir().join(format!("cov_sqlite_{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let url = format!("sqlite://{}", path.display().to_string().replace('\\', "/"));
    let a = SqliteAdapter::connect(&url).await.unwrap();
    a.query(
        "CREATE TABLE cov_ops (id INTEGER PRIMARY KEY, code TEXT NOT NULL UNIQUE, score REAL)",
        &[],
    )
    .await
    .unwrap();
    let t = "cov_ops";
    let mk = |code: &str| dbmr_core::types::row([("code", DbValue::Text(code.into()))]);

    a.insert_rows(t, &[mk("a"), mk("b")], &[]).await.unwrap();
    // Upsert where every column is the conflict key: DO NOTHING.
    let s = a
        .upsert_rows(t, &[mk("a")], &[vec!["code".into()]])
        .await
        .unwrap();
    assert_eq!((s.rows, s.record), (1, None));
    // A failing bulk write rolls back entirely.
    let e = err(a.insert_rows(t, &[mk("c"), mk("a")], &[]).await);
    assert!(e.starts_with("SQLite query error"), "{e}");
    assert_eq!(a.count(t, &FilterExpr::empty(), None).await.unwrap(), 2);

    // Pool-level commit/rollback are no-ops.
    a.commit().await.unwrap();
    a.rollback().await.unwrap();

    let tx = a.begin_tx().await.unwrap();
    assert!(tx.is_connected().await);
    assert_eq!(
        tx.insert_rows(t, &[mk("t1"), mk("t2")], &[])
            .await
            .unwrap()
            .rows,
        2
    );
    assert_eq!(
        tx.remove_rows(t, &FilterExpr::eq("code", "t2"), None)
            .await
            .unwrap()
            .rows,
        1
    );
    let e = err(tx.begin_tx().await);
    assert!(e.starts_with("Transaction already open"), "{e}");
    tx.commit().await.unwrap();
    assert!(!tx.is_connected().await);
    assert_eq!(
        err(tx.query("SELECT 1", &[]).await),
        "Transaction already closed"
    );
    assert_eq!(
        err(tx.remove_rows(t, &FilterExpr::eq("code", "t1"), None).await),
        "Transaction already closed"
    );
    tx.rollback().await.unwrap();
    assert!(tx.clone().as_adapter().escape_id("x").starts_with('"'));
    assert_eq!(a.count(t, &FilterExpr::empty(), None).await.unwrap(), 3);

    // Commit/rollback fail once the transaction was ended behind the adapter's back.
    let tx = a.begin_tx().await.unwrap();
    tx.query("ROLLBACK", &[]).await.unwrap();
    let e = err(tx.commit().await);
    assert!(e.starts_with("SQLite transaction commit error"), "{e}");
    let tx = a.begin_tx().await.unwrap();
    tx.query("ROLLBACK", &[]).await.unwrap();
    let e = err(tx.rollback().await);
    assert!(e.starts_with("SQLite transaction rollback error"), "{e}");

    let e = err(a
        .remove_rows("cov_missing", &FilterExpr::empty(), None)
        .await);
    assert!(e.starts_with("SQLite query error"), "{e}");

    a.disconnect().await.unwrap();
    assert!(!a.is_connected().await);
    let e = err(a.begin_tx().await);
    assert!(e.starts_with("SQLite begin transaction error"), "{e}");
    let _ = std::fs::remove_file(&path);

    let e = err(SqliteAdapter::connect("sqlite:x.db?mode=bogus").await);
    assert!(e.starts_with("Invalid SQLite URL"), "{e}");
    let e = err(SqliteAdapter::connect("sqlite:///cov_no_such_dir/x/y.db").await);
    assert!(e.starts_with("Failed to connect to SQLite"), "{e}");
}

#[tokio::test]
async fn sqlite_update_where_timestamps_tx_connect_with() {
    let a = SqliteAdapter::connect_with(
        "sqlite::memory:",
        PoolConfig {
            max_connections: 2,
            ..PoolConfig::default()
        },
    )
    .await
    .unwrap();
    let _ = a.query("DROP TABLE IF EXISTS uw_sqlite", &[]).await;
    a.query("CREATE TABLE uw_sqlite (id INT PRIMARY KEY, name TEXT, version INT, deleted INTEGER DEFAULT 0, ts TEXT, ts_text TEXT)", &[]).await.unwrap();
    common_sql::update_where_and_timestamps(a.as_ref(), "uw_sqlite").await;
    common_sql::transactions(a.as_ref(), "uw_sqlite").await;
    common_sql::concurrent_pings(a.as_ref()).await;
}
