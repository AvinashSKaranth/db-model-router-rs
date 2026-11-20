//! Model lifecycle hooks (§23).
//!
//! ```
//! use dbmr::core::hooks::ModelHooks;
//! use dbmr::core::types::DbValue;
//!
//! let hooks = ModelHooks::new()
//!     .before_save(|mut row, ctx| async move {
//!         // ctx.user is whatever the HTTP layer (or your code) put there
//!         if let Some(tenant) = ctx.user.get("tenant_id").and_then(|v| v.as_str()) {
//!             row.insert("tenant_id".into(), DbValue::from(tenant));
//!         }
//!         Ok(row)
//!     })
//!     .after_find(|rows, _ctx| async move { Ok(rows) });
//! ```
//!
//! Order on single writes: `before_save` → `before_create|before_update|before_upsert`
//! → strip/validate/encrypt → adapter → decrypt → `after_save` →
//! `after_create|after_update|after_upsert`. Bulk writes run before-hooks per row and
//! `after_bulk` once with the adapter summary. Returning `Err` aborts the operation.

use crate::core::error::Result;
use crate::core::filter::FilterExpr;
use crate::core::types::{RowMap, WriteSummary};
use serde_json::{Map, Value};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Per-call context: who is calling and which rows the call may touch.
///
/// The HTTP router builds it from the request: `user` from a [`CurrentUser`] extension,
/// `scope` from a [`Scope`] extension, `request` from the method, path, params, query,
/// headers and body. Code outside HTTP (jobs, workers) builds it directly.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub user: Value,
    pub request: Value,
    /// Row restriction, e.g. `{"tenant_id": "A"}` or `{"tenant_id": ["A", "B"]}`. On the
    /// columns a model declares, every read, update and delete is limited to matching
    /// rows (a list means IN), inserts get the value (a list: the row must name one),
    /// and includes and `belongs_to` checks stay inside it. Empty: no restriction.
    pub scope: Map<String, Value>,
    /// Trusted call that may run without the columns a model requires with
    /// `require_scope` (cross-scope jobs). Set by [`Context::system`].
    pub system: bool,
}

impl Context {
    /// No user, no scope.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A trusted cross-scope call (jobs, migrations): passes `require_scope`. Add
    /// [`Context::with_scope`] to confine it anyway.
    #[must_use]
    pub fn system() -> Self {
        Self {
            system: true,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn with_user(user: Value) -> Self {
        Self {
            user,
            ..Self::default()
        }
    }

    /// Confine the call to these column values (see [`Context::scope`]):
    /// `.with_scope([("tenant_id", json!("A"))])`.
    #[must_use]
    pub fn with_scope<K: Into<String>, V: Into<Value>>(
        mut self,
        scope: impl IntoIterator<Item = (K, V)>,
    ) -> Self {
        self.scope = scope
            .into_iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        self
    }
}

/// Request extension with the authenticated user; hooks see it as `ctx.user`.
#[derive(Debug, Clone, Default)]
pub struct CurrentUser(pub Value);

/// Request extension with the row restriction for this request; the router copies it to
/// [`Context::scope`]. Set it in your auth middleware:
/// `req.extensions_mut().insert(Scope::new([("tenant_id", json!(user.tenant_id))]))`.
#[derive(Debug, Clone, Default)]
pub struct Scope(pub Map<String, Value>);

impl Scope {
    #[must_use]
    pub fn new<K: Into<String>, V: Into<Value>>(scope: impl IntoIterator<Item = (K, V)>) -> Self {
        Self(
            scope
                .into_iter()
                .map(|(k, v)| (k.into(), v.into()))
                .collect(),
        )
    }
}

pub type BoxFut<R> = Pin<Box<dyn Future<Output = Result<R>> + Send>>;
pub type HookFn<T> = Arc<dyn Fn(T, Context) -> BoxFut<T> + Send + Sync>;
pub type BulkHookFn = Arc<dyn Fn(BulkOp, WriteSummary, Context) -> BoxFut<()> + Send + Sync>;

/// Which bulk operation finished (argument of `after_bulk`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BulkOp {
    Insert,
    Update,
    Upsert,
}

#[derive(Default, Clone)]
pub struct ModelHooks {
    pub before_save: Option<HookFn<RowMap>>,
    pub before_create: Option<HookFn<RowMap>>,
    pub before_update: Option<HookFn<RowMap>>,
    pub before_upsert: Option<HookFn<RowMap>>,
    /// Receives the raw delete criteria (JSON) and may replace it.
    pub before_delete: Option<HookFn<Value>>,
    pub before_find: Option<HookFn<FilterExpr>>,

    pub after_save: Option<HookFn<RowMap>>,
    pub after_create: Option<HookFn<RowMap>>,
    pub after_update: Option<HookFn<RowMap>>,
    pub after_upsert: Option<HookFn<RowMap>>,
    pub after_find: Option<HookFn<Vec<RowMap>>>,
    /// Receives the adapter remove summary; its return value is ignored by the caller.
    pub after_delete: Option<HookFn<WriteSummary>>,
    pub after_bulk: Option<BulkHookFn>,
}

fn wrap<T, F, Fut>(f: F) -> HookFn<T>
where
    T: Send + 'static,
    F: Fn(T, Context) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<T>> + Send + 'static,
{
    Arc::new(move |t, c| Box::pin(f(t, c)))
}

macro_rules! setter {
    ($name:ident, $t:ty) => {
        #[must_use]
        pub fn $name<F, Fut>(mut self, f: F) -> Self
        where
            F: Fn($t, Context) -> Fut + Send + Sync + 'static,
            Fut: Future<Output = Result<$t>> + Send + 'static,
        {
            self.$name = Some(wrap(f));
            self
        }
    };
}

impl ModelHooks {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    setter!(before_save, RowMap);
    setter!(before_create, RowMap);
    setter!(before_update, RowMap);
    setter!(before_upsert, RowMap);
    setter!(before_delete, Value);
    setter!(before_find, FilterExpr);
    setter!(after_save, RowMap);
    setter!(after_create, RowMap);
    setter!(after_update, RowMap);
    setter!(after_upsert, RowMap);
    setter!(after_find, Vec<RowMap>);
    setter!(after_delete, WriteSummary);

    #[must_use]
    pub fn after_bulk<F, Fut>(mut self, f: F) -> Self
    where
        F: Fn(BulkOp, WriteSummary, Context) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<()>> + Send + 'static,
    {
        self.after_bulk = Some(Arc::new(move |op, s, c| Box::pin(f(op, s, c))));
        self
    }
}

/// Run an optional hook.
pub(crate) async fn run<T>(hook: Option<&HookFn<T>>, value: T, ctx: &Context) -> Result<T> {
    match hook {
        Some(h) => h(value, ctx.clone()).await,
        None => Ok(value),
    }
}
