//! Shared live checks for the SQL adapters: `update_where`, `ping`, RFC 3339 timestamp
//! writes and transactions. Each test file creates the table with its own DDL:
//! `id` (plain integer PK), `name` text, `version` int, `deleted` 0/1 flag,
//! `ts` native datetime, `ts_text` text.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use dbmr::core::adapter::DatabaseAdapter;
use dbmr::core::filter::{FilterExpr, Operator};
use dbmr::core::transaction::Transaction;
use dbmr::core::types::{DbValue, GetOptions, RowMap};

pub const TS1: &str = "2026-11-16T10:20:30.123Z";
pub const TS2: &str = "2026-11-17T01:02:03.456Z";

pub fn row(id: i64, name: &str, deleted: i64) -> RowMap {
    let mut r = RowMap::new();
    r.insert("id".into(), DbValue::Int(id.into()));
    r.insert("name".into(), DbValue::from(name));
    r.insert("version".into(), DbValue::Int(1));
    r.insert("deleted".into(), DbValue::Int(deleted.into()));
    r.insert("ts".into(), DbValue::from(TS1));
    r.insert("ts_text".into(), DbValue::from(TS1));
    r
}

pub async fn fetch(db: &dyn DatabaseAdapter, table: &str, id: i64) -> Option<RowMap> {
    db.get(
        table,
        &FilterExpr::eq("id", id),
        &[],
        None,
        GetOptions::default(),
    )
    .await
    .unwrap()
    .data
    .pop()
}

/// Native datetime text varies by driver (`...T..Z`, `... ...0000`); compare the prefix.
fn assert_ts(v: Option<&DbValue>, want: &str) {
    let got = v
        .and_then(DbValue::as_str)
        .unwrap_or_else(|| panic!("ts not text: {v:?}"))
        .replace(' ', "T");
    assert!(got.starts_with(&want[..23]), "{got} vs {want}");
}

fn int(v: Option<&DbValue>) -> i128 {
    match v {
        Some(DbValue::Int(i)) => *i,
        Some(DbValue::Bool(b)) => i128::from(*b),
        Some(DbValue::Decimal(d)) => d.to_string().parse().unwrap(),
        other => panic!("not an int: {other:?}"),
    }
}

/// update_where (match / stale version / soft-deleted / empty set), timestamp writes via
/// insert, upsert and update_where, and ping.
pub async fn update_where_and_timestamps(db: &dyn DatabaseAdapter, table: &str) {
    db.ping().await.unwrap();
    db.insert_rows(table, &[row(1, "a", 0)], &[]).await.unwrap();
    db.insert_rows(table, &[row(2, "gone", 1)], &[])
        .await
        .unwrap();

    let mut set = RowMap::new();
    set.insert("name".into(), DbValue::from("b"));
    set.insert("version".into(), DbValue::Int(2));
    set.insert("ts".into(), DbValue::from(TS2));
    let v1 = FilterExpr::eq("id", 1).and("version", Operator::Eq, 1);
    assert_eq!(
        db.update_where(table, &set, &v1, Some("deleted"))
            .await
            .unwrap(),
        1
    );
    // Stale version: no match.
    assert_eq!(
        db.update_where(table, &set, &v1, Some("deleted"))
            .await
            .unwrap(),
        0
    );
    // Soft-deleted row is never updated.
    let gone = FilterExpr::eq("id", 2);
    assert_eq!(
        db.update_where(table, &set, &gone, Some("deleted"))
            .await
            .unwrap(),
        0
    );
    // Same row without the soft-delete guard is visible.
    let mut rename = RowMap::new();
    rename.insert("name".into(), DbValue::from("gone2"));
    assert_eq!(
        db.update_where(table, &rename, &gone, None).await.unwrap(),
        1
    );
    assert_eq!(
        db.update_where(table, &RowMap::new(), &v1, None)
            .await
            .unwrap(),
        0
    );

    let r = fetch(db, table, 1).await.unwrap();
    assert_eq!(r.get("name"), Some(&DbValue::from("b")));
    assert_eq!(int(r.get("version")), 2);
    assert_ts(r.get("ts"), TS2);
    // A text column keeps RFC 3339 text verbatim.
    assert_eq!(r.get("ts_text"), Some(&DbValue::from(TS1)));

    // Upsert: insert path then update path, both writing a timestamp.
    let mut up = row(3, "c", 0);
    db.upsert_rows(table, &[up.clone()], &[vec!["id".into()]])
        .await
        .unwrap();
    assert_ts(fetch(db, table, 3).await.unwrap().get("ts"), TS1);
    up.insert("ts".into(), DbValue::from(TS2));
    db.upsert_rows(table, &[up], &[vec!["id".into()]])
        .await
        .unwrap();
    let r = fetch(db, table, 3).await.unwrap();
    assert_ts(r.get("ts"), TS2);
    assert_eq!(r.get("ts_text"), Some(&DbValue::from(TS1)));
}

/// More concurrent pings than pooled connections (tests built with max_connections = 2).
pub async fn concurrent_pings(db: &dyn DatabaseAdapter) {
    for r in futures::future::join_all((0..8).map(|_| db.ping())).await {
        r.unwrap();
    }
}

async fn exists(db: &dyn DatabaseAdapter, table: &str, id: i64) -> bool {
    fetch(db, table, id).await.is_some()
}

/// Commit, rollback, savepoints (rolled back and released) and update_where in a tx.
pub async fn transactions(db: &dyn DatabaseAdapter, table: &str) {
    let tx = Transaction::begin(db).await.unwrap();
    tx.adapter()
        .insert_rows(table, &[row(10, "commit", 0)], &[])
        .await
        .unwrap();
    // Bulk inside a tx too.
    tx.adapter()
        .insert_rows(table, &[row(15, "x", 0), row(16, "y", 0)], &[])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(exists(db, table, 10).await);
    assert!(exists(db, table, 16).await);

    let tx = Transaction::begin(db).await.unwrap();
    tx.adapter()
        .insert_rows(table, &[row(11, "rollback", 0)], &[])
        .await
        .unwrap();
    let mut set = RowMap::new();
    set.insert("name".into(), DbValue::from("changed"));
    assert_eq!(
        tx.adapter()
            .update_where(table, &set, &FilterExpr::eq("id", 10), Some("deleted"))
            .await
            .unwrap(),
        1
    );
    tx.rollback().await.unwrap();
    assert!(!exists(db, table, 11).await);
    assert_eq!(
        fetch(db, table, 10).await.unwrap().get("name"),
        Some(&DbValue::from("commit"))
    );

    let tx = Transaction::begin(db).await.unwrap();
    let a = tx.adapter();
    a.insert_rows(table, &[row(12, "outer", 0)], &[])
        .await
        .unwrap();
    let sp = tx.savepoint().await.unwrap();
    a.insert_rows(table, &[row(13, "undone", 0)], &[])
        .await
        .unwrap();
    sp.rollback().await.unwrap();
    let sp = tx.savepoint().await.unwrap();
    a.insert_rows(table, &[row(14, "kept", 0)], &[])
        .await
        .unwrap();
    sp.commit().await.unwrap();
    drop(a);
    tx.commit().await.unwrap();
    assert!(exists(db, table, 12).await);
    assert!(!exists(db, table, 13).await);
    assert!(exists(db, table, 14).await);

    // Dropped without commit: rolled back.
    {
        let tx = Transaction::begin(db).await.unwrap();
        tx.adapter()
            .insert_rows(table, &[row(17, "dropped", 0)], &[])
            .await
            .unwrap();
    }
    assert!(!exists(db, table, 17).await);
}
