//! Model-level contract tests (BRD §21, §23, §26, §32, OQ-A/OQ-C) on SQLite in-memory.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use dbmr::adapters::SqliteAdapter;
use dbmr::core::events::EventSink;
use dbmr::core::types::row;
use dbmr::{
    Context, DatabaseAdapter, DbValue, EncryptionConfig, Model, ModelHooks, Query, Transaction,
};
use serde_json::{json, Value};
use sqlx::sqlite::SqlitePoolOptions;
use std::sync::{Arc, Mutex};

async fn db() -> Arc<SqliteAdapter> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query(
        "CREATE TABLE items (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            sku TEXT NOT NULL UNIQUE,
            name TEXT,
            qty INTEGER,
            secret TEXT,
            is_deleted INTEGER NOT NULL DEFAULT 0,
            created_at TEXT
        )",
    )
    .execute(&pool)
    .await
    .unwrap();
    Arc::new(SqliteAdapter::new(pool))
}

fn items(db: Arc<SqliteAdapter>) -> dbmr::core::ModelBuilder {
    Model::builder(db, "items")
        .column("id", "integer")
        .column("sku", "required|string")
        .column("name", "string|nullable")
        .column("qty", "integer")
        .unique(["sku"])
}

fn item(sku: &str, qty: i64) -> dbmr::RowMap {
    row([("sku", sku.into()), ("qty", qty.into())])
}

#[derive(Default)]
struct Sink(Mutex<Vec<(String, String, Value)>>);

#[async_trait]
impl EventSink for Sink {
    async fn emit(&self, table: &str, operation: &str, data: Value) {
        self.0
            .lock()
            .unwrap()
            .push((table.into(), operation.into(), data));
    }
}

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn insert_strips_pk_timestamps_unknown_and_refetches() {
    let m = items(db().await).build().unwrap();
    let mut r = item("a", 1);
    r.insert("id".into(), 500.into());
    r.insert("created_at".into(), "2020-01-01".into());
    r.insert("bogus".into(), "x".into());
    let rec = m.insert(r, &Context::new()).await.unwrap().unwrap();
    assert_eq!(rec["id"], DbValue::Int(1));
    assert_eq!(rec["created_at"], DbValue::Null);
    assert!(!rec.contains_key("bogus"));
}

#[tokio::test]
async fn unknown_rule_fails_at_build() {
    let err = Model::builder(db().await, "items")
        .column("x", "strnig")
        .build()
        .err()
        .unwrap();
    assert_eq!(err.message(), "Validation Rule: strnig does not exists.");
}

#[tokio::test]
async fn upsert_uses_unique_key_or_pk() {
    let m = items(db().await).build().unwrap();
    let ctx = Context::new();
    m.insert(item("a", 1), &ctx).await.unwrap();
    // unique key present → update in place
    let rec = m.upsert(item("a", 5), &ctx).await.unwrap().unwrap();
    assert_eq!(
        (rec["id"].clone(), rec["qty"].clone()),
        (DbValue::Int(1), DbValue::Int(5))
    );
    // only PK present → conflict on PK
    let rec = m
        .update(
            row([("id", 1.into()), ("sku", "a".into()), ("qty", 9.into())]),
            &ctx,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rec["qty"], DbValue::Int(9));
    assert_eq!(m.count(&Query::new(), &ctx).await.unwrap(), 1);
}

#[tokio::test]
async fn bulk_insert_is_atomic() {
    let ctx = Context::new();
    // Declared unique key: caught before the database, 409 with the row index.
    let m = items(db().await).build().unwrap();
    let err = m
        .insert_many(vec![item("a", 1), item("a", 2)], &ctx)
        .await
        .unwrap_err();
    assert_eq!(
        (err.status_code(), err.message()),
        (409, "data.1: Duplicate value for unique key (sku).")
    );
    assert_eq!(m.count(&Query::new(), &ctx).await.unwrap(), 0);
    // Constraint only in the table: the database rejects the batch and nothing is kept.
    let m = Model::builder(db().await, "items")
        .column("id", "integer")
        .column("sku", "required|string")
        .column("qty", "integer")
        .build()
        .unwrap();
    let err = m
        .insert_many(vec![item("a", 1), item("a", 2)], &ctx)
        .await
        .unwrap_err();
    assert_eq!(err.status_code(), 500);
    assert_eq!(m.count(&Query::new(), &ctx).await.unwrap(), 0);
}

