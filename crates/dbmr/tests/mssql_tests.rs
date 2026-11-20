#![cfg(feature = "mssql")]
#![allow(
    clippy::unwrap_used,
    clippy::many_single_char_names,
    clippy::expect_used,
    clippy::uninlined_format_args,
    clippy::needless_raw_string_hashes
)]

use compact_str::CompactString;
use dbmr::adapters::MssqlAdapter;
use dbmr::core::adapter::DatabaseAdapter;
use dbmr::core::filter::{Condition, FilterExpr, Operator};
use dbmr::core::types::{DbValue, GetOptions, RowMap, SortDirection, SortKey};

mod common_sql;
use dbmr::core::adapter::PoolConfig;

async fn get_mssql() -> Option<std::sync::Arc<MssqlAdapter>> {
    match MssqlAdapter::connect_host("127.0.0.1", 1433, "sa", "Password123!", "master", true).await
    {
        Ok(a) => Some(a),
        Err(e) => {
            panic!("Live MSSQL container not reachable: {e}")
        }
    }
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_mssql_crud() {
    let Some(adapter) = get_mssql().await else {
        return;
    };

    // 1. Create table
    adapter
        .query(
            r"
            IF OBJECT_ID('test_mssql_users', 'U') IS NOT NULL DROP TABLE test_mssql_users;
            CREATE TABLE test_mssql_users (
                id INT IDENTITY(1,1) PRIMARY KEY,
                name NVARCHAR(255) NOT NULL,
                age INT NOT NULL,
                is_active BIT DEFAULT 1,
                deleted INT DEFAULT 0
            );
            ",
            &[],
        )
        .await
        .unwrap();

    // 2. Insert rows
    let mut row1 = RowMap::new();
    row1.insert(
        CompactString::new("name"),
        DbValue::Text("Alice".to_string()),
    );
    row1.insert(CompactString::new("age"), DbValue::Int(30));
    row1.insert(CompactString::new("is_active"), DbValue::Bool(true));

    let mut row2 = RowMap::new();
    row2.insert(CompactString::new("name"), DbValue::Text("Bob".to_string()));
    row2.insert(CompactString::new("age"), DbValue::Int(25));
    row2.insert(CompactString::new("is_active"), DbValue::Bool(false));

    let summary = adapter
        .insert_rows("test_mssql_users", &[row1, row2], &[])
        .await
        .unwrap();
    assert_eq!(summary.rows, 2);

    // 3. Count
    let total = adapter
        .count("test_mssql_users", &FilterExpr::default(), None)
        .await
        .unwrap();
    assert_eq!(total, 2);

    // 4. Query with filter and sort
    let mut filter = FilterExpr::default();
    filter.groups.push(vec![Condition {
        column: CompactString::new("name"),
        operator: Operator::Eq,
        value: DbValue::Text("Alice".to_string()),
    }]);

    let res = adapter
        .get(
            "test_mssql_users",
            &filter,
            &[SortKey {
                column: CompactString::new("age"),
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

    // 6. Remove rows with safe delete
    let remove_summary = adapter
        .remove_rows("test_mssql_users", &filter, Some("deleted"))
        .await
        .unwrap();
    assert_eq!(remove_summary.rows, 1);

    // Active count with safe_delete
    let active_count = adapter
        .count("test_mssql_users", &FilterExpr::default(), Some("deleted"))
        .await
        .unwrap();
    assert_eq!(active_count, 1); // Only Bob remains active
}

/// S5: without `trust_cert` the container's self-signed certificate is rejected.
#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn test_mssql_untrusted_cert_is_rejected() {
    let res =
        MssqlAdapter::connect_host("127.0.0.1", 1433, "sa", "Password123!", "master", false).await;
    assert!(
        res.is_err(),
        "self-signed certificate must not be trusted by default"
    );
}

// ---- coverage: type decoding, binds, upsert paths, error paths (own `cov_` tables) ----

const MSSQL_ADO: &str =
    "server=tcp:127.0.0.1,1433;user=sa;password=Password123!;database=master;TrustServerCertificate=true";

fn err<T>(r: dbmr::core::error::Result<T>) -> String {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => e.message().to_string(),
    }
}

#[tokio::test]
#[ignore = "requires live MSSQL"]
async fn cov_mssql_decodes_every_column_type() {
    let a = MssqlAdapter::connect(MSSQL_ADO).await.expect("ADO connect");
    a.query(
        "IF OBJECT_ID('cov_mssql_types', 'U') IS NOT NULL DROP TABLE cov_mssql_types;
         CREATE TABLE cov_mssql_types (
            id INT IDENTITY(1,1) PRIMARY KEY,
            ti TINYINT, si SMALLINT, i INT, bi BIGINT, r REAL, f FLOAT, dc DECIMAL(10,2),
            b BIT, s NVARCHAR(50), vb VARBINARY(16), g UNIQUEIDENTIFIER, dt DATETIME,
            j NVARCHAR(MAX), x XML, wide DECIMAL(38,30))",
        &[],
    )
    .await
    .unwrap();

    let dec = rust_decimal::Decimal::new(12345, 2);
    let values = [
        ("ti", DbValue::Int(200)),
        ("si", DbValue::Int(-300)),
        ("i", DbValue::Int(-2_000_000)),
        ("bi", DbValue::Int(9_000_000_000)),
        ("r", DbValue::Float(1.5)),
        ("f", DbValue::Float(2.25)),
        ("dc", DbValue::Decimal(dec)),
        ("b", DbValue::Bool(true)),
        ("s", DbValue::Text("hello".into())),
        ("vb", DbValue::Bytes(vec![0, 1, 255])),
        (
            "g",
            DbValue::Text("6F9619FF-8B86-D011-B42D-00C04FC964FF".into()),
        ),
        ("dt", DbValue::Text("2024-01-02T03:04:05".into())),
        ("j", DbValue::Json(serde_json::json!({"k": 1}))),
        ("x", DbValue::Text("<a>1</a>".into())),
        (
            "wide",
            DbValue::Text("1.500000000000000000000000000000".into()),
        ),
    ];
    let s = a
        .insert_rows(
            "cov_mssql_types",
            &[dbmr::core::types::row(values.clone())],
            &[],
        )
        .await
        .unwrap();
    let id = s.id.expect("identity id");
    assert_eq!(id, DbValue::Int(1));

    let r = a
        .get(
            "cov_mssql_types",
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
            "g" => DbValue::Text("6f9619ff-8b86-d011-b42d-00c04fc964ff".into()),
            // DATETIME reads back as ISO-8601 UTC, like PostgreSQL TIMESTAMP.
            "dt" => DbValue::Text("2024-01-02T03:04:05.000Z".into()),
            "j" => DbValue::Text(r#"{"k":1}"#.into()),
            _ => v,
        };
        assert_eq!(row.get(k), Some(&v), "column {k}");
    }

    // NULLs of every type decode as Null.
    a.query("INSERT INTO cov_mssql_types (s) VALUES (NULL)", &[])
        .await
        .unwrap();
    let r = a
        .get(
            "cov_mssql_types",
            &FilterExpr::eq("id", 2),
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
#[ignore = "requires live MSSQL"]
async fn cov_mssql_binds_paging_upsert_and_errors() {
    let a = MssqlAdapter::connect_host("127.0.0.1", 1433, "sa", "Password123!", "master", true)
        .await
        .unwrap();
    a.query(
        "IF OBJECT_ID('cov_mssql_ops', 'U') IS NOT NULL DROP TABLE cov_mssql_ops;
         CREATE TABLE cov_mssql_ops (
            id INT IDENTITY(1,1) PRIMARY KEY, code NVARCHAR(20) NOT NULL UNIQUE,
            flag BIT, score FLOAT, price DECIMAL(10,2), raw VARBINARY(16), meta NVARCHAR(100),
            deleted INT NOT NULL DEFAULT 0)",
        &[],
    )
    .await
    .unwrap();
    let t = "cov_mssql_ops";

    let s = a.insert_rows(t, &[], &[]).await.unwrap();
    assert_eq!(
        (s.rows, s.message.as_str()),
        (0, "0 Cov Mssql Opss are saved")
    );
    assert_eq!(a.upsert_rows(t, &[], &[]).await.unwrap().rows, 0);

    let price = rust_decimal::Decimal::new(999, 2);
    let mk = |code: &str, flag: bool, score: f64| {
        dbmr::core::types::row([
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
            t,
            &[mk("a", true, 1.5), mk("b", false, 2.5), mk("c", true, 3.5)],
            &[],
        )
        .await
        .unwrap();
    assert_eq!((s.rows, s.id), (3, None));

    // Every bind kind round-trips through a WHERE clause.
    let cnt = |f: FilterExpr| {
        let a = &a;
        async move { a.count(t, &f, None).await.unwrap() }
    };
    assert_eq!(cnt(FilterExpr::eq("flag", true)).await, 2);
    assert_eq!(cnt(FilterExpr::eq("score", 2.5)).await, 1);
    assert_eq!(cnt(FilterExpr::eq("price", price)).await, 3);
    assert_eq!(
        cnt(FilterExpr::eq("raw", DbValue::Bytes(vec![1, 2, 3]))).await,
        3
    );
    assert_eq!(
        cnt(FilterExpr::eq(
            "meta",
            DbValue::Json(serde_json::json!({"a": 1}))
        ))
        .await,
        3
    );
    assert_eq!(
        cnt(FilterExpr::default().and("score", Operator::Gt, DbValue::Null)).await,
        0
    );
    assert_eq!(
        cnt(FilterExpr::default().and("code", Operator::NotEq, "a")).await,
        2
    );

    // Paging: OFFSET/FETCH with and without an explicit order, TOP for limit-only.
    let by_code = [SortKey {
        column: CompactString::new("code"),
        direction: SortDirection::Desc,
    }];
    let page = a
        .list(t, &FilterExpr::empty(), &by_code, None, 1, 1)
        .await
        .unwrap();
    assert_eq!((page.data.len(), page.count), (1, 3));
    assert_eq!(page.data[0].get("code"), Some(&DbValue::Text("b".into())));
    let page = a
        .list(t, &FilterExpr::empty(), &[], None, 0, 2)
        .await
        .unwrap();
    assert_eq!((page.data.len(), page.count), (2, 3));
    let top = a
        .get(
            t,
            &FilterExpr::empty(),
            &by_code,
            None,
            GetOptions {
                limit: Some(1),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(top.data[0].get("code"), Some(&DbValue::Text("c".into())));

    // Upsert on `code`: existing row is updated (identity column never SET), new row inserted,
    // a row without the key column is a plain insert.
    let mut upd = mk("a", false, 9.0);
    upd.insert("id".into(), DbValue::Int(1));
    let s = a
        .upsert_rows(t, &[upd, mk("d", true, 4.0)], &[vec!["code".to_string()]])
        .await
        .unwrap();
    assert_eq!(s.rows, 2);
    assert_eq!(cnt(FilterExpr::eq("score", 9.0)).await, 1);
    assert_eq!(cnt(FilterExpr::eq("code", "d")).await, 1);
    let s = a
        .upsert_rows(
            t,
            &[mk("e", true, 5.0)],
            &[vec!["code".into(), "absent".into()]],
        )
        .await
        .unwrap();
    assert_eq!(s.rows, 1);
    // Default key `id`: only key columns present means nothing to update.
    let s = a
        .upsert_rows(t, &[dbmr::core::types::row([("id", DbValue::Int(1))])], &[])
        .await
        .unwrap();
    assert_eq!(s.rows, 1);
    assert_eq!(cnt(FilterExpr::empty()).await, 5);

    // Remove: soft then hard.
    let s = a
        .remove_rows(t, &FilterExpr::eq("code", "e"), Some("deleted"))
        .await
        .unwrap();
    assert_eq!(s.rows, 1);
    assert_eq!(
        a.count(t, &FilterExpr::empty(), Some("deleted"))
            .await
            .unwrap(),
        4
    );
    let s = a
        .remove_rows(t, &FilterExpr::eq("code", "d"), None)
        .await
        .unwrap();
    assert_eq!((s.rows, s.message.as_str()), (1, "1 cov_mssql_ops removed"));

    // Errors.
    assert!(
        err(a.query("SELECT * FROM cov_mssql_missing", &[]).await).starts_with("MSSQL query error")
    );
    let dup = err(a.insert_rows(t, &[mk("a", true, 1.0)], &[]).await);
    assert!(dup.contains("UNIQUE KEY"), "{dup}");
    assert_eq!(a.escape_id("a]b"), "[a]]b]");

    assert!(a.is_connected().await);
    a.disconnect().await.unwrap();
    assert!(!a.is_connected().await);

    // Connection failures.
    let refused = err(MssqlAdapter::connect_host("127.0.0.1", 1, "sa", "x", "master", true).await);
    assert!(
        refused.starts_with("Failed to connect to MSSQL 127.0.0.1:1"),
        "{refused}"
    );
    let refused = err(MssqlAdapter::connect(
        "server=tcp:127.0.0.1,1;user=sa;password=x;TrustServerCertificate=true",
    )
    .await);
    assert!(
        refused.starts_with("Failed to connect to MSSQL 127.0.0.1:1"),
        "{refused}"
    );
    let bad_login = err(MssqlAdapter::connect(
        "server=tcp:127.0.0.1,1433;user=sa;password=wrong;TrustServerCertificate=true",
    )
    .await);
    assert!(
        bad_login.starts_with("MSSQL handshake failed"),
        "{bad_login}"
    );
    let bad_login =
        err(MssqlAdapter::connect_host("127.0.0.1", 1433, "sa", "wrong", "master", true).await);
    assert!(
        bad_login.starts_with("MSSQL handshake failed"),
        "{bad_login}"
    );
    let invalid = err(MssqlAdapter::connect("jdbc:sqlserver://;=;;").await);
    assert!(!invalid.is_empty());
}

#[tokio::test]
#[ignore = "requires live service: docker compose up, then cargo test --all-features -- --ignored"]
async fn mssql_update_where_timestamps_tx_connect_with() {
    let a = MssqlAdapter::connect_with(
        MSSQL_ADO,
        PoolConfig {
            max_connections: 2,
            ..PoolConfig::default()
        },
    )
    .await
    .unwrap();
    let _ = a.query("DROP TABLE IF EXISTS uw_mssql", &[]).await;
    a.query("CREATE TABLE uw_mssql (id INT PRIMARY KEY, name NVARCHAR(50), version INT, deleted INT DEFAULT 0, ts DATETIME2(3), ts_text NVARCHAR(50))", &[]).await.unwrap();
    common_sql::update_where_and_timestamps(a.as_ref(), "uw_mssql").await;
    common_sql::transactions(a.as_ref(), "uw_mssql").await;
    common_sql::concurrent_pings(a.as_ref()).await;
}
