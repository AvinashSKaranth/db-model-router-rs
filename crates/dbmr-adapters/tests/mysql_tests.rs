#![cfg(feature = "mysql")]
#![allow(
    clippy::unwrap_used,
    clippy::many_single_char_names,
    clippy::expect_used,
    clippy::uninlined_format_args,
    clippy::needless_raw_string_hashes
)]

use dbmr_adapters::MySqlAdapter;
use dbmr_core::adapter::DatabaseAdapter;
use dbmr_core::filter::{Condition, FilterExpr, Operator};
use dbmr_core::types::{DbValue, GetOptions, RowMap, SortDirection, SortKey};

mod common_sql;
use dbmr_core::adapter::PoolConfig;

async fn setup_mysql() -> MySqlAdapter {
    let url = std::env::var("MYSQL_URL")
        .unwrap_or_else(|_| "mysql://root:password@127.0.0.1:3308/test_db".to_string());
    let adapter = MySqlAdapter::connect(&url).await.expect("connect to mysql");

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
async fn test_mysql_crud() {
    let adapter = setup_mysql().await;

    // Single insert
    let mut row = RowMap::new();
    row.insert("name".into(), DbValue::Text("Alice MySQL".into()));
    row.insert("email".into(), DbValue::Text("alice@mysql.com".into()));
    row.insert("age".into(), DbValue::Int(28));
    row.insert("active".into(), DbValue::Bool(true));
    row.insert("deleted_at".into(), DbValue::Int(0));

    let summary = adapter
        .insert_rows("test_users", &[row], &[])
        .await
        .expect("mysql insert");
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
        &DbValue::Text("Alice MySQL".into())
    );

    // Bulk insert
    let mut bulk = Vec::new();
    for i in 1..=4 {
        let mut r = RowMap::new();
        r.insert("name".into(), DbValue::Text(format!("Bulk {i}")));
        r.insert("email".into(), DbValue::Text(format!("bulk{i}@mysql.com")));
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

    // Upsert (ON DUPLICATE KEY UPDATE)
    let mut upsert_row = RowMap::new();
    upsert_row.insert("name".into(), DbValue::Text("Alice Updated".into()));
    upsert_row.insert("email".into(), DbValue::Text("alice@mysql.com".into()));
    upsert_row.insert("age".into(), DbValue::Int(29));
    upsert_row.insert("deleted_at".into(), DbValue::Int(0));

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

    // Remove
    let del_summary = adapter
        .remove_rows("test_users", &filter, None)
        .await
        .expect("remove");
    assert_eq!(del_summary.rows, 1);
    assert_eq!(del_summary.message, "1 test_users removed");
}

// ---- coverage: type decoding, binds, error paths (own `cov_` tables) ----

fn mysql_url() -> String {
    std::env::var("MYSQL_URL")
        .unwrap_or_else(|_| "mysql://root:password@127.0.0.1:3308/test_db".to_string())
}

fn err<T>(r: dbmr_core::error::Result<T>) -> String {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => e.message().to_string(),
    }
}

#[tokio::test]
#[ignore = "requires live MySQL"]
async fn cov_mysql_decodes_every_column_type() {
    let a = MySqlAdapter::connect(&mysql_url()).await.unwrap();
    a.query("DROP TABLE IF EXISTS cov_mysql_types", &[])
        .await
        .unwrap();
    a.query(
        "CREATE TABLE cov_mysql_types (
            id INT AUTO_INCREMENT PRIMARY KEY,
            b BOOLEAN, t TINYINT, s SMALLINT, m MEDIUMINT, i INT, bi BIGINT,
            tu TINYINT UNSIGNED, iu INT UNSIGNED, bu BIGINT UNSIGNED,
            f FLOAT, d DOUBLE, dc DECIMAL(10,2), bl BLOB, vb VARBINARY(16), j JSON,
            txt VARCHAR(50), dt DATETIME, dd DATE, tm TIME
        ) ENGINE=InnoDB",
        &[],
    )
    .await
    .unwrap();

    let dec = rust_decimal::Decimal::new(12345, 2);
    let values = [
        ("b", DbValue::Bool(true)),
        ("t", DbValue::Int(-7)),
        ("s", DbValue::Int(-300)),
        ("m", DbValue::Int(70_000)),
        ("i", DbValue::Int(-2_000_000)),
        ("bi", DbValue::Int(9_000_000_000)),
        ("tu", DbValue::Int(200)),
        ("iu", DbValue::Int(4_000_000_000)),
        ("bu", DbValue::Int(10_000_000_000)),
        ("f", DbValue::Float(1.5)),
        ("d", DbValue::Float(2.25)),
        ("dc", DbValue::Decimal(dec)),
        ("bl", DbValue::Bytes(vec![0, 1, 255])),
        ("vb", DbValue::Bytes(vec![9, 8])),
        ("j", DbValue::Json(serde_json::json!({"k": [1, 2]}))),
        ("txt", DbValue::Text("hello".into())),
        ("dt", DbValue::Text("2024-01-02 03:04:05".into())),
        ("dd", DbValue::Text("2024-01-02".into())),
        ("tm", DbValue::Text("03:04:05".into())),
    ];
    let s = a
        .insert_rows(
            "cov_mysql_types",
            &[dbmr_core::types::row(values.clone())],
            &[],
        )
        .await
        .unwrap();
    let id = s.id.expect("auto-increment id");

    let r = a
        .get(
            "cov_mysql_types",
            &FilterExpr::eq("id", id),
            &[],
            None,
            GetOptions::default(),
        )
        .await
        .unwrap();
    let row = &r.data[0];
    for (k, v) in values {
        let v = match k {
            // DATETIME reads back as ISO-8601 UTC, like PostgreSQL TIMESTAMP.
            "dt" => DbValue::Text("2024-01-02T03:04:05.000Z".into()),
            _ => v,
        };
        assert_eq!(row.get(k), Some(&v), "column {k}");
    }

    // An all-NULL row decodes every column as Null (fallback decoders included).
    let s = a
        .insert_rows(
            "cov_mysql_types",
            &[dbmr_core::types::row([("txt", DbValue::Null)])],
            &[],
        )
        .await
        .unwrap();
    let r = a
        .get(
            "cov_mysql_types",
            &FilterExpr::eq("id", s.id.unwrap()),
            &[],
            None,
            GetOptions::default(),
        )
        .await
        .unwrap();
    for (k, v) in &r.data[0] {
        if k != "id" {
            assert_eq!(v, &DbValue::Null, "column {k}");
        }
    }
}