#[tokio::test]
async fn soft_delete_hides_rows_and_mass_delete_is_guarded() {
    let m = items(db().await).safe_delete("is_deleted").build().unwrap();
    let ctx = Context::new();
    m.insert_many(vec![item("a", 1), item("b", 2)], &ctx)
        .await
        .unwrap();
    let err = m.remove(json!({}), &ctx).await.unwrap_err();
    assert_eq!(
        (err.status_code(), err.message()),
        (400, "Request body must contain filter criteria")
    );
    let s = m.remove(json!(1), &ctx).await.unwrap();
    assert_eq!(s.rows, 1);
    assert_eq!(m.count(&Query::new(), &ctx).await.unwrap(), 1);
    assert!(m.by_id(1, &ctx).await.unwrap().is_none());
    // The row is still physically there, flagged.
    let raw = m
        .adapter()
        .query("SELECT is_deleted FROM items WHERE id = 1", &[])
        .await
        .unwrap();
    assert_eq!(raw[0]["is_deleted"], DbValue::Int(1));
}

#[tokio::test]
async fn null_semantics() {
    let m = items(db().await).build().unwrap();
    let ctx = Context::new();
    m.insert(row([("sku", "a".into()), ("name", DbValue::Null)]), &ctx)
        .await
        .unwrap();
    m.insert(row([("sku", "b".into()), ("name", "x".into())]), &ctx)
        .await
        .unwrap();
    let (q, _) = Query::from_json(&json!({"name": null}), "id").unwrap();
    assert_eq!(m.find(q, &ctx).await.unwrap().count, 1);
    let (q, _) = Query::from_json(&json!({"filter": [[["name", "!=", null]]]}), "id").unwrap();
    assert_eq!(
        m.find(q, &ctx).await.unwrap().data[0]["sku"],
        DbValue::from("b")
    );
}

