//! OQ-B adapter conformance: one behaviour contract, driven through `Model`, run against
//! every adapter as one test per scenario (`<adapter>::<scenario>`). Each scenario uses its
//! own table (`conf_<scenario>`), dropped and recreated, so scenarios are independent.
//! SQLite runs offline; the others need `docker compose up -d --wait` and
//! `cargo test -p dbmr --all-features --test conformance -- --include-ignored`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::many_single_char_names
)]

use dbmr::core::filter::{FilterExpr, Operator};
use dbmr::core::types::row;
use dbmr::{AppError, Context, DatabaseAdapter, DbValue, Model, Query, RowMap, Transaction};
use serde_json::{json, Value};
use std::sync::Arc;

const BIG: i64 = 9_007_199_254_740_993; // 2^53 + 1

/// One scenario's database handle and table.
struct Fx {
    db: Arc<dyn DatabaseAdapter>,
    table: String,
    id_rule: &'static str,
    tx: bool,
}

impl Fx {
    /// `safe`: soft delete via `is_deleted`. `meta` / `tags` are JSON object / array columns.
    fn model(&self, safe: bool) -> Model {
        let b = self.builder();
        (if safe { b.safe_delete("is_deleted") } else { b })
            .build()
            .unwrap()
    }

    fn builder(&self) -> dbmr::core::ModelBuilder {
        Model::builder(self.db.clone(), &self.table)
            .column("id", self.id_rule)
            .column("sku", "required|string")
            .column("grp", "required|integer")
            .column("name", "string|nullable")
            .column("qty", "integer")
            .column("meta", "object|nullable")
            .column("tags", "array|nullable")
            .unique(["sku", "grp"])
    }

    /// Model with soft delete and the four standard rows:
    /// (a,1,Alpha,1) (a,2,beta,2) (b,1,null,3) (c,1,Gamma,BIG).
    async fn seeded(&self) -> Model {
        let m = self.model(true);
        let s = m
            .insert_many(
                vec![
                    item("a", 1, Some("Alpha"), 1),
                    item("a", 2, Some("beta"), 2),
                    item("b", 1, None, 3),
                    item("c", 1, Some("Gamma"), BIG),
                ],
                &Context::new(),
            )
            .await
            .unwrap();
        assert_eq!(s.rows, 4, "seed");
        m
    }
}

fn item(sku: &str, grp: i64, name: Option<&str>, qty: i64) -> RowMap {
    row([
        ("sku", sku.into()),
        ("grp", grp.into()),
        ("name", name.map_or(DbValue::Null, Into::into)),
        ("qty", qty.into()),
    ])
}

async fn find(m: &Model, q: Value) -> Vec<RowMap> {
    let (q, _) = Query::from_json(&q, "id").unwrap();
    m.find(q, &Context::new()).await.unwrap().data
}

async fn one(m: &Model, sku: &str, grp: i64) -> RowMap {
    let mut r = find(m, json!({"sku": sku, "grp": grp})).await;
    assert_eq!(r.len(), 1, "expected one row for ({sku},{grp})");
    r.remove(0)
}

async fn count(m: &Model) -> u64 {
    m.count(&Query::new(), &Context::new()).await.unwrap()
}

fn int(v: &DbValue) -> i64 {
    match v {
        DbValue::Int(i) => i64::try_from(*i).unwrap(),
        DbValue::Decimal(d) => d.to_string().parse().unwrap(),
        DbValue::Text(s) => s.parse().unwrap(),
        DbValue::Float(f) => *f as i64,
        other => panic!("not an integer: {other:?}"),
    }
}

fn status(e: &AppError) -> u16 {
    e.status_code()
}

