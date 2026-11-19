//! Model, transaction and adapter-trait behaviour against an in-test fake adapter that
//! records every call and can be told to fail.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use async_trait::async_trait;
use dbmr_core::adapter::{DatabaseAdapter, TransactionAdapter, NO_TX_MESSAGE};
use dbmr_core::encryption::encrypt;
use dbmr_core::error::{AppError, Result};
use dbmr_core::events::EventSink;
use dbmr_core::filter::{FilterExpr, Operator};
use dbmr_core::hooks::{BulkOp, Context, ModelHooks};
use dbmr_core::model::{Model, ModelOptions, MASS_DELETE_MESSAGE};
use dbmr_core::query::Query;
use dbmr_core::transaction::Transaction;
use dbmr_core::types::{row, DbValue, FindResult, GetOptions, RowMap, SortKey, WriteSummary};
use dbmr_core::EncryptionConfig;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

// ------------------------------------------------------------------ fake adapter

/// Rows written by one call and the conflict target it was given.
type Write = (Vec<RowMap>, Vec<Vec<String>>);

#[derive(Default)]
struct State {
    log: Mutex<Vec<String>>,
    rows: Mutex<Vec<RowMap>>,
    queued: Mutex<VecDeque<Vec<RowMap>>>,
    filters: Mutex<Vec<FilterExpr>>,
    sorts: Mutex<Vec<Vec<SortKey>>>,
    writes: Mutex<Vec<Write>>,
    summary: Mutex<Option<WriteSummary>>,
    fail: Mutex<Option<&'static str>>,
}

#[derive(Clone, Default)]
struct Fake(Arc<State>);

impl Fake {
    fn db(&self) -> Arc<dyn DatabaseAdapter> {
        Arc::new(self.clone())
    }
    fn push_log(&self, s: String) {
        self.0.log.lock().unwrap().push(s);
    }
    fn log(&self) -> Vec<String> {
        self.0.log.lock().unwrap().clone()
    }
    fn fail_on(&self, m: &'static str) {
        *self.0.fail.lock().unwrap() = Some(m);
    }
    fn check(&self, m: &str) -> Result<()> {
        if *self.0.fail.lock().unwrap() == Some(m) {
            return Err(AppError::Database(format!("{m} failed")));
        }
        Ok(())
    }
    fn set_rows(&self, rows: Vec<RowMap>) {
        *self.0.rows.lock().unwrap() = rows;
    }
    fn queue(&self, rows: Vec<RowMap>) {
        self.0.queued.lock().unwrap().push_back(rows);
    }
    fn set_summary(&self, s: WriteSummary) {
        *self.0.summary.lock().unwrap() = Some(s);
    }
    fn next_rows(&self, filter: &FilterExpr, sort: &[SortKey]) -> Vec<RowMap> {
        self.0.filters.lock().unwrap().push(filter.clone());
        self.0.sorts.lock().unwrap().push(sort.to_vec());
        self.0
            .queued
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| self.0.rows.lock().unwrap().clone())
    }
    fn last_filter(&self) -> FilterExpr {
        self.0.filters.lock().unwrap().last().cloned().unwrap()
    }
    fn last_sort(&self) -> Vec<SortKey> {
        self.0.sorts.lock().unwrap().last().cloned().unwrap()
    }
    fn last_write(&self) -> Write {
        self.0.writes.lock().unwrap().last().cloned().unwrap()
    }
    fn write(&self, data: &[RowMap], unique: &[Vec<String>]) -> WriteSummary {
        self.0
            .writes
            .lock()
            .unwrap()
            .push((data.to_vec(), unique.to_vec()));
        self.0
            .summary
            .lock()
            .unwrap()
            .clone()
            .unwrap_or(WriteSummary {
                rows: data.len() as u64,
                message: "ok".into(),
                id: None,
                record: None,
            })
    }
}

