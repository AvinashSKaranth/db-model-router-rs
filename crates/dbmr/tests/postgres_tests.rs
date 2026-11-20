#![cfg(feature = "postgres")]
#![allow(
    clippy::unwrap_used,
    clippy::many_single_char_names,
    clippy::expect_used,
    clippy::uninlined_format_args,
    clippy::needless_raw_string_hashes
)]

use dbmr::adapters::PostgresAdapter;
use dbmr::core::adapter::DatabaseAdapter;
use dbmr::core::filter::{Condition, FilterExpr, Operator};
use dbmr::core::types::{DbValue, GetOptions, RowMap, SortDirection, SortKey};

mod common_sql;
use dbmr::core::adapter::PoolConfig;

async fn setup_pg() -> PostgresAdapter {
    let url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:password@127.0.0.1:5432/test_db".to_string());
    let adapter = PostgresAdapter::connect(&url).await.expect("connect to pg");

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
        .query("TRUNCATE TABLE test_users RESTART IDENTITY", &[])
        .await
        .expect("truncate table");

    (*adapter).clone()
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_postgres_crud_and_transactions() {
    let adapter = setup_pg().await;

    // Single insert
    let mut row = RowMap::new();
    row.insert("name".into(), DbValue::Text("Alice PG".into()));
    row.insert("email".into(), DbValue::Text("alice@pg.com".into()));
    row.insert("age".into(), DbValue::Int(28));
    row.insert("active".into(), DbValue::Bool(true));
    row.insert("deleted_at".into(), DbValue::Bool(false));

    let summary = adapter
        .insert_rows("test_users", &[row], &[])
        .await
        .expect("pg insert");
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
        &DbValue::Text("Alice PG".into())
    );

    // Bulk insert
    let mut bulk = Vec::new();
    for i in 1..=4 {
        let mut r = RowMap::new();
        r.insert("name".into(), DbValue::Text(format!("Bulk {i}")));
        r.insert("email".into(), DbValue::Text(format!("bulk{i}@pg.com")));
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

    // List with sorting and paging (0-indexed)
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

    // Upsert (ON CONFLICT on email)
    let mut upsert_row = RowMap::new();
    upsert_row.insert("name".into(), DbValue::Text("Alice Updated".into()));
    upsert_row.insert("email".into(), DbValue::Text("alice@pg.com".into()));
    upsert_row.insert("age".into(), DbValue::Int(29));
    upsert_row.insert("deleted_at".into(), DbValue::Bool(false));

    let upsert_res = adapter
        .upsert_rows("test_users", &[upsert_row], &[vec!["email".to_string()]])
        .await
        .expect("upsert");
    assert_eq!(upsert_res.rows, 1);

    let updated_res = adapter
        .get("test_users", &filter, &[], None, GetOptions::default())
        .await
        .expect("get");
    assert_eq!(
        updated_res.data[0].get("name").unwrap(),
        &DbValue::Text("Alice Updated".into())
    );

    // Transaction Rollback
    let tx = adapter.begin_tx().await.expect("begin_tx");
    tx.query(
        "INSERT INTO test_users (name, email, age, deleted_at) VALUES ($1, $2, $3, $4)",
        &[
            DbValue::Text("Ghost".into()),
            DbValue::Text("ghost@pg.com".into()),
            DbValue::Int(99),
            DbValue::Bool(false),
        ],
    )
    .await
    .expect("tx query");
    tx.rollback().await.expect("tx rollback");

    let count_after_rollback = adapter
        .count("test_users", &FilterExpr::empty(), None)
        .await
        .expect("count");
    assert_eq!(count_after_rollback, 5);

    // Remove
    let del_summary = adapter
        .remove_rows("test_users", &filter, None)
        .await
        .expect("remove");
    assert_eq!(del_summary.rows, 1);
    assert_eq!(del_summary.message, "1 test_users removed");
}

// ---- coverage: type decoding, binds, transactions, error paths (own `cov_` tables) ----

fn pg_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:password@127.0.0.1:5432/test_db".to_string())
}

fn err<T>(r: dbmr::core::error::Result<T>) -> String {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => e.message().to_string(),
    }
}

