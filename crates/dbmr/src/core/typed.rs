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