#[async_trait]
impl DatabaseAdapter for Fake {
    async fn disconnect(&self) -> Result<()> {
        self.push_log("disconnect".into());
        self.check("disconnect")
    }
    async fn is_connected(&self) -> bool {
        true
    }
    async fn get(
        &self,
        _table: &str,
        filter: &FilterExpr,
        sort: &[SortKey],
        safe_delete: Option<&str>,
        options: GetOptions,
    ) -> Result<FindResult> {
        self.check("get")?;
        self.push_log(format!(
            "get sd={safe_delete:?} limit={:?} count={}",
            options.limit, options.count
        ));
        let mut data = self.next_rows(filter, sort);
        let count = data.len() as u64;
        if let Some(l) = options.limit {
            data.truncate(l as usize);
        }
        Ok(FindResult { data, count })
    }
    async fn list(
        &self,
        _table: &str,
        filter: &FilterExpr,
        sort: &[SortKey],
        _safe_delete: Option<&str>,
        page: u64,
        size: u64,
    ) -> Result<FindResult> {
        self.push_log(format!("list page={page} size={size}"));
        let data = self.next_rows(filter, sort);
        Ok(FindResult {
            count: data.len() as u64,
            data,
        })
    }
    async fn count(&self, _t: &str, filter: &FilterExpr, _sd: Option<&str>) -> Result<u64> {
        Ok(self.next_rows(filter, &[]).len() as u64)
    }
    async fn insert_rows(
        &self,
        _t: &str,
        data: &[RowMap],
        unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        self.check("insert")?;
        self.push_log("insert".into());
        Ok(self.write(data, unique))
    }
    async fn upsert_rows(
        &self,
        _t: &str,
        data: &[RowMap],
        unique: &[Vec<String>],
    ) -> Result<WriteSummary> {
        self.push_log("upsert".into());
        Ok(self.write(data, unique))
    }
    async fn remove_rows(
        &self,
        _t: &str,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<WriteSummary> {
        self.push_log(format!("remove sd={safe_delete:?}"));
        self.0.filters.lock().unwrap().push(filter.clone());
        Ok(WriteSummary {
            rows: 2,
            message: "removed".into(),
            id: None,
            record: None,
        })
    }
    async fn update_where(
        &self,
        _t: &str,
        _set: &RowMap,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<u64> {
        self.push_log(format!("update_where sd={safe_delete:?}"));
        self.0.filters.lock().unwrap().push(filter.clone());
        Ok(1)
    }
    async fn query(&self, sql: &str, _params: &[DbValue]) -> Result<Vec<RowMap>> {
        self.check("query")?;
        self.push_log(sql.to_string());
        Ok(Vec::new())
    }
    fn escape_id(&self, name: &str) -> String {
        format!("\"{name}\"")
    }
    async fn begin_tx(&self) -> Result<Arc<dyn TransactionAdapter>> {
        self.push_log("BEGIN".into());
        Ok(Arc::new(self.clone()))
    }
}

#[async_trait]
impl TransactionAdapter for Fake {
    async fn commit(&self) -> Result<()> {
        self.check("commit")?;
        self.push_log("COMMIT".into());
        Ok(())
    }
    async fn rollback(&self) -> Result<()> {
        self.push_log("ROLLBACK".into());
        Ok(())
    }
    fn as_adapter(self: Arc<Self>) -> Arc<dyn DatabaseAdapter> {
        self
    }
}

/// Adapter that keeps the trait's default `begin_tx`.
struct NoTx;

#[async_trait]
impl DatabaseAdapter for NoTx {
    async fn disconnect(&self) -> Result<()> {
        Ok(())
    }
    async fn is_connected(&self) -> bool {
        false
    }
    async fn get(
        &self,
        _: &str,
        _: &FilterExpr,
        _: &[SortKey],
        _: Option<&str>,
        _: GetOptions,
    ) -> Result<FindResult> {
        Ok(FindResult {
            data: vec![],
            count: 0,
        })
    }
    async fn list(
        &self,
        _: &str,
        _: &FilterExpr,
        _: &[SortKey],
        _: Option<&str>,
        _: u64,
        _: u64,
    ) -> Result<FindResult> {
        Ok(FindResult {
            data: vec![],
            count: 0,
        })
    }
    async fn count(&self, _: &str, _: &FilterExpr, _: Option<&str>) -> Result<u64> {
        Ok(0)
    }
    async fn insert_rows(&self, _: &str, _: &[RowMap], _: &[Vec<String>]) -> Result<WriteSummary> {
        Err(AppError::Database("read-only".into()))
    }
    async fn upsert_rows(&self, _: &str, _: &[RowMap], _: &[Vec<String>]) -> Result<WriteSummary> {
        Err(AppError::Database("read-only".into()))
    }
    async fn remove_rows(&self, _: &str, _: &FilterExpr, _: Option<&str>) -> Result<WriteSummary> {
        Err(AppError::Database("read-only".into()))
    }
    async fn update_where(
        &self,
        _: &str,
        _: &RowMap,
        _: &FilterExpr,
        _: Option<&str>,
    ) -> Result<u64> {
        Err(AppError::Database("read-only".into()))
    }
    async fn query(&self, _: &str, _: &[DbValue]) -> Result<Vec<RowMap>> {
        Ok(vec![])
    }
    fn escape_id(&self, name: &str) -> String {
        name.to_string()
    }
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

impl Sink {
    fn ops(&self) -> Vec<String> {
        self.0.lock().unwrap().iter().map(|e| e.1.clone()).collect()
    }
}

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

fn summary_with(record: Option<RowMap>, id: Option<DbValue>) -> WriteSummary {
    WriteSummary {
        rows: 1,
        message: "ok".into(),
        id,
        record,
    }
}

type Log = Arc<Mutex<Vec<String>>>;

macro_rules! tag {
    ($log:expr, $name:expr) => {{
        let l: Log = $log.clone();
        move |r, _c: Context| {
            let l = l.clone();
            async move {
                l.lock().unwrap().push($name.to_string());
                Ok(r)
            }
        }
    }};
}

fn ctx() -> Context {
    Context::new()
}

// ------------------------------------------------------------------ builder

#[tokio::test]
async fn define_and_accessors_expose_the_model_definition() {
    let fake = Fake::default();
    let m = Model::define(
        fake.db(),
        "users",
        &[("uid", "integer"), ("name", "required|string")],
        "uid",
        &["name"],
    )
    .unwrap();
    assert_eq!(m.pk(), "uid");
    assert_eq!(m.table_name(), "users");
    assert_eq!(m.structure().keys().collect::<Vec<_>>(), ["uid", "name"]);
    assert!(!m.options().allow_destructive);
    assert!(m.hooks().before_save.is_none());
    assert_eq!(m.adapter().escape_id("x"), "\"x\"");

    let err = Model::define(fake.db(), "t", &[("a", "bogus")], "id", &[])
        .err()
        .unwrap();
    assert_eq!(err.message(), "Validation Rule: bogus does not exists.");
}

#[tokio::test]
async fn build_requires_encryption_config_for_encrypted_columns() {
    let fake = Fake::default();
    let err = Model::builder(fake.db(), "t")
        .column("s", "encrypted|string")
        .build()
        .err()
        .unwrap();
    assert_eq!(err.status_code(), 422);
    assert!(err.message().starts_with(
        "Model \"t\" declares encrypted fields but no encryption config was provided."
    ));

    let cfg =
        EncryptionConfig::new("raw:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", 1, None).unwrap();
    assert!(Model::builder(fake.db(), "t")
        .column("s", "encrypted|string")
        .encryption_shared(Some(Arc::new(cfg)))
        .build()
        .is_ok());
}

#[tokio::test]
async fn custom_timestamp_variants_replace_the_defaults() {
    let fake = Fake::default();
    let m = Model::builder(fake.db(), "t")
        .columns([
            ("id", "integer"),
            ("made", "string"),
            ("created_at", "string"),
        ])
        .options(ModelOptions {
            created_at: Some(vec!["made".into()]),
            modified_at: Some(vec![]),
            ..ModelOptions::default()
        })
        .build()
        .unwrap();
    m.insert_many(
        vec![row([("made", "x".into()), ("created_at", "y".into())])],
        &ctx(),
    )
    .await
    .unwrap();
    let (written, _) = fake.last_write();
    assert!(!written[0].contains_key("made"));
    assert_eq!(written[0]["created_at"], DbValue::Text("y".into()));
}

// ------------------------------------------------------------------ writes

#[tokio::test]
async fn insert_refetches_by_returned_id_and_decodes_json_text_columns() {
    let fake = Fake::default();
    fake.set_summary(summary_with(None, Some(DbValue::Int(5))));
    fake.set_rows(vec![row([
        ("id", 5i64.into()),
        ("meta", r#"{"a":1}"#.into()),
        ("tags", DbValue::Bytes(b"[1,2]".to_vec())),
        ("note", "plain text".into()),
    ])]);
    let m = Model::builder(fake.db(), "t")
        .columns([
            ("id", "integer"),
            ("meta", "object"),
            ("tags", "array"),
            ("note", "object"),
            ("absent", "array"),
        ])
        .build()
        .unwrap();
    let rec = m
        .insert(
            row([
                ("id", 9i64.into()),
                ("meta", DbValue::Json(json!({"a": 1}))),
            ]),
            &ctx(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rec["meta"], DbValue::Json(json!({"a": 1})));
    assert_eq!(rec["tags"], DbValue::Json(json!([1, 2])));
    // Text that is not JSON stays text.
    assert_eq!(rec["note"], DbValue::Text("plain text".into()));

    let (written, _) = fake.last_write();
    assert!(!written[0].contains_key("id"), "insert strips the PK");
    assert_eq!(written[0]["meta"], DbValue::Text(r#"{"a":1}"#.into()));
    assert_eq!(fake.last_filter(), FilterExpr::eq("id", 5i64));
    // Insert re-fetch ignores safe delete and asks for one row.
    assert_eq!(
        fake.log().last().unwrap(),
        "get sd=None limit=Some(1) count=false"
    );
}

#[tokio::test]
async fn write_returns_none_when_the_record_cannot_be_refetched() {
    let fake = Fake::default();
    let m = Model::builder(fake.db(), "t")
        .columns([("id", "integer"), ("name", "string")])
        .safe_delete("gone")
        .build()
        .unwrap();
    // No id from the adapter, none in the payload: nothing to re-fetch.
    assert!(m
        .insert(row([("name", "a".into())]), &ctx())
        .await
        .unwrap()
        .is_none());
    assert!(!fake.log().iter().any(|l| l.starts_with("get")));

    // Adapter id 0 is ignored; payload PK is used; the row is gone.
    fake.set_summary(summary_with(None, Some(DbValue::Int(0))));
    assert!(m
        .update(row([("id", 3i64.into()), ("name", "a".into())]), &ctx())
        .await
        .unwrap()
        .is_none());
    assert_eq!(fake.last_filter(), FilterExpr::eq("id", 3i64));
    assert_eq!(
        fake.log().last().unwrap(),
        "get sd=Some(\"gone\") limit=Some(1) count=false"
    );
}

#[tokio::test]
async fn upsert_conflict_target_prefers_unique_then_pk_then_declared_keys() {
    let fake = Fake::default();
    let m = Model::builder(fake.db(), "t")
        .columns([
            ("id", "integer"),
            ("sku", "string"),
            ("a", "string"),
            ("b", "string"),
        ])
        .unique_keys(vec![vec!["sku".into()], vec!["a".into(), "b".into()]])
        .build()
        .unwrap();
    let target = |rows: Vec<RowMap>| {
        let m = m.clone();
        let fake = fake.clone();
        async move {
            m.upsert_many(rows, &ctx()).await.unwrap();
            fake.last_write().1
        }
    };
    assert_eq!(
        target(vec![row([("a", "1".into()), ("b", "2".into())])]).await,
        [vec!["a".to_string(), "b".to_string()]]
    );
    assert_eq!(
        target(vec![row([("id", 1i64.into())]), row([("id", 2i64.into())])]).await,
        [vec!["id".to_string()]]
    );
    // One row lacks every key: fall back to the declared keys.
    assert_eq!(
        target(vec![row([("id", 1i64.into())]), row([("a", "x".into())])]).await,
        [
            vec!["sku".to_string()],
            vec!["a".to_string(), "b".to_string()]
        ]
    );
}

#[tokio::test]
async fn single_upsert_runs_hooks_in_documented_order_and_uses_adapter_record() {
    let fake = Fake::default();
    let stored = row([("id", 1i64.into()), ("name", "n".into())]);
    fake.set_summary(summary_with(Some(stored.clone()), None));
    let log: Log = Arc::default();
    let hooks = ModelHooks::new()
        .before_save(tag!(log, "before_save"))
        .before_update(tag!(log, "before_update"))
        .before_upsert(tag!(log, "before_upsert"))
        .after_save(tag!(log, "after_save"))
        .after_update(tag!(log, "after_update"))
        .after_upsert(tag!(log, "after_upsert"));
    let sink = Arc::new(Sink::default());
    let m = Model::builder(fake.db(), "t")
        .columns([("id", "integer"), ("name", "string")])
        .hooks(hooks)
        .events(sink.clone())
        .build()
        .unwrap();
    let rec = m
        .upsert(row([("name", "n".into())]), &ctx())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rec, stored);
    assert_eq!(
        *log.lock().unwrap(),
        [
            "before_save",
            "before_update",
            "before_upsert",
            "after_save",
            "after_update",
            "after_upsert"
        ]
    );
    // Record came from the adapter summary: no re-fetch.
    assert!(!fake.log().iter().any(|l| l.starts_with("get")));
    settle().await;
    assert_eq!(sink.ops(), ["upsert"]);
}

#[tokio::test]
async fn after_bulk_receives_operation_and_summary_and_can_abort() {
    let fake = Fake::default();
    let seen: Arc<Mutex<Vec<(BulkOp, u64)>>> = Arc::default();
    let s = seen.clone();
    let m = Model::builder(fake.db(), "t")
        .columns([("id", "integer"), ("name", "string")])
        .hooks(ModelHooks::new().after_bulk(move |op, summary, _ctx| {
            let s = s.clone();
            async move {
                s.lock().unwrap().push((op, summary.rows));
                Ok(())
            }
        }))
        .build()
        .unwrap();
    let rows = vec![row([("name", "a".into())]), row([("name", "b".into())])];
    assert_eq!(m.insert_many(rows.clone(), &ctx()).await.unwrap().rows, 2);
    m.update_many(rows.clone(), &ctx()).await.unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        [(BulkOp::Insert, 2), (BulkOp::Update, 2)]
    );

    let failing = Model::builder(fake.db(), "t")
        .columns([("name", "string")])
        .hooks(
            ModelHooks::new()
                .after_bulk(|_, _, _| async { Err(AppError::BadRequest("bulk refused".into())) }),
        )
        .build()
        .unwrap();
    let err = failing.insert_many(rows, &ctx()).await.unwrap_err();
    assert_eq!(err.message(), "bulk refused");
}

#[tokio::test]
async fn adapter_errors_propagate_from_writes() {
    let fake = Fake::default();
    fake.fail_on("insert");
    let m = Model::builder(fake.db(), "t")
        .column("name", "string")
        .build()
        .unwrap();
    let err = m
        .insert(row([("name", "a".into())]), &ctx())
        .await
        .unwrap_err();
    assert_eq!((err.status_code(), err.message()), (500, "insert failed"));
}

#[tokio::test]
async fn patch_merges_into_existing_row_and_returns_none_if_it_vanishes() {
    let fake = Fake::default();
    fake.queue(vec![row([
        ("id", 1i64.into()),
        ("name", "old".into()),
        ("qty", 1i64.into()),
    ])]);
    fake.queue(vec![]);
    let m = Model::builder(fake.db(), "t")
        .columns([
            ("id", "integer"),
            ("name", "required|string"),
            ("qty", "integer"),
        ])
        .build()
        .unwrap();
    let out = m
        .patch(
            row([
                ("id", 1i64.into()),
                ("qty", 2i64.into()),
                ("junk", 1i64.into()),
            ]),
            &ctx(),
        )
        .await
        .unwrap();
    assert!(out.is_none());
    let (written, unique) = fake.last_write();
    assert_eq!(
        written[0],
        row([
            ("id", 1i64.into()),
            ("name", "old".into()),
            ("qty", 2i64.into())
        ])
    );
    assert_eq!(unique, [vec!["id".to_string()]]);
}

// ------------------------------------------------------------------ remove

#[tokio::test]
async fn remove_runs_after_delete_and_guards_mass_delete() {
    let fake = Fake::default();
    let log: Log = Arc::default();
    let sink = Arc::new(Sink::default());
    let m = Model::builder(fake.db(), "t")
        .columns([("id", "integer"), ("qty", "integer"), ("name", "string")])
        .hooks(ModelHooks::new().after_delete(tag!(log, "after_delete")))
        .events(sink.clone())
        .build()
        .unwrap();

    let filter = FilterExpr::eq("qty", 3i64).and("name", Operator::Like, "%a%");
    let summary = m.remove_where(&filter, &ctx()).await.unwrap();
    assert_eq!(summary.rows, 2);
    assert_eq!(fake.last_filter(), filter);
    assert_eq!(*log.lock().unwrap(), ["after_delete"]);
    settle().await;
    assert_eq!(sink.ops(), ["delete"]);

    let err = m.remove(json!({}), &ctx()).await.unwrap_err();
    assert_eq!(
        (err.status_code(), err.message()),
        (400, MASS_DELETE_MESSAGE)
    );
    let err = m
        .remove_where(&FilterExpr::empty(), &ctx())
        .await
        .unwrap_err();
    assert_eq!(err.message(), MASS_DELETE_MESSAGE);

    let open = Model::builder(fake.db(), "t")
        .column("id", "integer")
        .allow_destructive(true)
        .build()
        .unwrap();
    open.remove(json!({}), &ctx()).await.unwrap();
    assert!(fake.last_filter().is_empty());
}

// ------------------------------------------------------------------ reads

#[tokio::test]
async fn by_id_rejects_non_scalar_or_empty_ids() {
    let fake = Fake::default();
    let m = Model::builder(fake.db(), "t")
        .column("id", "integer")
        .build()
        .unwrap();
    for bad in [
        DbValue::Null,
        DbValue::Text(String::new()),
        DbValue::Bool(true),
        DbValue::Json(json!([1])),
        DbValue::Bytes(vec![1]),
    ] {
        let err = m.by_id(bad.clone(), &ctx()).await.unwrap_err();
        assert_eq!(
            (err.status_code(), err.message()),
            (422, "Invalid id value"),
            "{bad:?}"
        );
    }
    assert!(m.by_id(2.5f64, &ctx()).await.unwrap().is_none());
    assert!(m.by_id(Decimal::ONE, &ctx()).await.unwrap().is_none());
    assert_eq!(fake.last_filter(), FilterExpr::eq("id", Decimal::ONE));
}

#[tokio::test]
async fn after_find_on_single_reads_takes_first_row_or_keeps_original() {
    let fake = Fake::default();
    let original = row([("id", 1i64.into()), ("name", "a".into())]);
    fake.set_rows(vec![original.clone()]);
    let build = |hooks: ModelHooks| {
        Model::builder(fake.db(), "t")
            .columns([("id", "integer"), ("name", "string")])
            .hooks(hooks)
            .build()
            .unwrap()
    };
    let replacing = build(
        ModelHooks::new().after_find(|rows: Vec<RowMap>, _| async move {
            Ok(rows
                .into_iter()
                .map(|mut r| {
                    r.insert("name".into(), "hooked".into());
                    r
                })
                .collect())
        }),
    );
    let rec = replacing.by_id(1i64, &ctx()).await.unwrap().unwrap();
    assert_eq!(rec["name"], DbValue::Text("hooked".into()));

    let emptying = build(ModelHooks::new().after_find(|_, _| async { Ok(vec![]) }));
    assert_eq!(
        emptying.by_id(1i64, &ctx()).await.unwrap().unwrap(),
        original
    );
}

#[tokio::test]
async fn find_one_honours_sort_select_before_find_and_empty_results() {
    let fake = Fake::default();
    let m = Model::builder(fake.db(), "t")
        .columns([("id", "integer"), ("name", "string"), ("tenant", "string")])
        .safe_delete("gone")
        .hooks(
            ModelHooks::new().before_find(|f: FilterExpr, _| async move {
                Ok(f.and("tenant", Operator::Eq, "t1"))
            }),
        )
        .build()
        .unwrap();
    assert!(m.find_one(Query::new(), &ctx()).await.unwrap().is_none());

    fake.set_rows(vec![
        row([("id", 1i64.into()), ("name", "a".into())]),
        row([("id", 2i64.into()), ("name", "b".into())]),
    ]);
    let rec = m
        .find_one(
            Query::new().eq("name", "a").sort("-id").select(["name"]),
            &ctx(),
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(rec, row([("name", "a".into())]));
    assert_eq!(
        fake.last_filter(),
        FilterExpr::eq("name", "a").and("tenant", Operator::Eq, "t1")
    );
    assert_eq!(fake.last_sort()[0].column.as_str(), "id");
    assert_eq!(
        fake.log().last().unwrap(),
        "get sd=Some(\"gone\") limit=Some(1) count=false"
    );
}

#[tokio::test]
async fn find_and_list_apply_select_and_pass_paging() {
    let fake = Fake::default();
    fake.set_rows(vec![row([("id", 1i64.into()), ("name", "a".into())])]);
    let m = Model::builder(fake.db(), "t")
        .columns([("id", "integer"), ("name", "string")])
        .search_columns(["name"])
        .build()
        .unwrap();
    let res = m
        .find(Query::new().select(["id"]).limit(3), &ctx())
        .await
        .unwrap();
    assert_eq!(res.data, [row([("id", 1i64.into())])]);
    assert_eq!(
        fake.log().last().unwrap(),
        "get sd=None limit=Some(3) count=false"
    );

    let res = m
        .list(Query::new().search("bob").page(1).size(5), &ctx())
        .await
        .unwrap();
    assert_eq!(res.count, 1);
    assert_eq!(fake.log().last().unwrap(), "list page=1 size=5");
    assert_eq!(
        fake.last_filter(),
        FilterExpr::default().and("name", Operator::Like, "bob")
    );
}

#[tokio::test]
async fn read_filters_coerce_text_to_declared_numeric_types() {
    let fake = Fake::default();
    let m = Model::builder(fake.db(), "t")
        .columns([
            ("id", "auto_increment"),
            ("qty", "integer"),
            ("price", "numeric"),
            ("name", "string"),
        ])
        .build()
        .unwrap();
    let q = Query::new()
        .eq("id", "4")
        .eq("qty", "abc")
        .and("price", Operator::Gte, "1.5")
        .and("price", Operator::Lt, "2")
        .and("qty", Operator::In, DbValue::Json(json!(["1", "x", 3])))
        .and("qty", Operator::Like, "%1%")
        .and("qty", Operator::Gt, 1i64)
        .eq("name", "5")
        .eq("unknown", "6");
    m.count(&q, &ctx()).await.unwrap();
    let got: Vec<DbValue> = fake.last_filter().groups[0]
        .iter()
        .map(|c| c.value.clone())
        .collect();
    assert_eq!(
        got,
        [
            DbValue::Int(4),
            DbValue::Text("abc".into()),
            DbValue::Float(1.5),
            DbValue::Int(2), // integral numeric → integer (same JSON everywhere)
            DbValue::Json(json!([1, "x", 3])),
            DbValue::Text("%1%".into()),
            DbValue::Int(1),
            DbValue::Text("5".into()),
            DbValue::Text("6".into()),
        ]
    );
}

#[tokio::test]
async fn decode_surfaces_decryption_failures() {
    let fake = Fake::default();
    let foreign = encrypt("x", &[1u8; 32], 9).unwrap();
    fake.set_rows(vec![row([("id", 1i64.into()), ("s", foreign.into())])]);
    let m = Model::builder(fake.db(), "t")
        .columns([("id", "integer"), ("s", "encrypted|string")])
        .encryption(
            EncryptionConfig::new("raw:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", 1, None)
                .unwrap(),
        )
        .build()
        .unwrap();
    let err = m.by_id(1i64, &ctx()).await.unwrap_err();
    assert_eq!(err.message(), "No key found for envelope version v9");

    // A config without encrypted columns leaves envelopes untouched.
    let plain = Model::builder(fake.db(), "t")
        .columns([("id", "integer"), ("s", "string")])
        .encryption(
            EncryptionConfig::new("raw:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=", 1, None)
                .unwrap(),
        )
        .build()
        .unwrap();
    let rec = plain.by_id(1i64, &ctx()).await.unwrap().unwrap();
    assert!(dbmr_core::is_encrypted(rec["s"].as_str().unwrap()));
}

// ------------------------------------------------------------------ events / transactions

#[test]
fn events_without_a_tokio_runtime_are_dropped_not_fatal() {
    let fake = Fake::default();
    fake.set_summary(summary_with(Some(row([("id", 1i64.into())])), None));
    let sink = Arc::new(Sink::default());
    let m = Model::builder(fake.db(), "t")
        .column("id", "integer")
        .events(sink.clone())
        .build()
        .unwrap();
    let rec = futures::executor::block_on(m.insert(row([]), &ctx())).unwrap();
    assert!(rec.is_some());
    assert!(sink.ops().is_empty());
}

#[tokio::test]
async fn transaction_buffers_events_across_savepoints_until_commit() {
    let fake = Fake::default();
    fake.set_summary(summary_with(Some(row([("id", 1i64.into())])), None));
    let sink = Arc::new(Sink::default());
    let m = Model::builder(fake.db(), "t")
        .column("id", "integer")
        .events(sink.clone())
        .build()
        .unwrap();

    let tx = Transaction::begin(&fake).await.unwrap();
    let mt = m.with_tx(&tx);
    mt.insert(row([]), &ctx()).await.unwrap();

    let sp = tx.savepoint().await.unwrap();
    mt.upsert(row([]), &ctx()).await.unwrap();
    sp.rollback().await.unwrap();

    let sp = tx.savepoint().await.unwrap();
    let nested = sp.savepoint().await.unwrap();
    mt.update(row([("id", 1i64.into())]), &ctx()).await.unwrap();
    nested.commit().await.unwrap();
    sp.commit().await.unwrap();

    settle().await;
    assert!(sink.ops().is_empty(), "nothing is emitted before commit");
    tx.commit().await.unwrap();
    settle().await;
    assert_eq!(sink.ops(), ["insert", "update"]);

    let sql: Vec<String> = fake
        .log()
        .into_iter()
        .filter(|l| l.contains("SAVEPOINT") || ["BEGIN", "COMMIT"].contains(&l.as_str()))
        .collect();
    assert_eq!(
        sql,
        [
            "BEGIN",
            "SAVEPOINT sp_1",
            "ROLLBACK TO SAVEPOINT sp_1",
            "SAVEPOINT sp_1",
            "SAVEPOINT sp_2",
            "RELEASE SAVEPOINT sp_2",
            "RELEASE SAVEPOINT sp_1",
            "COMMIT"
        ]
    );
}

#[tokio::test]
async fn failed_commit_discards_buffered_events_and_rollback_reaches_adapter() {
    let fake = Fake::default();
    fake.set_summary(summary_with(Some(row([("id", 1i64.into())])), None));
    let sink = Arc::new(Sink::default());
    let m = Model::builder(fake.db(), "t")
        .column("id", "integer")
        .events(sink.clone())
        .build()
        .unwrap();

    let tx = Transaction::begin(&fake).await.unwrap();
    m.with_tx(&tx).insert(row([]), &ctx()).await.unwrap();
    tx.rollback().await.unwrap();
    assert!(fake.log().contains(&"ROLLBACK".to_string()));

    let tx = Transaction::begin(&fake).await.unwrap();
    m.with_tx(&tx).insert(row([]), &ctx()).await.unwrap();
    fake.fail_on("commit");
    let err = tx.commit().await.unwrap_err();
    assert_eq!(err.message(), "commit failed");
    settle().await;
    assert!(sink.ops().is_empty());

    fake.fail_on("query");
    let tx = Transaction::begin(&fake).await.unwrap();
    assert_eq!(
        tx.savepoint().await.err().unwrap().message(),
        "query failed"
    );
}

#[tokio::test]
async fn adapters_without_transactions_return_the_documented_error() {
    let err = Transaction::begin(&NoTx).await.err().unwrap();
    assert_eq!((err.status_code(), err.message()), (500, NO_TX_MESSAGE));
    // Through the Arc blanket impl too.
    let wrapped: Arc<dyn DatabaseAdapter> = Arc::new(Arc::new(NoTx));
    assert_eq!(
        wrapped.begin_tx().await.err().unwrap().message(),
        NO_TX_MESSAGE
    );
    assert!(!wrapped.is_connected().await);
}

#[tokio::test]
async fn arc_wrapped_adapter_delegates_every_call() {
    let fake = Fake::default();
    let outer: Arc<dyn DatabaseAdapter> = Arc::new(Arc::new(fake.clone()));
    assert!(outer.is_connected().await);
    outer.disconnect().await.unwrap();
    assert_eq!(outer.escape_id("c"), "\"c\"");
    outer.query("SELECT 1", &[]).await.unwrap();
    outer.begin_tx().await.unwrap();
    let f = FilterExpr::eq("id", 1i64);
    outer.count("t", &f, None).await.unwrap();
    outer.list("t", &f, &[], None, 0, 1).await.unwrap();
    outer
        .get("t", &f, &[], None, GetOptions::default())
        .await
        .unwrap();
    outer.insert_rows("t", &[], &[]).await.unwrap();
    outer.upsert_rows("t", &[], &[]).await.unwrap();
    outer.remove_rows("t", &f, Some("gone")).await.unwrap();
    assert_eq!(
        fake.log(),
        [
            "disconnect",
            "SELECT 1",
            "BEGIN",
            "list page=0 size=1",
            "get sd=None limit=None count=false",
            "insert",
            "upsert",
            "remove sd=Some(\"gone\")"
        ]
    );
}