#[tokio::test]
#[ignore = "requires live PostgreSQL"]
async fn cov_pg_decodes_every_column_type() {
    let a = PostgresAdapter::connect(&pg_url()).await.unwrap();
    a.query("DROP TABLE IF EXISTS cov_pg_types", &[])
        .await
        .unwrap();
    a.query(
        "CREATE TABLE cov_pg_types (
            id SERIAL PRIMARY KEY, b BOOLEAN, i2 SMALLINT, i4 INTEGER, i8 BIGINT, f4 REAL,
            f8 DOUBLE PRECISION, n NUMERIC(10,2), big NUMERIC(30,0), by BYTEA, j JSON, jb JSONB,
            tz TIMESTAMPTZ, ts TIMESTAMP, d DATE, t TIME, u UUID, txt TEXT, ip INET)",
        &[],
    )
    .await
    .unwrap();

    let dec = rust_decimal::Decimal::new(12345, 2);
    let big = i128::from(i64::MAX) + 1;
    let uuid = "6f9619ff-8b86-d011-b42d-00c04fc964ff";
    let values = [
        // Integers bound to a boolean column become booleans.
        ("b", DbValue::Int(1)),
        ("i2", DbValue::Int(-300)),
        ("i4", DbValue::Int(-2_000_000)),
        ("i8", DbValue::Int(9_000_000_000)),
        ("f4", DbValue::Float(1.5)),
        ("f8", DbValue::Float(2.25)),
        ("n", DbValue::Decimal(dec)),
        // Beyond i64: bound as text and cast to the column type.
        ("big", DbValue::Int(big)),
        ("by", DbValue::Bytes(vec![0, 1, 255])),
        ("j", DbValue::Json(serde_json::json!({"k": [1, 2]}))),
        ("jb", DbValue::Json(serde_json::json!({"x": true}))),
        ("tz", DbValue::Text("2024-01-02T03:04:05.678Z".into())),
        ("ts", DbValue::Text("2024-01-02 03:04:05".into())),
        ("d", DbValue::Text("2024-01-02".into())),
        ("t", DbValue::Text("03:04:05".into())),
        ("u", DbValue::Text(uuid.into())),
        ("txt", DbValue::Text("hello".into())),
        ("ip", DbValue::Text("10.0.0.1".into())),
    ];
    let s = a
        .insert_rows(
            "cov_pg_types",
            &[dbmr::core::types::row(values.clone())],
            &[],
        )
        .await
        .unwrap();
    let rec = s.record.expect("single insert returns the stored record");
    assert_eq!(s.id, rec.get("id").cloned());

    for (k, v) in values {
        let v = match k {
            "b" => DbValue::Bool(true),
            "big" => DbValue::Decimal(rust_decimal::Decimal::from_i128_with_scale(big, 0)),
            "ts" => DbValue::Text("2024-01-02T03:04:05.000Z".into()),
            // ponytail-documented: exotic types without a text decoder read as Null.
            "ip" => DbValue::Null,
            _ => v,
        };
        assert_eq!(rec.get(k), Some(&v), "column {k}");
    }

    let s = a
        .insert_rows(
            "cov_pg_types",
            &[dbmr::core::types::row([("txt", DbValue::Null)])],
            &[],
        )
        .await
        .unwrap();
    for (k, v) in &s.record.unwrap() {
        if k != "id" {
            assert_eq!(v, &DbValue::Null, "column {k}");
        }
    }
}