#[tokio::test]
async fn patch_requires_pk_and_changes_only_supplied_columns() {
    let m = items(db().await).build().unwrap();
    let ctx = Context::new();
    m.insert(
        row([("sku", "a".into()), ("name", "n".into()), ("qty", 1.into())]),
        &ctx,
    )
    .await
    .unwrap();
    let err = m.patch(row([("qty", 2.into())]), &ctx).await.unwrap_err();
    assert_eq!(
        (err.status_code(), err.message()),
        (422, "Primary key is required for patch")
    );
    let rec = m
        .patch(row([("id", 1.into()), ("qty", 2.into())]), &ctx)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (rec["name"].clone(), rec["qty"].clone()),
        (DbValue::from("n"), DbValue::Int(2))
    );
    assert!(m
        .patch(row([("id", 99.into()), ("qty", 2.into())]), &ctx)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn hooks_see_user_and_can_rewrite() {
    let hooks = ModelHooks::new()
        .before_save(|mut r, ctx| async move {
            let who = ctx.user["name"].as_str().unwrap_or("anon").to_string();
            r.insert("name".into(), who.into());
            Ok(r)
        })
        .after_find(|mut rows, _| async move {
            rows.retain(|r| r["qty"] != DbValue::Int(0));
            Ok(rows)
        });
    let m = items(db().await).hooks(hooks).build().unwrap();
    let ctx = Context::with_user(json!({"name": "ann"}));
    let rec = m.insert(item("a", 1), &ctx).await.unwrap().unwrap();
    assert_eq!(rec["name"], DbValue::from("ann"));
    m.insert(item("b", 0), &ctx).await.unwrap();
    assert_eq!(m.find(Query::new(), &ctx).await.unwrap().data.len(), 1);
}

#[tokio::test]
async fn encrypted_columns_round_trip_and_are_ciphertext_at_rest() {
    let db = db().await;
    let m = items(db.clone())
        .column("secret", "encrypted|string")
        .encryption(
            EncryptionConfig::new("raw:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", 1, None)
                .unwrap(),
        )
        .build()
        .unwrap();
    let ctx = Context::new();
    let mut r = item("a", 1);
    r.insert("secret".into(), "s3cr3t".into());
    let rec = m.insert(r, &ctx).await.unwrap().unwrap();
    assert_eq!(rec["secret"], DbValue::from("s3cr3t"));
    let raw = db.query("SELECT secret FROM items", &[]).await.unwrap();
    let DbValue::Text(stored) = &raw[0]["secret"] else {
        panic!()
    };
    assert!(dbmr::core::is_encrypted(stored.as_str()) && !stored.contains("s3cr3t"));
    // Without a key the model refuses to build.
    let err = items(db)
        .column("secret", "encrypted|string")
        .build()
        .err()
        .unwrap();
    assert_eq!(err.status_code(), 422);
}

#[tokio::test]
async fn transaction_commit_rollback_and_savepoints() {
    let db = db().await;
    let sink = Arc::new(Sink::default());
    let m = items(db.clone()).events(sink.clone()).build().unwrap();
    let ctx = Context::new();

    let tx = Transaction::begin(db.as_ref()).await.unwrap();
    let mt = m.with_tx(&tx);
    mt.insert(item("a", 1), &ctx).await.unwrap();
    let sp = tx.savepoint().await.unwrap();
    mt.insert(item("b", 1), &ctx).await.unwrap();
    sp.rollback().await.unwrap();
    mt.insert(item("c", 1), &ctx).await.unwrap();
    settle().await;
    assert!(sink.0.lock().unwrap().is_empty(), "events wait for commit");
    tx.commit().await.unwrap();
    settle().await;

    let skus: Vec<_> = m
        .find(Query::new().sort("sku"), &ctx)
        .await
        .unwrap()
        .data
        .into_iter()
        .map(|r| r["sku"].clone())
        .collect();
    assert_eq!(skus, vec![DbValue::from("a"), DbValue::from("c")]);
    let ops: Vec<_> = sink.0.lock().unwrap().iter().map(|e| e.1.clone()).collect();
    assert_eq!(ops, vec!["insert", "insert"]);

    let tx = Transaction::begin(db.as_ref()).await.unwrap();
    m.with_tx(&tx).insert(item("z", 1), &ctx).await.unwrap();
    tx.rollback().await.unwrap();
    settle().await;
    assert_eq!(m.count(&Query::new(), &ctx).await.unwrap(), 2);
    assert_eq!(sink.0.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn events_fire_without_transaction() {
    let sink = Arc::new(Sink::default());
    let m = items(db().await).events(sink.clone()).build().unwrap();
    m.insert(item("a", 1), &Context::new()).await.unwrap();
    settle().await;
    let e = sink.0.lock().unwrap();
    assert_eq!((e[0].0.as_str(), e[0].1.as_str()), ("items", "insert"));
    assert_eq!(e[0].2["sku"], "a");
}

#[tokio::test]
async fn list_pagination_defaults_and_clamp() {
    let m = items(db().await).build().unwrap();
    let ctx = Context::new();
    let rows: Vec<_> = (0..250).map(|i| item(&format!("s{i:03}"), i)).collect();
    m.insert_many(rows, &ctx).await.unwrap();
    let p = m.list(Query::new(), &ctx).await.unwrap();
    assert_eq!((p.data.len(), p.count), (30, 250));
    let p = m.list(Query::new().size(1000), &ctx).await.unwrap();
    assert_eq!(p.data.len(), 200);
    let p = m.list(Query::new().page(1).size(200), &ctx).await.unwrap();
    assert_eq!(p.data.len(), 50);
}

#[tokio::test]
async fn mass_delete_guard_runs_after_before_delete_hook() {
    // S9: a hook that empties the criteria must not turn into a delete-all.
    let hooks = ModelHooks::new().before_delete(|_, _| async { Ok(json!({})) });
    let m = items(db().await).hooks(hooks).build().unwrap();
    let ctx = Context::new();
    m.insert(item("a", 1), &ctx).await.unwrap();
    let err = m.remove(json!(1), &ctx).await.unwrap_err();
    assert_eq!(err.status_code(), 400);
    assert_eq!(m.count(&Query::new(), &ctx).await.unwrap(), 1);
}

#[tokio::test]
async fn soft_delete_flag_is_not_client_writable() {
    // D19: even when the flag column is declared, writes cannot set or clear it.
    let m = items(db().await)
        .column("is_deleted", "integer")
        .safe_delete("is_deleted")
        .build()
        .unwrap();
    let ctx = Context::new();
    let mut r = item("a", 1);
    r.insert("is_deleted".into(), 1.into());
    let rec = m.insert(r, &ctx).await.unwrap().unwrap();
    assert_eq!(rec["is_deleted"], DbValue::Int(0), "insert set the flag");
    m.remove(json!(1), &ctx).await.unwrap();
    let undelete = row([("id", 1.into()), ("is_deleted", 0.into())]);
    assert!(
        m.patch(undelete, &ctx).await.unwrap().is_none(),
        "patch revived a deleted row"
    );
    m.upsert(row([("sku", "a".into()), ("is_deleted", 0.into())]), &ctx)
        .await
        .unwrap();
    assert_eq!(
        m.count(&Query::new(), &ctx).await.unwrap(),
        0,
        "upsert revived a deleted row"
    );
}