#[tokio::test]
#[ignore = "requires live MySQL"]
async fn cov_mysql_binds_options_and_errors() {
    let a = MySqlAdapter::connect(&mysql_url()).await.unwrap();
    a.query("DROP TABLE IF EXISTS cov_mysql_ops", &[])
        .await
        .unwrap();
    a.query(
        "CREATE TABLE cov_mysql_ops (
            id INT AUTO_INCREMENT PRIMARY KEY, code VARCHAR(20) NOT NULL UNIQUE,
            flag BOOLEAN, score DOUBLE, price DECIMAL(10,2), raw BLOB, meta JSON,
            deleted TINYINT NOT NULL DEFAULT 0)",
        &[],
    )
    .await
    .unwrap();

    // Empty writes are no-ops.
    let s = a.insert_rows("cov_mysql_ops", &[], &[]).await.unwrap();
    assert_eq!(
        (s.rows, s.message.as_str()),
        (0, "0 Cov Mysql Opss are saved")
    );
    let s = a.upsert_rows("cov_mysql_ops", &[], &[]).await.unwrap();
    assert_eq!(s.rows, 0);

    let price = rust_decimal::Decimal::new(999, 2);
    let mk = |code: &str, flag: bool, score: f64| {
        dbmr_core::types::row([
            ("code", DbValue::Text(code.into())),
            ("flag", DbValue::Bool(flag)),
            ("score", DbValue::Float(score)),
            ("price", DbValue::Decimal(price)),
            ("raw", DbValue::Bytes(vec![1, 2, 3])),
            ("meta", DbValue::Json(serde_json::json!({"a": 1}))),
        ])
    };
    let s = a
        .insert_rows(
            "cov_mysql_ops",
            &[mk("a", true, 1.5), mk("b", false, 2.5)],
            &[],
        )
        .await
        .unwrap();
    assert_eq!((s.rows, s.id), (2, None));

    // count binds every value kind through the scalar binder.
    let cnt = |f: FilterExpr| {
        let a = a.clone();
        async move { a.count("cov_mysql_ops", &f, None).await.unwrap() }
    };
    assert_eq!(cnt(FilterExpr::eq("flag", true)).await, 1);
    assert_eq!(cnt(FilterExpr::eq("score", 2.5)).await, 1);
    assert_eq!(cnt(FilterExpr::eq("price", price)).await, 2);
    assert_eq!(cnt(FilterExpr::eq("code", "a")).await, 1);
    assert_eq!(cnt(FilterExpr::eq("id", 0)).await, 0);
    assert_eq!(
        cnt(FilterExpr::eq("raw", DbValue::Bytes(vec![1, 2, 3]))).await,
        2
    );
    assert_eq!(
        cnt(FilterExpr::eq(
            "meta",
            DbValue::Json(serde_json::json!({"a": 1}))
        ))
        .await,
        0,
        "JSON = text compares as a string, never equal to a JSON document"
    );
    assert_eq!(
        cnt(FilterExpr::default().and("score", Operator::Gt, DbValue::Null)).await,
        0
    );

    // Raw query binds every value kind too.
    let rows = a
        .query(
            "SELECT ? AS n, ? AS b, ? AS f, ? AS d, ? AS t, ? AS by_, ? AS j, ? AS i",
            &[
                DbValue::Null,
                DbValue::Bool(true),
                DbValue::Float(0.5),
                DbValue::Decimal(price),
                DbValue::Text("x".into()),
                DbValue::Bytes(vec![7]),
                DbValue::Json(serde_json::json!([1])),
                DbValue::Int(42),
            ],
        )
        .await
        .unwrap();
    assert_eq!(rows[0].get("n"), Some(&DbValue::Null));
    assert_eq!(rows[0].get("t"), Some(&DbValue::Text("x".into())));
    assert_eq!(rows[0].get("i"), Some(&DbValue::Int(42)));
    assert_eq!(rows[0].get("f"), Some(&DbValue::Float(0.5)));

    // get: limit without offset, and count: true.
    let r = a
        .get(
            "cov_mysql_ops",
            &FilterExpr::empty(),
            &[SortKey {
                column: "code".into(),
                direction: SortDirection::Asc,
            }],
            None,
            GetOptions {
                count: true,
                limit: Some(1),
                offset: None,
            },
        )
        .await
        .unwrap();
    assert_eq!((r.data.len(), r.count), (1, 2));
    assert_eq!(r.data[0].get("code"), Some(&DbValue::Text("a".into())));

    // Upsert: a single new row reports its id; an existing key updates in place.
    let s = a
        .upsert_rows("cov_mysql_ops", &[mk("c", true, 3.0)], &[])
        .await
        .unwrap();
    assert!(matches!(s.id, Some(DbValue::Int(i)) if i > 0));
    let s = a
        .upsert_rows(
            "cov_mysql_ops",
            &[mk("a", false, 9.0), mk("b", true, 8.0)],
            &[],
        )
        .await
        .unwrap();
    assert_eq!((s.rows, s.id), (2, None));
    assert_eq!(cnt(FilterExpr::eq("score", 9.0)).await, 1);
    assert_eq!(cnt(FilterExpr::empty()).await, 3);

    // Soft delete hides the row from reads and counts.
    let s = a
        .remove_rows(
            "cov_mysql_ops",
            &FilterExpr::eq("code", "c"),
            Some("deleted"),
        )
        .await
        .unwrap();
    assert_eq!(s.rows, 1);
    assert_eq!(
        a.count("cov_mysql_ops", &FilterExpr::empty(), Some("deleted"))
            .await
            .unwrap(),
        2
    );

    // Error paths name the failing operation.
    let missing = "cov_mysql_missing";
    let f = FilterExpr::eq("id", 1);
    let row = dbmr_core::types::row([("id", DbValue::Int(1))]);
    assert!(
        err(a.get(missing, &f, &[], None, GetOptions::default()).await)
            .starts_with("MySQL query error")
    );
    assert!(err(a.count(missing, &f, None).await).starts_with("MySQL count error"));
    assert!(err(a
        .insert_rows(missing, std::slice::from_ref(&row), &[])
        .await)
    .starts_with("MySQL insert error"));
    assert!(err(a.upsert_rows(missing, &[row], &[]).await).starts_with("MySQL upsert error"));
    assert!(err(a.remove_rows(missing, &f, None).await).starts_with("MySQL remove error"));
    assert!(err(a.query("SELEC nonsense", &[]).await).starts_with("MySQL raw query error"));
    assert_eq!(a.escape_id("a`b"), "`a``b`");

    // Pool lifecycle.
    let pool = sqlx::MySqlPool::connect(&mysql_url()).await.unwrap();
    let b = MySqlAdapter::new(pool);
    assert!(b.is_connected().await);
    b.disconnect().await.unwrap();
    assert!(!b.is_connected().await);
    assert!(err(b.query("SELECT 1", &[]).await).starts_with("MySQL raw query error"));

    // Connection failures.
    assert!(err(MySqlAdapter::connect("not a url").await).starts_with("Invalid MySQL URL"));
    let bad = mysql_url().replace(":password@", ":wrong-password@");
    assert!(err(MySqlAdapter::connect(&bad).await).starts_with("Failed to connect to MySQL"));
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn mysql_update_where_timestamps_tx_connect_with() {
    let a = MySqlAdapter::connect_with(
        &mysql_url(),
        PoolConfig {
            max_connections: 2,
            ..PoolConfig::default()
        },
    )
    .await
    .unwrap();
    let _ = a.query("DROP TABLE IF EXISTS uw_mysql", &[]).await;
    a.query("CREATE TABLE uw_mysql (id INT PRIMARY KEY, name VARCHAR(50), version INT, deleted TINYINT DEFAULT 0, ts DATETIME(3), ts_text VARCHAR(50)) ENGINE=InnoDB", &[]).await.unwrap();
    common_sql::update_where_and_timestamps(a.as_ref(), "uw_mysql").await;
    common_sql::transactions(a.as_ref(), "uw_mysql").await;
    common_sql::concurrent_pings(a.as_ref()).await;
}
