//! Typed view over a [`Model`]: the same operations, taking and returning serde types.
//!
//! ```no_run
//! # async fn demo(notes: dbmr::Model) -> dbmr::Result<()> {
//! #[derive(serde::Serialize, serde::Deserialize)]
//! struct Note { id: Option<i64>, title: String }
//!
//! let notes = notes.typed::<Note>();
//! let ctx = dbmr::Context::new();
//! let saved = notes.insert(&Note { id: None, title: "hi".into() }, &ctx).await?;
//! let page = notes.list(dbmr::Query::new(), &ctx).await?;
//! # Ok(()) }
//! ```

use crate::core::error::{AppError, Result};
use crate::core::hooks::Context;
use crate::core::model::Model;
use crate::core::query::Query;
use crate::core::types::{json_to_row, row_to_json, DbValue, RowMap, WriteSummary};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;
use std::marker::PhantomData;

/// A page of typed records.
#[derive(Debug, Clone)]
pub struct Page<T> {
    pub data: Vec<T>,
    /// Total match count (see [`Model::list`]).
    pub count: u64,
}

/// Returned by [`Model::typed`].
pub struct TypedModel<T> {
    model: Model,
    _t: PhantomData<fn() -> T>,
}

impl<T> Clone for TypedModel<T> {
    fn clone(&self) -> Self {
        Self::new(self.model.clone())
    }
}

impl<T> TypedModel<T> {
    pub(crate) fn new(model: Model) -> Self {
        Self {
            model,
            _t: PhantomData,
        }
    }

    /// The underlying row-based model.
    #[must_use]
    pub fn model(&self) -> &Model {
        &self.model
    }
}

/// `T` → row. On insert, `null` fields are dropped so database defaults apply.
fn to_row<T: Serialize>(v: &T, drop_nulls: bool) -> Result<RowMap> {
    let mut j = serde_json::to_value(v)
        .map_err(|e| AppError::UnprocessableEntity(format!("Cannot serialize record: {e}")))?;
    let Value::Object(map) = &mut j else {
        return Err(AppError::UnprocessableEntity(
            "A typed record must serialize to a JSON object".into(),
        ));
    };
    if drop_nulls {
        map.retain(|_, v| !v.is_null());
    }
    Ok(json_to_row(&j))
}

fn from_row<T: DeserializeOwned>(r: &RowMap) -> Result<T> {
    serde_json::from_value(row_to_json(r))
        .map_err(|e| AppError::Internal(format!("Cannot decode row into the typed record: {e}")))
}

fn opt<T: DeserializeOwned>(r: Option<RowMap>) -> Result<Option<T>> {
    r.map(|r| from_row(&r)).transpose()
}

impl<T: Serialize + DeserializeOwned> TypedModel<T> {
    /// See [`Model::insert`]. `None` fields are omitted so column defaults apply.
    pub async fn insert(&self, v: &T, ctx: &Context) -> Result<Option<T>> {
        opt(self.model.insert(to_row(v, true)?, ctx).await?)
    }

    pub async fn insert_many(&self, vs: &[T], ctx: &Context) -> Result<WriteSummary> {
        let rows = vs.iter().map(|v| to_row(v, true)).collect::<Result<_>>()?;
        self.model.insert_many(rows, ctx).await
    }

    pub async fn update(&self, v: &T, ctx: &Context) -> Result<Option<T>> {
        opt(self.model.update(to_row(v, false)?, ctx).await?)
    }

    pub async fn update_many(&self, vs: &[T], ctx: &Context) -> Result<WriteSummary> {
        let rows = vs.iter().map(|v| to_row(v, false)).collect::<Result<_>>()?;
        self.model.update_many(rows, ctx).await
    }

    pub async fn upsert(&self, v: &T, ctx: &Context) -> Result<Option<T>> {
        opt(self.model.upsert(to_row(v, false)?, ctx).await?)
    }

    pub async fn upsert_many(&self, vs: &[T], ctx: &Context) -> Result<WriteSummary> {
        let rows = vs.iter().map(|v| to_row(v, false)).collect::<Result<_>>()?;
        self.model.upsert_many(rows, ctx).await
    }

    /// Partial update from any serializable value (a `json!({...})` or a patch struct);
    /// must contain the primary key.
    pub async fn patch(&self, changes: &impl Serialize, ctx: &Context) -> Result<Option<T>> {
        opt(self.model.patch(to_row(changes, false)?, ctx).await?)
    }

    pub async fn by_id(&self, id: impl Into<DbValue>, ctx: &Context) -> Result<Option<T>> {
        opt(self.model.by_id(id, ctx).await?)
    }

    pub async fn find(&self, q: Query, ctx: &Context) -> Result<Page<T>> {
        let r = self.model.find(q, ctx).await?;
        Ok(Page {
            data: r.data.iter().map(from_row).collect::<Result<_>>()?,
            count: r.count,
        })
    }

    pub async fn find_one(&self, q: Query, ctx: &Context) -> Result<Option<T>> {
        opt(self.model.find_one(q, ctx).await?)
    }

    pub async fn list(&self, q: Query, ctx: &Context) -> Result<Page<T>> {
        let r = self.model.list(q, ctx).await?;
        Ok(Page {
            data: r.data.iter().map(from_row).collect::<Result<_>>()?,
            count: r.count,
        })
    }
}