/// One HTTP call against `app`: `(status, JSON body or the raw text as a JSON string)`.
async fn http(app: &axum::Router, method: &str, uri: &str, body: Option<Value>) -> (u16, Value) {
    use tower::ServiceExt;
    let b = axum::http::Request::builder().method(method).uri(uri);
    let req = match body {
        Some(v) => b
            .header("content-type", "application/json")
            .body(axum::body::Body::from(v.to_string())),
        None => b.body(axum::body::Body::empty()),
    }
    .unwrap();
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status().as_u16();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

/// The scenarios. Each takes a fresh, empty table.
mod sc {
    use super::*;

    /// The HTTP contract is the same on every adapter: values come back with the declared
    /// column type (`"9"` -> 9 for integer, `"007"` stays text, `0/1` -> booleans, `9.00`
    /// -> 9), every declared column is present (null when unset) in declared order, and
    /// filters match the same rows. A database or driver upgrade that changes what the
    /// store returns fails here.
    pub async fn type_contract(fx: Fx) {
        const DECLARED: [&str; 9] = [
            "id",
            "sku",
            "grp",
            "name",
            "qty",
            "meta",
            "tags",
            "price",
            "is_deleted",
        ];
        let m = fx
            .builder()
            .column("price", "numeric|nullable")
            .column("is_deleted", "boolean")
            .build()
            .unwrap();
        let app = axum::Router::new().nest("/items", dbmr::create_model_router(m));
        // Declared columns in order, without the id (its type is the adapter's).
        let body = |rec: &Value| -> Value {
            let o = rec
                .as_object()
                .unwrap_or_else(|| panic!("not a record: {rec}"));
            let keys: Vec<&str> = o.keys().map(String::as_str).take(DECLARED.len()).collect();
            assert_eq!(keys, DECLARED, "declared columns first, in order: {rec}");
            Value::Object(
                DECLARED[1..]
                    .iter()
                    .map(|k| ((*k).to_string(), o[*k].clone()))
                    .collect(),
            )
        };

        // Writes: text forms become the declared type; text stays text.
        let cases = [
            (
                json!({"sku":"s1","grp":"1","name":"007","qty":"9","price":"9.00","is_deleted":"false",
                       "meta":{"a":1.50,"b":"9"},"tags":[1,"2"]}),
                json!({"sku":"s1","grp":1,"name":"007","qty":9,"price":9,"meta":{"a":1.5,"b":"9"},
                       "tags":[1,"2"],"is_deleted":false}),
            ),
            (
                json!({"sku":"s2","grp":2,"name":"9","qty":9,"price":9.5,"is_deleted":true}),
                json!({"sku":"s2","grp":2,"name":"9","qty":9,"price":9.5,"meta":null,"tags":null,
                       "is_deleted":true}),
            ),
            (
                json!({"sku":"s3","grp":3,"name":"true","qty":BIG,"price":"12.30","is_deleted":1}),
                json!({"sku":"s3","grp":3,"name":"true","qty":BIG,"price":12.3,"meta":null,"tags":null,
                       "is_deleted":true}),
            ),
            (
                json!({"sku":"s4","grp":4,"is_deleted":"0"}),
                json!({"sku":"s4","grp":4,"name":null,"qty":null,"price":null,"meta":null,"tags":null,
                       "is_deleted":false}),
            ),
        ];
        let mut ids = Vec::new();
        for (input, want) in &cases {
            let (st, rec) = http(&app, "POST", "/items/0", Some(input.clone())).await;
            assert!(st < 300, "insert {input}: {st} {rec}");
            assert_eq!(body(&rec), *want, "insert result for {input}");
            let id = rec["id"].clone();
            let id = id.as_str().map_or_else(|| id.to_string(), str::to_string);
            let (st, got) = http(&app, "GET", &format!("/items/{id}"), None).await;
            assert_eq!(st, 200);
            assert_eq!(body(&got), *want, "read back {input}");
            ids.push(id);
        }

        // Filters match the same rows on every adapter (query strings are text).
        for (qs, want) in [
            ("name=007", "s1"),
            ("name=9", "s2"),
            ("name=true", "s3"),
            ("qty=9", "s1,s2"),
            ("grp=1", "s1"),
            ("is_deleted=true", "s2,s3"),
            ("is_deleted=false", "s1,s4"),
            ("price=9.5", "s2"),
            ("price=%3E%3D9", "s1,s2,s3"),
            ("price=9", "s1"),
        ] {
            let (st, res) = http(&app, "GET", &format!("/items?{qs}&sort=sku"), None).await;
            assert_eq!(st, 200, "{qs}: {res}");
            let skus: Vec<&str> = res["data"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| r["sku"].as_str().unwrap())
                .collect();
            assert_eq!(skus.join(","), want, "filter {qs}");
            assert_eq!(res["count"], skus.len(), "count for {qs}");
        }

        // Patch converts too.
        let uri = format!("/items/{}", ids[3]);
        let (st, rec) = http(
            &app,
            "PATCH",
            &uri,
            Some(json!({"qty":"10","is_deleted":"1"})),
        )
        .await;
        assert_eq!(st, 200, "{rec}");
        assert_eq!(
            (&rec["qty"], &rec["is_deleted"]),
            (&json!(10), &json!(true))
        );

        // Bad values are 422 everywhere (never a database-dependent 500).
        for bad in [
            json!({"sku":"b","grp":9,"qty":"abc","is_deleted":false}),
            json!({"sku":"b","grp":9,"qty":9.5,"is_deleted":false}),
            json!({"sku":"b","grp":9,"is_deleted":"yes"}),
            json!({"sku":"b","grp":9,"name":7,"is_deleted":false}),
            json!({"sku":"b","grp":9,"price":"1.2.3","is_deleted":false}),
        ] {
            let (st, res) = http(&app, "POST", "/items/0", Some(bad.clone())).await;
            assert_eq!(st, 422, "{bad}: {res}");
        }

        // Declared unique keys are enforced the same way everywhere: 409, also within a batch.
        let dup = json!({"sku":"s1","grp":"1","is_deleted":false});
        let (st, res) = http(&app, "POST", "/items/0", Some(dup)).await;
        assert_eq!(
            (st, &res["message"]),
            (409, &json!("Duplicate value for unique key (sku, grp).")),
            "{res}"
        );
        let batch = json!({"data":[{"sku":"n","grp":1,"is_deleted":false},{"sku":"n","grp":1,"is_deleted":false}]});
        let (st, res) = http(&app, "POST", "/items", Some(batch)).await;
        assert_eq!(
            (st, &res["message"]),
            (
                409,
                &json!("data.1: Duplicate value for unique key (sku, grp).")
            ),
            "{res}"
        );

        // A database error does not poison the connection for the next request.
        assert!(fx
            .db
            .query("SELECT * FROM no_such_table_xyz", &[])
            .await
            .is_err());
        let (st, res) = http(&app, "GET", "/items", None).await;
        assert_eq!((st, &res["count"]), (200, &json!(4)), "{res}");

        // CSV header follows the declared order.
        let (st, csv) = http(&app, "GET", "/items?output_content_type=csv&sort=sku", None).await;
        assert_eq!(st, 200);
        let header = csv.as_str().unwrap().lines().next().unwrap().to_string();
        assert!(
            header.starts_with(&DECLARED.join(",")),
            "csv header: {header}"
        );
    }

    /// Optimistic locking through `update_where`: version 1 on insert, +1 per write,
    /// 409 on a stale version, 428 without one.
    pub async fn optimistic_lock(fx: Fx) {
        let m = fx
            .builder()
            .safe_delete("is_deleted")
            .version_column("version")
            .build()
            .unwrap();
        let ctx = Context::new();
        let rec = m
            .insert(item("a", 1, Some("x"), 1), &ctx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(int(&rec["version"]), 1, "insert sets version 1");
        let id = rec["id"].clone();

        let mut up = item("a", 1, Some("y"), 2);
        up.insert("id".into(), id.clone());
        up.insert("version".into(), 1.into());
        let rec = m.update(up.clone(), &ctx).await.unwrap().unwrap();
        assert_eq!(int(&rec["version"]), 2);
        assert_eq!(rec["name"], DbValue::Text("y".into()));

        let e = m.update(up.clone(), &ctx).await.unwrap_err();
        assert_eq!(status(&e), 409, "stale version: {e}");
        up.shift_remove("version");
        assert_eq!(status(&m.update(up, &ctx).await.unwrap_err()), 428);

        let p = row([("id", id.clone()), ("qty", 9.into()), ("version", 2.into())]);
        let rec = m.patch(p.clone(), &ctx).await.unwrap().unwrap();
        assert_eq!(int(&rec["version"]), 3);
        assert_eq!(int(&rec["qty"]), 9);
        assert_eq!(
            rec["name"],
            DbValue::Text("y".into()),
            "patch keeps other columns"
        );
        assert_eq!(status(&m.patch(p, &ctx).await.unwrap_err()), 409);
        let p = row([("id", id.clone()), ("qty", 1.into())]);
        assert_eq!(status(&m.patch(p, &ctx).await.unwrap_err()), 428);

        // Upsert on the unique key: existing row needs the version; a new row does not.
        assert_eq!(
            status(&m.upsert(item("a", 1, None, 5), &ctx).await.unwrap_err()),
            428
        );
        let mut u = item("a", 1, Some("z"), 5);
        u.insert("version".into(), 3.into());
        assert_eq!(
            int(&m.upsert(u, &ctx).await.unwrap().unwrap()["version"]),
            4
        );
        let fresh = m
            .upsert(item("n", 1, None, 1), &ctx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(int(&fresh["version"]), 1);

        // Bulk: per-row versions; the stale row fails with its index.
        let mut r0 = item("a", 1, Some("b0"), 1);
        r0.insert("id".into(), id.clone());
        r0.insert("version".into(), 4.into());
        let mut r1 = item("n", 1, Some("b1"), 1);
        r1.insert("id".into(), fresh["id"].clone());
        r1.insert("version".into(), 1.into());
        let s = m.update_many(vec![r0.clone(), r1], &ctx).await.unwrap();
        assert_eq!(s.rows, 2);
        let e = m.update_many(vec![r0], &ctx).await.unwrap_err();
        assert_eq!(status(&e), 409);
        assert!(e.message().starts_with("data.0: "), "{}", e.message());

        // Soft-deleted rows are not updated (404 semantics: None).
        m.remove(json!({"id": id.to_json()}), &ctx).await.unwrap();
        let mut gone = item("a", 1, None, 1);
        gone.insert("id".into(), id);
        gone.insert("version".into(), 5.into());
        assert!(m.update(gone, &ctx).await.unwrap().is_none());
        fx.db.ping().await.unwrap();
    }

    /// Hidden columns are written but never returned; `with_hidden` shows them.
    pub async fn hidden_columns(fx: Fx) {
        let m = fx.builder().hidden(["name"]).build().unwrap();
        let ctx = Context::new();
        let rec = m
            .insert(item("a", 1, Some("secret"), 1), &ctx)
            .await
            .unwrap()
            .unwrap();
        assert!(!rec.contains_key("name"), "insert result: {rec:?}");
        let rows = m.find(Query::new(), &ctx).await.unwrap().data;
        assert!(!rows[0].contains_key("name"));
        let by = m.by_id(rec["id"].clone(), &ctx).await.unwrap().unwrap();
        assert!(!by.contains_key("name"));
        let full = m
            .with_hidden()
            .by_id(rec["id"].clone(), &ctx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(full["name"], DbValue::Text("secret".into()));
    }

    /// App-side timestamps: insert sets both, update moves only modified.
    pub async fn auto_timestamps(fx: Fx) {
        let m = fx
            .builder()
            .created_at("created_at")
            .modified_at("updated_at")
            .build()
            .unwrap();
        let ctx = Context::new();
        let mut a = item("a", 1, None, 1);
        a.insert("created_at".into(), "1999-01-01T00:00:00Z".into()); // client value ignored
        let rec = m.insert(a, &ctx).await.unwrap().unwrap();
        let (c0, u0) = (rec["created_at"].clone(), rec["updated_at"].clone());
        assert!(!c0.is_null() && !u0.is_null(), "{rec:?}");
        assert!(
            !c0.to_string().starts_with("1999"),
            "client cannot set created_at"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        let p = row([("id", rec["id"].clone()), ("qty", 2.into())]);
        let rec = m.patch(p, &ctx).await.unwrap().unwrap();
        assert_eq!(
            rec["created_at"].to_string(),
            c0.to_string(),
            "created_at kept"
        );
        assert_ne!(
            rec["updated_at"].to_string(),
            u0.to_string(),
            "updated_at moved"
        );
    }

    /// `include`: has_many and belongs_to on the same table (`name` holds the parent id).
    pub async fn include_relations(fx: Fx) {
        let base = fx.model(false);
        let m = base
            .has_many("kids", &base, "name")
            .belongs_to("parent", &base, "name");
        let ctx = Context::new();
        let p = m
            .insert(item("p", 1, None, 1), &ctx)
            .await
            .unwrap()
            .unwrap();
        let pid = p["id"].to_string();
        for (i, sku) in ["k1", "k2"].iter().enumerate() {
            m.insert(item(sku, i64::try_from(i).unwrap(), Some(&pid), 1), &ctx)
                .await
                .unwrap();
        }
        let q = Query::new().eq("sku", "p").include(["kids"]);
        let rows = m.find(q, &ctx).await.unwrap().data;
        let DbValue::Json(Value::Array(kids)) = &rows[0]["kids"] else {
            panic!("kids not attached: {:?}", rows[0]);
        };
        assert_eq!(kids.len(), 2);
        let q = Query::new().eq("sku", "k1").include(["parent"]);
        let k = m.find_one(q, &ctx).await.unwrap().unwrap();
        let DbValue::Json(parent) = &k["parent"] else {
            panic!("parent not attached: {k:?}");
        };
        assert_eq!(parent["sku"], "p");
        let bad = m
            .find(Query::new().include(["nope"]), &ctx)
            .await
            .unwrap_err();
        assert_eq!(status(&bad), 422);
    }

    /// Typed view round trip.
    pub async fn typed_roundtrip(fx: Fx) {
        #[derive(serde::Serialize, serde::Deserialize, Debug)]
        struct Item {
            #[serde(skip_serializing_if = "Option::is_none")]
            id: Option<Value>,
            sku: String,
            grp: i64,
            name: Option<String>,
            qty: Option<i64>,
        }
        let m = fx.model(false).typed::<Item>();
        let ctx = Context::new();
        let it = Item {
            id: None,
            sku: "t".into(),
            grp: 1,
            name: None,
            qty: Some(BIG),
        };
        let saved = m.insert(&it, &ctx).await.unwrap().unwrap();
        assert!(saved.id.is_some());
        assert_eq!(saved.qty, Some(BIG));
        let page = m.list(Query::new(), &ctx).await.unwrap();
        assert_eq!((page.count, page.data[0].sku.as_str()), (1, "t"));
    }

    pub async fn insert_single(fx: Fx) {
        let m = fx.model(true);
        let rec = m
            .insert(item("a", 1, Some("Alpha"), 1), &Context::new())
            .await
            .unwrap()
            .expect("insert returns the record");
        assert_ne!(rec["id"], DbValue::Null, "id assigned by the database");
        assert_eq!(rec["name"], DbValue::Text("Alpha".into()));
        assert_eq!(int(&rec["qty"]), 1);
        assert_eq!(count(&m).await, 1);
    }

    pub async fn insert_many(fx: Fx) {
        let m = fx.seeded().await; // asserts the bulk summary
        assert_eq!(count(&m).await, 4);
        assert_eq!(
            int(&one(&m, "c", 1).await["qty"]),
            BIG,
            "big int round trip"
        );
        assert_eq!(one(&m, "b", 1).await["name"], DbValue::Null, "null stored");
    }

    pub async fn by_id(fx: Fx) {
        let m = fx.seeded().await;
        let ctx = Context::new();
        let a = one(&m, "a", 2).await;
        let got = m.by_id(a["id"].clone(), &ctx).await.unwrap().unwrap();
        assert_eq!(got["name"], DbValue::Text("beta".into()));
        m.remove(json!({"sku": "a", "grp": 2}), &ctx).await.unwrap();
        assert!(
            m.by_id(a["id"].clone(), &ctx).await.unwrap().is_none(),
            "soft-deleted row is not found by id"
        );
    }

    pub async fn find_filters(fx: Fx) {
        let m = fx.seeded().await;
        assert_eq!(find(&m, json!({"name": null})).await.len(), 1, "IS NULL");
        assert_eq!(find(&m, json!({"qty": "in(2,3)"})).await.len(), 2, "IN");
        // `?qty=in()` is a literal (Node parity); an empty IN list from code matches nothing.
        let empty_in = Query::new().and("qty", Operator::In, DbValue::Json(json!([])));
        assert!(
            m.find(empty_in, &Context::new())
                .await
                .unwrap()
                .data
                .is_empty(),
            "empty IN"
        );
        assert_eq!(find(&m, json!({"qty": ">=3"})).await.len(), 2, ">=");
        assert_eq!(find(&m, json!({"qty": "<3"})).await.len(), 2, "<");
        assert_eq!(
            find(&m, json!({"name": "%ALP%"})).await.len(),
            1,
            "LIKE is case-insensitive"
        );
        assert_eq!(find(&m, json!({"sku": "a"})).await.len(), 2, "equality");
    }

    pub async fn list_paging(fx: Fx) {
        let m = fx.seeded().await;
        let ctx = Context::new();
        let page = m
            .list(Query::new().sort("-qty").page(0).size(2), &ctx)
            .await
            .unwrap();
        assert_eq!(page.data.len(), 2, "page length");
        assert_eq!(page.count, 4, "count is the total, not the page length");
        assert_eq!(int(&page.data[0]["qty"]), BIG, "sort desc");
        let page1 = m
            .list(Query::new().sort("-qty").page(1).size(2), &ctx)
            .await
            .unwrap();
        assert_eq!(int(&page1.data[0]["qty"]), 2, "second page");
        assert_eq!(int(&page1.data[1]["qty"]), 1, "sorted within the page");
    }

    pub async fn update_single(fx: Fx) {
        let m = fx.seeded().await;
        let mut r = one(&m, "a", 1).await;
        r.insert("name".into(), "Alef".into());
        r.insert("qty".into(), 11.into());
        r.shift_remove("is_deleted");
        m.update(r, &Context::new()).await.unwrap();
        let a = one(&m, "a", 1).await;
        assert_eq!(a["name"], DbValue::Text("Alef".into()));
        assert_eq!(int(&a["qty"]), 11);
        assert_eq!(count(&m).await, 4, "update added no row");
    }

    pub async fn update_many(fx: Fx) {
        let m = fx.seeded().await;
        let rows: Vec<RowMap> = find(&m, json!({"sku": "a"}))
            .await
            .into_iter()
            .map(|mut r| {
                let q = int(&r["qty"]);
                r.insert("qty".into(), (q + 100).into());
                r.shift_remove("is_deleted");
                r
            })
            .collect();
        let s = m.update_many(rows, &Context::new()).await.unwrap();
        assert_eq!(s.rows, 2, "bulk update summary");
        assert_eq!(int(&one(&m, "a", 1).await["qty"]), 101);
        assert_eq!(int(&one(&m, "a", 2).await["qty"]), 102);
        assert_eq!(
            int(&one(&m, "b", 1).await["qty"]),
            3,
            "other rows untouched"
        );
        assert_eq!(count(&m).await, 4);
    }

    pub async fn patch_partial(fx: Fx) {
        let m = fx.seeded().await;
        let b = one(&m, "b", 1).await;
        let mut p = RowMap::new();
        p.insert("id".into(), b["id"].clone());
        p.insert("name".into(), "Bee".into());
        m.patch(p, &Context::new()).await.unwrap();
        let b2 = one(&m, "b", 1).await;
        assert_eq!(b2["name"], DbValue::Text("Bee".into()), "patched");
        assert_eq!(int(&b2["qty"]), 3, "patch kept qty");
    }

    /// S7: composite unique key, so upsert must touch only the row matching ALL key columns.
    pub async fn upsert_single(fx: Fx) {
        let m = fx.seeded().await;
        let ctx = Context::new();
        m.upsert(item("a", 1, Some("Alpha"), 10), &ctx)
            .await
            .unwrap();
        assert_eq!(int(&one(&m, "a", 1).await["qty"]), 10, "updated (a,1)");
        assert_eq!(int(&one(&m, "a", 2).await["qty"]), 2, "left (a,2) alone");
        assert_eq!(count(&m).await, 4, "no row added");
        m.upsert(item("d", 1, Some("Delta"), 4), &ctx)
            .await
            .unwrap();
        assert_eq!(count(&m).await, 5, "new key inserts");
    }

    pub async fn upsert_many(fx: Fx) {
        let m = fx.seeded().await;
        m.upsert_many(
            vec![
                item("a", 2, Some("beta"), 20),
                item("e", 1, Some("Echo"), 5),
            ],
            &Context::new(),
        )
        .await
        .unwrap();
        assert_eq!(
            int(&one(&m, "a", 2).await["qty"]),
            20,
            "existing key updated"
        );
        assert_eq!(int(&one(&m, "e", 1).await["qty"]), 5, "new key inserted");
        assert_eq!(int(&one(&m, "a", 1).await["qty"]), 1, "other row untouched");
        assert_eq!(count(&m).await, 5);
    }

    /// Soft delete: flag set, row hidden from reads and counts, still stored.
    pub async fn remove_soft(fx: Fx) {
        let m = fx.seeded().await;
        let r = m
            .remove(json!({"sku": "b"}), &Context::new())
            .await
            .unwrap();
        assert_eq!(r.rows, 1, "soft delete summary");
        assert!(find(&m, json!({"sku": "b"})).await.is_empty(), "hidden");
        assert_eq!(count(&m).await, 3, "count hides it");
        let f = Query::from_json(&json!({"sku": "b"}), "id")
            .unwrap()
            .0
            .filter;
        let raw = fx
            .db
            .get(
                &fx.table,
                &f,
                &[],
                None,
                dbmr::core::types::GetOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(raw.data.len(), 1, "still stored");
    }

    /// Bulk delete by filter (several rows).
    pub async fn remove_many(fx: Fx) {
        let m = fx.seeded().await;
        let r = m.remove(json!({"grp": 1}), &Context::new()).await.unwrap();
        assert_eq!(r.rows, 3, "bulk delete summary");
        assert_eq!(count(&m).await, 1);
        assert_eq!(one(&m, "a", 2).await["name"], DbValue::Text("beta".into()));
    }

    /// Without `safe_delete`, rows are physically removed.
    pub async fn remove_hard(fx: Fx) {
        fx.seeded().await;
        let m = fx.model(false);
        let r = m
            .remove(json!({"sku": "a"}), &Context::new())
            .await
            .unwrap();
        assert_eq!(r.rows, 2, "hard delete summary");
        let all = fx
            .db
            .get(
                &fx.table,
                &FilterExpr::empty(),
                &[],
                None,
                dbmr::core::types::GetOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(all.data.len(), 2, "rows physically gone");
    }

    pub async fn mass_delete_guard(fx: Fx) {
        let m = fx.seeded().await;
        assert!(
            m.remove(json!({}), &Context::new()).await.is_err(),
            "refused"
        );
        assert_eq!(count(&m).await, 4, "nothing deleted");
    }

    /// JSON object/array columns come back as JSON on every adapter, whether the database
    /// stores them typed (MySQL JSON, Postgres JSONB) or as text (MariaDB, SQLite, ...).
    pub async fn json_columns(fx: Fx) {
        let m = fx.model(true);
        let ctx = Context::new();
        let meta = json!({"color": "red", "size": {"w": 2, "h": 3}, "ok": true});
        let tags = json!(["x", "y", 3]);
        let mut r = item("j", 1, Some("Json"), 1);
        r.insert("meta".into(), DbValue::Json(meta.clone()));
        r.insert("tags".into(), DbValue::Json(tags.clone()));
        let rec = m.insert(r, &ctx).await.unwrap().unwrap();
        assert_eq!(
            rec["meta"],
            DbValue::Json(meta.clone()),
            "insert returns object"
        );
        assert_eq!(
            rec["tags"],
            DbValue::Json(tags.clone()),
            "insert returns array"
        );

        let got = m.by_id(rec["id"].clone(), &ctx).await.unwrap().unwrap();
        assert_eq!(got["meta"], DbValue::Json(meta.clone()), "by_id object");
        let listed = find(&m, json!({"sku": "j"})).await;
        assert_eq!(listed[0]["tags"], DbValue::Json(tags.clone()), "find array");

        // Bulk insert with JSON, and a row with JSON columns null.
        let mut r2 = item("k", 1, None, 2);
        r2.insert("meta".into(), DbValue::Json(json!({"n": 1})));
        r2.insert("tags".into(), DbValue::Json(json!([])));
        m.insert_many(vec![r2, item("l", 1, None, 3)], &ctx)
            .await
            .unwrap();
        let k = one(&m, "k", 1).await;
        assert_eq!(k["meta"], DbValue::Json(json!({"n": 1})));
        assert_eq!(k["tags"], DbValue::Json(json!([])), "empty array");
        let l = one(&m, "l", 1).await;
        assert!(
            l.get("meta").is_none_or(DbValue::is_null),
            "null object stays null"
        );

        // Patch replaces the object.
        let mut p = RowMap::new();
        p.insert("id".into(), rec["id"].clone());
        p.insert("meta".into(), DbValue::Json(json!({"color": "blue"})));
        let patched = m.patch(p, &ctx).await.unwrap().unwrap();
        assert_eq!(patched["meta"], DbValue::Json(json!({"color": "blue"})));
        assert_eq!(patched["tags"], DbValue::Json(tags), "patch kept the array");

        // Wrong shape is a validation error.
        let mut bad = item("m", 1, None, 1);
        bad.insert("meta".into(), DbValue::Json(json!([1])));
        assert!(
            m.insert(bad, &ctx).await.is_err(),
            "array in an object column"
        );
    }

    /// Transactions: commit and rollback where supported, the documented error elsewhere.
    pub async fn transactions(fx: Fx) {
        let m = fx.model(true);
        let ctx = Context::new();
        match Transaction::begin(fx.db.as_ref()).await {
            Ok(t) => {
                assert!(fx.tx, "unexpected transaction support");
                m.with_tx(&t)
                    .insert(item("r", 1, None, 1), &ctx)
                    .await
                    .unwrap();
                t.rollback().await.unwrap();
                assert_eq!(count(&m).await, 0, "rollback discards");
                let t = Transaction::begin(fx.db.as_ref()).await.unwrap();
                m.with_tx(&t)
                    .insert(item("c", 1, None, 1), &ctx)
                    .await
                    .unwrap();
                t.commit().await.unwrap();
                assert_eq!(count(&m).await, 1, "commit keeps");
            }
            Err(e) => {
                assert!(!fx.tx, "begin failed: {e}");
                assert!(e.to_string().contains(dbmr::core::adapter::NO_TX_MESSAGE));
            }
        }
    }
}

/// One `#[tokio::test]` per scenario for an adapter. `#[$attr]` goes on every test (the
/// live adapters' `#[ignore]`; SQLite passes a no-op). `$setup` gets the table name and returns
/// the adapter with that table empty and current.
macro_rules! suite {
    ($name:ident, $id:literal, $tx:literal, #[$attr:meta] |$t:ident| $setup:expr) => {
        mod $name {
            use super::*;

            async fn fx(scenario: &str) -> Fx {
                let $t = format!("conf_{scenario}");
                let db: Arc<dyn DatabaseAdapter> = $setup;
                Fx { db, table: $t, id_rule: $id, tx: $tx }
            }

            suite!(@tests [$attr] insert_single insert_many by_id find_filters
                list_paging update_single update_many patch_partial upsert_single upsert_many
                remove_soft remove_many remove_hard mass_delete_guard json_columns transactions
                optimistic_lock hidden_columns auto_timestamps include_relations typed_roundtrip
                type_contract);
        }
    };
    (@tests [$attr:meta] $($s:ident)*) => {
        $(
            #[$attr]
            #[tokio::test]
            async fn $s() {
                sc::$s(fx(stringify!($s)).await).await;
            }
        )*
    };
}

/// SQL DDL for one scenario table. `j` is the JSON column type.
#[allow(clippy::too_many_arguments)]
fn ddl(
    t: &str,
    id: &str,
    text: &str,
    int: &str,
    big: &str,
    flag: &str,
    j: &str,
    ts: &str,
    dec: &str,
) -> String {
    format!(
        "CREATE TABLE {t} (id {id}, sku {text} NOT NULL, grp {int} NOT NULL, name {text},
         qty {big}, meta {j}, tags {j}, is_deleted {flag}, version {int} DEFAULT 1 NOT NULL,
         created_at {ts}, updated_at {ts}, price {dec}, CONSTRAINT uq_{t} UNIQUE (sku, grp))"
    )
}

/// Drop then create, so a schema change never meets a stale table.
async fn recreate(db: &dyn DatabaseAdapter, drop: &str, create: &str) {
    db.query(drop, &[]).await.unwrap();
    db.query(create, &[]).await.unwrap();
}

suite!(
    sqlite,
    "integer",
    true,
    #[cfg_attr(any(), ignore)]
    |t| {
        let db = dbmr::adapters::SqliteAdapter::connect("sqlite::memory:")
            .await
            .unwrap();
        let c = ddl(
            &t,
            "INTEGER PRIMARY KEY AUTOINCREMENT",
            "TEXT",
            "INTEGER",
            "INTEGER",
            "INTEGER NOT NULL DEFAULT 0",
            "TEXT",
            "TEXT",
            "NUMERIC",
        );
        db.query(&c, &[]).await.unwrap();
        db
    }
);

#[cfg(feature = "postgres")]
const PG_ID: &str = "BIGSERIAL PRIMARY KEY";

#[cfg(feature = "postgres")]
suite!(
    postgres,
    "integer",
    true,
    #[ignore = "requires live PostgreSQL"]
    |t| {
        let url = std::env::var("DATABASE_URL")
            .unwrap_or_else(|_| "postgres://postgres:password@127.0.0.1:5432/test_db".into());
        let db = dbmr::adapters::PostgresAdapter::connect(&url)
            .await
            .unwrap();
        recreate(
            db.as_ref(),
            &format!("DROP TABLE IF EXISTS {t}"),
            &ddl(
                &t,
                PG_ID,
                "TEXT",
                "INT",
                "BIGINT",
                "BOOLEAN NOT NULL DEFAULT FALSE",
                "JSONB",
                "TIMESTAMPTZ",
                "NUMERIC(12,2)",
            ),
        )
        .await;
        db
    }
);

#[cfg(feature = "cockroachdb")]
suite!(
    cockroachdb,
    "integer",
    true,
    #[ignore = "requires live CockroachDB"]
    |t| {
        let url = std::env::var("COCKROACH_URL")
            .unwrap_or_else(|_| "postgres://root@127.0.0.1:26257/defaultdb?sslmode=disable".into());
        let db = dbmr::adapters::CockroachAdapter::connect(&url)
            .await
            .unwrap();
        recreate(
            db.as_ref(),
            &format!("DROP TABLE IF EXISTS {t}"),
            &ddl(
                &t,
                PG_ID,
                "TEXT",
                "INT",
                "BIGINT",
                "BOOLEAN NOT NULL DEFAULT FALSE",
                "JSONB",
                "TIMESTAMPTZ",
                "NUMERIC(12,2)",
            ),
        )
        .await;
        db
    }
);

#[cfg(feature = "mysql")]
fn my_ddl(t: &str) -> String {
    ddl(
        t,
        "BIGINT AUTO_INCREMENT PRIMARY KEY",
        "VARCHAR(50)",
        "INT",
        "BIGINT",
        "TINYINT NOT NULL DEFAULT 0",
        "JSON",
        "DATETIME(3)",
        "DECIMAL(12,2)",
    )
}

#[cfg(feature = "mysql")]
suite!(
    mysql,
    "integer",
    true,
    #[ignore = "requires live MySQL"]
    |t| {
        let url = std::env::var("MYSQL_URL")
            .unwrap_or_else(|_| "mysql://root:password@127.0.0.1:3308/test_db".into());
        let db = dbmr::adapters::MySqlAdapter::connect(&url).await.unwrap();
        recreate(
            db.as_ref(),
            &format!("DROP TABLE IF EXISTS {t}"),
            &my_ddl(&t),
        )
        .await;
        db
    }
);

// MariaDB `JSON` is an alias for LONGTEXT + CHECK(JSON_VALID): values come back as text.
#[cfg(feature = "mariadb")]
suite!(
    mariadb,
    "integer",
    true,
    #[ignore = "requires live MariaDB"]
    |t| {
        let url = std::env::var("MARIADB_URL")
            .unwrap_or_else(|_| "mysql://root:password@127.0.0.1:3309/test_db".into());
        let db = dbmr::adapters::MariaDbAdapter::connect(&url).await.unwrap();
        recreate(
            db.as_ref(),
            &format!("DROP TABLE IF EXISTS {t}"),
            &my_ddl(&t),
        )
        .await;
        db
    }
);

#[cfg(feature = "mssql")]
suite!(
    mssql,
    "integer",
    true,
    #[ignore = "requires live MSSQL"]
    |t| {
        let db = dbmr::adapters::MssqlAdapter::connect_host(
            "127.0.0.1",
            1433,
            "sa",
            "Password123!",
            "master",
            true,
        )
        .await
        .unwrap();
        recreate(
            db.as_ref(),
            &format!("DROP TABLE IF EXISTS {t}"),
            &ddl(
                &t,
                "BIGINT IDENTITY PRIMARY KEY",
                "NVARCHAR(50)",
                "INT",
                "BIGINT",
                "BIT NOT NULL DEFAULT 0",
                "NVARCHAR(MAX)",
                "DATETIME2",
                "DECIMAL(12,2)",
            ),
        )
        .await;
        db
    }
);

// Oracle identifiers are unquoted (upper case); the adapter upper-cases names.
#[cfg(feature = "oracle")]
suite!(
    oracle,
    "integer",
    true,
    #[ignore = "requires live Oracle"]
    |t| {
        let db = dbmr::adapters::OracleAdapter::connect(
            "127.0.0.1",
            1521,
            "FREEPDB1",
            "SYSTEM",
            "oracle",
        )
        .await
        .unwrap();
        recreate(
            db.as_ref(),
            &format!("DROP TABLE IF EXISTS {t}"),
            &ddl(
                &t,
                "NUMBER(19) GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY",
                "VARCHAR2(50)",
                "NUMBER(10)",
                "NUMBER(19)",
                "NUMBER(1) DEFAULT 0 NOT NULL",
                "VARCHAR2(4000)",
                "TIMESTAMP",
                "NUMBER(12,2)",
            ),
        )
        .await;
        db
    }
);

#[cfg(feature = "mongodb")]
suite!(
    mongodb,
    "string",
    false,
    #[ignore = "requires live MongoDB"]
    |t| {
        let url = std::env::var("MONGO_URL").unwrap_or_else(|_| "mongodb://127.0.0.1:27017".into());
        let db = dbmr::adapters::MongoAdapter::connect(&url, "test_db")
            .await
            .unwrap();
        db.remove_rows(&t, &FilterExpr::empty(), None)
            .await
            .unwrap();
        db
    }
);

#[cfg(feature = "redis")]
suite!(
    redis,
    "integer",
    false,
    #[ignore = "requires live Redis"]
    |t| {
        let url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".into());
        let db = dbmr::adapters::RedisAdapter::connect(&url).unwrap();
        db.remove_rows(&t, &FilterExpr::empty(), None)
            .await
            .unwrap();
        db
    }
);

#[cfg(feature = "dynamodb")]
suite!(
    dynamodb,
    "string",
    false,
    #[ignore = "requires live DynamoDB Local"]
    |t| {
        let db = dbmr::adapters::DynamoDbAdapter::new("http://127.0.0.1:8000");
        db.ensure_table(&t, "id").await.unwrap();
        db.remove_rows(&t, &FilterExpr::empty(), None)
            .await
            .unwrap();
        db
    }
);