#[tokio::test]
#[ignore = "requires live PostgreSQL"]
async fn cov_pg_writes_transactions_and_errors() {
    use dbmr::core::adapter::TransactionAdapter;

    let a = PostgresAdapter::connect(&pg_url()).await.unwrap();
    a.query("DROP TABLE IF EXISTS cov_pg_ops", &[])
        .await
        .unwrap();
    a.query(
        "CREATE TABLE cov_pg_ops (
            id SERIAL PRIMARY KEY, code TEXT NOT NULL UNIQUE, score DOUBLE PRECISION,
            gone BOOLEAN NOT NULL DEFAULT FALSE, del INTEGER NOT NULL DEFAULT 0)",
        &[],
    )
    .await
    .unwrap();
    let t = "cov_pg_ops";
    let mk = |code: &str| dbmr::core::types::row([("code", DbValue::Text(code.into()))]);

    // Rows with different key sets are written one by one.
    let mut b = mk("b");
    b.insert("score".into(), DbValue::Float(2.0));
    let s = a.insert_rows(t, &[mk("a"), b], &[]).await.unwrap();
    assert_eq!((s.rows, s.record), (2, None));

    // Upsert where every column is the conflict key: DO NOTHING.
    let s = a
        .upsert_rows(t, &[mk("a")], &[vec!["code".into()]])
        .await
        .unwrap();
    assert_eq!((s.rows, s.record), (1, None));
    assert_eq!(a.count(t, &FilterExpr::empty(), None).await.unwrap(), 2);

    // A failing bulk write rolls back entirely.
    let e = err(a.insert_rows(t, &[mk("c"), mk("a")], &[]).await);
    assert!(e.starts_with("PostgreSQL query error"), "{e}");
    assert_eq!(
        a.count(t, &FilterExpr::eq("code", "c"), None)
            .await
            .unwrap(),
        0
    );

    // get with limit only.
    let r = a
        .get(
            t,
            &FilterExpr::empty(),
            &[SortKey {
                column: "code".into(),
                direction: SortDirection::Desc,
            }],
            None,
            GetOptions {
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(r.data[0].get("code"), Some(&DbValue::Text("b".into())));

    // Soft delete on a boolean flag (TRUE) and on an integer flag (1).
    let s = a
        .remove_rows(t, &FilterExpr::eq("code", "a"), Some("gone"))
        .await
        .unwrap();
    assert_eq!(s.rows, 1);
    assert_eq!(
        a.count(t, &FilterExpr::empty(), Some("gone"))
            .await
            .unwrap(),
        1
    );
    let s = a
        .remove_rows(t, &FilterExpr::eq("code", "b"), Some("del"))
        .await
        .unwrap();
    assert_eq!(s.rows, 1);
    assert_eq!(
        a.count(t, &FilterExpr::empty(), Some("del")).await.unwrap(),
        1
    );

    // Pool-level commit/rollback are no-ops.
    a.commit().await.unwrap();
    a.rollback().await.unwrap();

    // Transaction scope: bulk writes, reads and removes run on the tx connection.
    let tx = a.begin_tx().await.unwrap();
    assert!(tx.is_connected().await);
    let s = tx.insert_rows(t, &[mk("t1"), mk("t2")], &[]).await.unwrap();
    assert_eq!(s.rows, 2);
    assert_eq!(tx.count(t, &FilterExpr::empty(), None).await.unwrap(), 4);
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
    // A finished transaction refuses further work; commit/rollback stay idempotent.
    assert_eq!(
        err(tx.query("SELECT 1", &[]).await),
        "Transaction already closed"
    );
    assert_eq!(
        err(tx.remove_rows(t, &FilterExpr::eq("code", "t1"), None).await),
        "Transaction already closed"
    );
    tx.commit().await.unwrap();
    tx.rollback().await.unwrap();
    let as_db = tx.clone().as_adapter();
    assert_eq!(as_db.escape_id("a\"b"), "\"a\"\"b\"");
    assert_eq!(
        a.count(t, &FilterExpr::eq("code", "t1"), None)
            .await
            .unwrap(),
        1
    );

    // Commit fails when a deferred constraint is violated.
    a.query("DROP TABLE IF EXISTS cov_pg_deferred", &[])
        .await
        .unwrap();
    a.query(
        "CREATE TABLE cov_pg_deferred (k INT, CONSTRAINT cov_pg_deferred_uq UNIQUE (k) \
         DEFERRABLE INITIALLY DEFERRED)",
        &[],
    )
    .await
    .unwrap();
    let tx = a.begin_tx().await.unwrap();
    tx.query("INSERT INTO cov_pg_deferred VALUES (1), (1)", &[])
        .await
        .unwrap();
    let e = err(tx.commit().await);
    assert!(e.starts_with("PostgreSQL transaction commit error"), "{e}");

    // Rollback fails when the server has dropped the session.
    let tx = a.begin_tx().await.unwrap();
    let _ = tx
        .query("SELECT pg_terminate_backend(pg_backend_pid())", &[])
        .await;
    let e = err(tx.rollback().await);
    assert!(
        e.starts_with("PostgreSQL transaction rollback error"),
        "{e}"
    );

    // Errors and lifecycle.
    let e = err(a.query("SELECT * FROM cov_pg_missing", &[]).await);
    assert!(e.starts_with("PostgreSQL query error"), "{e}");
    let e = err(a.remove_rows(t, &FilterExpr::eq("nope", 1), None).await);
    assert!(e.starts_with("PostgreSQL query error"), "{e}");
    let pool = sqlx::PgPool::connect(&pg_url()).await.unwrap();
    let b = PostgresAdapter::new(pool);
    assert!(b.is_connected().await);
    b.disconnect().await.unwrap();
    assert!(!b.is_connected().await);
    let e = err(b.begin_tx().await);
    assert!(e.starts_with("PostgreSQL begin transaction error"), "{e}");
    let e = err(PostgresAdapter::connect("not a url").await);
    assert!(e.starts_with("Invalid PostgreSQL URL"), "{e}");
    let e = err(PostgresAdapter::connect(&pg_url().replace(":password@", ":wrong@")).await);
    assert!(e.starts_with("Failed to connect to PostgreSQL"), "{e}");
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn pg_update_where_timestamps_tx_connect_with() {
    let a = PostgresAdapter::connect_with(
        &pg_url(),
        PoolConfig {
            max_connections: 2,
            ..PoolConfig::default()
        },
    )
    .await
    .unwrap();
    let _ = a.query("DROP TABLE IF EXISTS uw_pg_tz", &[]).await;
    a.query("CREATE TABLE uw_pg_tz (id INT PRIMARY KEY, name TEXT, version INT, deleted INT DEFAULT 0, ts TIMESTAMPTZ, ts_text TEXT)", &[]).await.unwrap();
    common_sql::update_where_and_timestamps(a.as_ref(), "uw_pg_tz").await;
    let _ = a.query("DROP TABLE IF EXISTS uw_pg_naive", &[]).await;
    a.query("CREATE TABLE uw_pg_naive (id INT PRIMARY KEY, name TEXT, version INT, deleted INT DEFAULT 0, ts TIMESTAMP(3), ts_text TEXT)", &[]).await.unwrap();
    common_sql::update_where_and_timestamps(a.as_ref(), "uw_pg_naive").await;
    common_sql::transactions(a.as_ref(), "uw_pg_tz").await;
    common_sql::concurrent_pings(a.as_ref()).await;
}