#[cfg(all(test, feature = "sqlite"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::adapters::SqliteAdapter;
    use crate::core::adapter::DatabaseAdapter;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct Item {
        id: Option<i64>,
        name: String,
        val: Option<i64>,
    }

    #[derive(Debug, Serialize, Deserialize)]
    struct Incompatible {
        pub missing_bool: bool,
    }

    async fn setup_typed_model() -> (TypedModel<Item>, Context) {
        let db = SqliteAdapter::connect("sqlite::memory:").await.unwrap();
        db.query(
            "CREATE TABLE items (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, val INTEGER)",
            &[],
        )
        .await
        .unwrap();

        let model = Model::builder(db, "items")
            .column("id", "integer")
            .column("name", "required|string")
            .column("val", "integer|nullable")
            .build()
            .unwrap();

        (model.typed::<Item>(), Context::new())
    }

    #[tokio::test]
    async fn test_typed_model_full_lifecycle() {
        let (tm, ctx) = setup_typed_model().await;

        // Clone and model()
        let tm_clone = tm.clone();
        assert_eq!(tm_clone.model().table_name(), "items");

        // insert
        let ins = tm
            .insert(
                &Item {
                    id: None,
                    name: "item1".into(),
                    val: Some(10),
                },
                &ctx,
            )
            .await
            .unwrap()
            .expect("inserted item");
        assert_eq!(ins.id, Some(1));
        assert_eq!(ins.name, "item1");
        assert_eq!(ins.val, Some(10));

        // insert_many
        let summary = tm
            .insert_many(
                &[
                    Item {
                        id: None,
                        name: "item2".into(),
                        val: Some(20),
                    },
                    Item {
                        id: None,
                        name: "item3".into(),
                        val: Some(30),
                    },
                ],
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(summary.rows, 2);

        // by_id
        let item1 = tm.by_id(1, &ctx).await.unwrap().expect("by_id 1");
        assert_eq!(item1.name, "item1");
        assert!(tm.by_id(999, &ctx).await.unwrap().is_none());

        // find
        let page = tm.find(Query::new(), &ctx).await.unwrap();
        assert_eq!(page.data.len(), 3);
        assert_eq!(page.count, 3);
        let _ = format!("{page:?}");
        let _ = page.clone();

        // find_one
        let item2 = tm
            .find_one(Query::new().eq("name", "item2"), &ctx)
            .await
            .unwrap();
        assert_eq!(item2.unwrap().name, "item2");

        let none_item = tm
            .find_one(Query::new().eq("name", "nonexistent"), &ctx)
            .await
            .unwrap();
        assert!(none_item.is_none());

        // list
        let list_page = tm.list(Query::new().limit(2), &ctx).await.unwrap();
        assert_eq!(list_page.data.len(), 2);
        assert_eq!(list_page.count, 3);

        // update
        let updated = tm
            .update(
                &Item {
                    id: Some(1),
                    name: "item1-upd".into(),
                    val: Some(15),
                },
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(updated.unwrap().name, "item1-upd");

        // update_many
        let upd_many = tm
            .update_many(
                &[
                    Item {
                        id: Some(2),
                        name: "item2-upd".into(),
                        val: Some(25),
                    },
                    Item {
                        id: Some(3),
                        name: "item3-upd".into(),
                        val: Some(35),
                    },
                ],
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(upd_many.rows, 2);

        // patch
        let patched = tm
            .patch(&serde_json::json!({ "id": 1, "val": 100 }), &ctx)
            .await
            .unwrap();
        assert_eq!(patched.unwrap().val, Some(100));

        // upsert
        let upserted = tm
            .upsert(
                &Item {
                    id: Some(1),
                    name: "item1-upsert".into(),
                    val: Some(111),
                },
                &ctx,
            )
            .await
            .unwrap();
        assert_eq!(upserted.unwrap().name, "item1-upsert");

        // upsert_many
        let upsert_many = tm
            .upsert_many(
                &[
                    Item {
                        id: Some(2),
                        name: "item2-upsert".into(),
                        val: Some(222),
                    },
                    Item {
                        id: Some(4),
                        name: "item4-new".into(),
                        val: Some(444),
                    },
                ],
                &ctx,
            )
            .await
            .unwrap();
        assert!(upsert_many.rows >= 2);

        // to_row error
        let err = tm.patch(&"not an object", &ctx).await.unwrap_err();
        match err {
            AppError::UnprocessableEntity(msg) => {
                assert!(msg.contains("must serialize to a JSON object"));
            }
            other => panic!("expected UnprocessableEntity, got {other:?}"),
        }

        // from_row error with Incompatible type
        let incomp = tm.model().typed::<Incompatible>();
        let err2 = incomp.find(Query::new(), &ctx).await.unwrap_err();
        match err2 {
            AppError::Internal(msg) => {
                assert!(msg.contains("Cannot decode row into the typed record"));
            }
            other => panic!("expected Internal error, got {other:?}"),
        }

        let err3 = incomp.list(Query::new(), &ctx).await.unwrap_err();
        match err3 {
            AppError::Internal(msg) => {
                assert!(msg.contains("Cannot decode row into the typed record"));
            }
            other => panic!("expected Internal error, got {other:?}"),
        }
    }
}
