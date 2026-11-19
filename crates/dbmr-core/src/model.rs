//! The model: validated, hook-aware, optionally encrypted CRUD over one table (§21).
//!
//! ```no_run
//! # async fn demo(db: std::sync::Arc<dyn dbmr_core::DatabaseAdapter>) -> dbmr_core::Result<()> {
//! use dbmr_core::{Context, Model, Query, types::row};
//!
//! let users = Model::builder(db, "users")
//!     .column("id", "integer|auto_increment")
//!     .column("name", "required|string|maxLength:100")
//!     .column("email", "required|email")
//!     .unique(["email"])
//!     .safe_delete("is_deleted")
//!     .search_columns(["name", "email"])
//!     .build()?;
//!
//! let ctx = Context::new();
//! let ann = users
//!     .insert(row([("name", "Ann".into()), ("email", "ann@example.com".into())]), &ctx)
//!     .await?
//!     .expect("re-fetch found the new row");
//! let page = users.list(Query::new().search("ann").size(20), &ctx).await?;
//! assert_eq!(page.data[0]["id"], ann["id"]);
//! # Ok(()) }
//! ```

use crate::adapter::DatabaseAdapter;
use crate::encryption::{EncryptionConfig, EncryptionMeta};
use crate::error::{AppError, Result};
use crate::events::EventSink;
use crate::filter::{data_to_filter, FilterExpr, Operator};
use crate::hooks::{self, BulkOp, Context, ModelHooks};
use crate::json::json_safe_parse;
use crate::query::{apply_select, Query};
use crate::rules::{self, ColumnRules, Rule};
use crate::transaction::{spawn_emit, PendingEvent, Transaction, TxShared};
use crate::types::{row_to_json, DbValue, FindResult, GetOptions, RowMap, WriteSummary};
use compact_str::CompactString;
use indexmap::IndexMap;
use serde_json::Value;
use std::sync::Arc;

pub static DEFAULT_CREATED_AT_VARIANTS: &[&str] = &[
    "created_at",
    "createdAt",
    "created",
    "create_date",
    "createDate",
    "creation_date",
    "creationDate",
];

pub static DEFAULT_MODIFIED_AT_VARIANTS: &[&str] = &[
    "modified_at",
    "modifiedAt",
    "modified",
    "updated_at",
    "updatedAt",
    "updated",
    "update_date",
    "updateDate",
    "modification_date",
    "modificationDate",
];

/// 409 body when an optimistic-lock version does not match.
pub const CONFLICT_MESSAGE: &str = "Record has been modified by another request.";
/// 428 body when a write on a versioned model does not carry the version.
pub const VERSION_REQUIRED_MESSAGE: &str = "The version field is required for this update.";

/// Message of the mass-delete guard (D15).
pub const MASS_DELETE_MESSAGE: &str = "Request body must contain filter criteria";

#[derive(Debug, Clone, Default)]
pub struct ModelOptions {
    /// Soft-delete flag column: reads filter `col = 0/false`, deletes set it to 1/true.
    pub safe_delete: Option<String>,
    /// Replaces the default created-at variant list (stripped from client writes).
    pub created_at: Option<Vec<String>>,
    /// Replaces the default modified-at variant list.
    pub modified_at: Option<Vec<String>>,
    pub search_columns: Vec<String>,
    /// Permit `remove` with an empty filter (deletes every row). Default `false` (D15).
    pub allow_destructive: bool,
    /// Columns never returned by reads, single writes or events (e.g. password hashes).
    /// They are still written. [`Model::with_hidden`] returns a copy that shows them.
    pub hidden: Vec<String>,
    /// Optimistic-lock column (integer). Inserts set it to 1; updates must send the
    /// current value and get `409` when it changed, `428` when it is missing.
    pub version_column: Option<String>,
    /// Column set to the current UTC time (RFC 3339) on insert.
    pub auto_created_at: Option<String>,
    /// Column set to the current UTC time (RFC 3339) on every write.
    pub auto_modified_at: Option<String>,
    /// Scope columns every call must carry in [`Context::scope`] (else `401 "Missing
    /// scope: <col>"`), unless the context is [`Context::system`].
    pub require_scope: Vec<String>,
}

/// How a related model is attached by `Query::include`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    /// `child.<foreign_key> = this.<pk>`; attached as an array.
    HasMany,
    /// `this.<foreign_key> = parent.<pk>`; attached as an object (or `null`).
    BelongsTo,
}

/// A named relation used by `Query::include` / `?include=`.
#[derive(Clone)]
pub struct Relation {
    pub name: String,
    pub kind: RelationKind,
    pub model: Model,
    pub foreign_key: String,
}

/// Model handle; cheap to clone (all state behind `Arc`).
#[derive(Clone)]
pub struct Model {
    db: Arc<dyn DatabaseAdapter>,
    inner: Arc<ModelDef>,
    tx: Option<Arc<TxShared>>,
    reveal_hidden: bool,
}

/// Previous name, kept for source compatibility.
pub type ModelInstance = Model;

#[derive(Clone)]
struct ModelDef {
    table: CompactString,
    primary_key: CompactString,
    structure: IndexMap<String, String>,
    rules: Vec<ColumnRules>,
    unique: Vec<Vec<String>>,
    options: ModelOptions,
    hooks: ModelHooks,
    encryption: Option<Arc<EncryptionConfig>>,
    enc_meta: EncryptionMeta,
    events: Option<Arc<dyn EventSink>>,
    strip_keys: Vec<String>,
    relations: Vec<Relation>,
}

/// Builder returned by [`Model::builder`].
pub struct ModelBuilder {
    db: Arc<dyn DatabaseAdapter>,
    table: String,
    primary_key: String,
    structure: IndexMap<String, String>,
    unique: Vec<Vec<String>>,
    options: ModelOptions,
    hooks: ModelHooks,
    encryption: Option<Arc<EncryptionConfig>>,
    events: Option<Arc<dyn EventSink>>,
}

impl ModelBuilder {
    /// Add a column with its rule string, e.g. `"required|string|maxLength:100"`.
    /// Use a dotted name (`"profile.dob"`) to encrypt a key inside a JSON column.
    #[must_use]
    pub fn column(mut self, name: &str, rule: &str) -> Self {
        self.structure.insert(name.to_string(), rule.to_string());
        self
    }

    #[must_use]
    pub fn columns<'a>(mut self, cols: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        for (n, r) in cols {
            self.structure.insert(n.to_string(), r.to_string());
        }
        self
    }

    /// Replace the whole structure (e.g. from schema metadata).
    #[must_use]
    pub fn structure(mut self, structure: IndexMap<String, String>) -> Self {
        self.structure = structure;
        self
    }

    /// Primary key column. Default `"id"`.
    #[must_use]
    pub fn primary_key(mut self, pk: &str) -> Self {
        self.primary_key = pk.to_string();
        self
    }

    /// One unique key (single or composite). Call repeatedly for several keys; the first
    /// is the upsert conflict target.
    #[must_use]
    pub fn unique<I: IntoIterator<Item = S>, S: Into<String>>(mut self, cols: I) -> Self {
        self.unique.push(cols.into_iter().map(Into::into).collect());
        self
    }

    #[must_use]
    pub fn unique_keys(mut self, keys: Vec<Vec<String>>) -> Self {
        self.unique = keys;
        self
    }

    #[must_use]
    pub fn safe_delete(mut self, col: &str) -> Self {
        self.options.safe_delete = Some(col.to_string());
        self
    }

    #[must_use]
    pub fn search_columns<I: IntoIterator<Item = S>, S: Into<String>>(mut self, cols: I) -> Self {
        self.options.search_columns = cols.into_iter().map(Into::into).collect();
        self
    }

    #[must_use]
    pub fn options(mut self, options: ModelOptions) -> Self {
        self.options = options;
        self
    }

    #[must_use]
    pub fn allow_destructive(mut self, allow: bool) -> Self {
        self.options.allow_destructive = allow;
        self
    }

    #[must_use]
    pub fn hooks(mut self, hooks: ModelHooks) -> Self {
        self.hooks = hooks;
        self
    }

    #[must_use]
    pub fn encryption(mut self, cfg: EncryptionConfig) -> Self {
        self.encryption = Some(Arc::new(cfg));
        self
    }

    #[must_use]
    pub fn encryption_shared(mut self, cfg: Option<Arc<EncryptionConfig>>) -> Self {
        self.encryption = cfg;
        self
    }

    /// Never return these columns (reads, single-write results, events). They can still be
    /// written, e.g. by a `before_save` hook that hashes a password.
    #[must_use]
    pub fn hidden<I: IntoIterator<Item = S>, S: Into<String>>(mut self, cols: I) -> Self {
        self.options.hidden = cols.into_iter().map(Into::into).collect();
        self
    }

    /// Optimistic locking on an integer column (DDL: `version INTEGER NOT NULL DEFAULT 1`).
    /// Declared as `integer` automatically when not declared.
    #[must_use]
    pub fn version_column(mut self, col: &str) -> Self {
        self.options.version_column = Some(col.to_string());
        self
    }

    /// Set `col` to the current UTC time on insert (clients cannot set it).
    #[must_use]
    pub fn created_at(mut self, col: &str) -> Self {
        self.options.auto_created_at = Some(col.to_string());
        self
    }

    /// Set `col` to the current UTC time on every write (clients cannot set it).
    #[must_use]
    pub fn modified_at(mut self, col: &str) -> Self {
        self.options.auto_modified_at = Some(col.to_string());
        self
    }

    /// Fail closed: every call must carry these columns in [`Context::scope`]
    /// (`401 "Missing scope: <col>"` when absent or null). Cross-scope jobs use
    /// [`Context::system`].
    #[must_use]
    pub fn require_scope<I: IntoIterator<Item = S>, S: Into<String>>(mut self, cols: I) -> Self {
        self.options.require_scope = cols.into_iter().map(Into::into).collect();
        self
    }

    /// Emit change events (e.g. `dbmr_kafka::KafkaProducer`).
    #[must_use]
    pub fn events(mut self, sink: Arc<dyn EventSink>) -> Self {
        self.events = Some(sink);
        self
    }

    /// Compile rules and build the model.
    ///
    /// # Errors
    /// [`AppError::Validation`] for an unknown or malformed rule, or when a column is
    /// marked `encrypted` but no [`EncryptionConfig`] was supplied.
    pub fn build(mut self) -> Result<Model> {
        // Columns the model manages are declared, so reads normalize and return them on
        // every adapter (an undeclared soft-delete flag reads 0/1 on some databases).
        let managed = [
            (self.options.version_column.clone(), "integer"),
            (self.options.safe_delete.clone(), "boolean"),
            (self.options.auto_created_at.clone(), "string|nullable"),
            (self.options.auto_modified_at.clone(), "string|nullable"),
        ];
        for (col, rule) in managed {
            if let Some(col) = col {
                self.structure
                    .entry(col)
                    .or_insert_with(|| rule.to_string());
            }
        }
        let rules = self
            .structure
            .iter()
            .map(|(c, r)| rules::compile(c, r))
            .collect::<Result<Vec<_>>>()?;
        let enc_meta = EncryptionMeta::from_rules(&rules);
        if !enc_meta.is_empty() && self.encryption.is_none() {
            return Err(AppError::Validation(format!(
                "Model \"{}\" declares encrypted fields but no encryption config was provided. \
                 Pass .encryption(EncryptionConfig::new(key, version, None)?) to the builder.",
                self.table
            )));
        }
        let defaults = |v: &[&str]| v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        let strip_keys = self
            .options
            .created_at
            .clone()
            .unwrap_or_else(|| defaults(DEFAULT_CREATED_AT_VARIANTS))
            .into_iter()
            .chain(
                self.options
                    .modified_at
                    .clone()
                    .unwrap_or_else(|| defaults(DEFAULT_MODIFIED_AT_VARIANTS)),
            )
            // D19: the soft-delete flag is server-managed; clients cannot set or clear it.
            .chain(self.options.safe_delete.clone())
            .chain(self.options.auto_created_at.clone())
            .chain(self.options.auto_modified_at.clone())
            .collect();
        Ok(Model {
            db: self.db,
            tx: None,
            reveal_hidden: false,
            inner: Arc::new(ModelDef {
                table: CompactString::new(&self.table),
                primary_key: CompactString::new(&self.primary_key),
                structure: self.structure,
                rules,
                unique: self.unique,
                options: self.options,
                hooks: self.hooks,
                encryption: self.encryption,
                enc_meta,
                events: self.events,
                strip_keys,
                relations: Vec::new(),
            }),
        })
    }
}

impl Model {
    /// Start building a model for `table` (primary key defaults to `"id"`).
    #[must_use]
    pub fn builder(db: Arc<dyn DatabaseAdapter>, table: &str) -> ModelBuilder {
        ModelBuilder {
            db,
            table: table.to_string(),
            primary_key: "id".to_string(),
            structure: IndexMap::new(),
            unique: Vec::new(),
            options: ModelOptions::default(),
            hooks: ModelHooks::default(),
            encryption: None,
            events: None,
        }
    }

    /// Shorthand: `Model::define(db, "users", &[("id","integer"),("name","required|string")], "id", &["name"])`.
    ///
    /// # Errors
    /// See [`ModelBuilder::build`].
    pub fn define(
        db: Arc<dyn DatabaseAdapter>,
        table: &str,
        fields: &[(&str, &str)],
        primary_key: &str,
        unique: &[&str],
    ) -> Result<Self> {
        let mut b = Self::builder(db, table)
            .columns(fields.iter().copied())
            .primary_key(primary_key);
        for u in unique {
            b = b.unique([*u]);
        }
        b.build()
    }

    /// Same model bound to a transaction: writes run inside it and events are buffered
    /// until the outermost commit.
    #[must_use]
    pub fn with_tx(&self, tx: &Transaction) -> Self {
        Self {
            db: tx.adapter(),
            inner: self.inner.clone(),
            tx: Some(tx.shared()),
            reveal_hidden: self.reveal_hidden,
        }
    }

    /// Same model, but hidden columns are returned (server-side checks such as a
    /// password comparison). Never hand this one to `create_model_router`.
    #[must_use]
    pub fn with_hidden(&self) -> Self {
        Self {
            reveal_hidden: true,
            ..self.clone()
        }
    }

    /// Add a one-to-many relation: `?include=<name>` attaches `child` rows whose
    /// `foreign_key` equals this row's primary key.
    #[must_use]
    pub fn has_many(&self, name: &str, child: &Model, foreign_key: &str) -> Self {
        self.with_relation(name, RelationKind::HasMany, child, foreign_key)
    }

    /// Add a many-to-one relation: `?include=<name>` attaches the `parent` row whose
    /// primary key equals this row's `foreign_key`.
    #[must_use]
    pub fn belongs_to(&self, name: &str, parent: &Model, foreign_key: &str) -> Self {
        self.with_relation(name, RelationKind::BelongsTo, parent, foreign_key)
    }

    fn with_relation(&self, name: &str, kind: RelationKind, other: &Model, fk: &str) -> Self {
        let mut def = (*self.inner).clone();
        def.relations.retain(|r| r.name != name);
        def.relations.push(Relation {
            name: name.to_string(),
            kind,
            model: other.clone(),
            foreign_key: fk.to_string(),
        });
        Self {
            inner: Arc::new(def),
            ..self.clone()
        }
    }

    #[must_use]
    pub fn relations(&self) -> &[Relation] {
        &self.inner.relations
    }

    #[must_use]
    pub fn hidden_columns(&self) -> &[String] {
        &self.inner.options.hidden
    }

    #[must_use]
    pub fn version_column(&self) -> Option<&str> {
        self.inner.options.version_column.as_deref()
    }

    /// Typed view: the same operations, taking and returning `T` (serde) instead of rows.
    #[must_use]
    pub fn typed<T>(&self) -> crate::typed::TypedModel<T> {
        crate::typed::TypedModel::new(self.clone())
    }

    #[must_use]
    pub fn pk(&self) -> &str {
        self.inner.primary_key.as_str()
    }

    #[must_use]
    pub fn table_name(&self) -> &str {
        self.inner.table.as_str()
    }

    #[must_use]
    pub fn structure(&self) -> &IndexMap<String, String> {
        &self.inner.structure
    }

    #[must_use]
    pub fn options(&self) -> &ModelOptions {
        &self.inner.options
    }

    #[must_use]
    pub fn hooks(&self) -> &ModelHooks {
        &self.inner.hooks
    }

    #[must_use]
    pub fn adapter(&self) -> &Arc<dyn DatabaseAdapter> {
        &self.db
    }

    // ---------------------------------------------------------------- helpers

    fn safe_delete(&self) -> Option<&str> {
        self.inner.options.safe_delete.as_deref()
    }

    fn strip_timestamps(&self, row: &mut RowMap) {
        for k in &self.inner.strip_keys {
            row.shift_remove(k.as_str());
        }
    }

    fn strip_unknown(&self, row: &mut RowMap) {
        row.retain(|k, _| self.inner.structure.contains_key(k.as_str()));
    }

    /// Drop hidden columns from an outgoing record.
    fn conceal(&self, row: &mut RowMap) {
        if !self.reveal_hidden {
            for c in &self.inner.options.hidden {
                row.shift_remove(c.as_str());
            }
        }
    }

    /// App-side timestamps: `created` also stamps the created-at column.
    fn stamp(&self, row: &mut RowMap, created: bool) {
        let opts = &self.inner.options;
        if opts.auto_created_at.is_none() && opts.auto_modified_at.is_none() {
            return;
        }
        let now = DbValue::Text(
            chrono::Utc::now()
                .format("%Y-%m-%dT%H:%M:%S%.3fZ")
                .to_string(),
        );
        if created {
            if let Some(c) = &opts.auto_created_at {
                row.insert(CompactString::new(c), now.clone());
            }
        }
        if let Some(c) = &opts.auto_modified_at {
            row.insert(CompactString::new(c), now);
        }
    }

    fn version_col(&self) -> Option<&str> {
        self.inner.options.version_column.as_deref()
    }

    fn encrypt(&self, row: &mut RowMap) -> Result<()> {
        match &self.inner.encryption {
            Some(cfg) if !self.inner.enc_meta.is_empty() => {
                self.inner.enc_meta.encrypt_row(cfg, row)
            }
            _ => Ok(()),
        }
    }

    /// Stored row -> application form: decrypt, then parse `object` / `array` columns that
    /// the database returned as text. Writes always store JSON as text, and only some
    /// databases hand it back typed (MySQL `JSON`, Postgres `JSONB`); MariaDB (`JSON` is
    /// `LONGTEXT`), SQLite, MSSQL, Oracle and the key-value stores return the string.
    fn decode(&self, row: &mut RowMap) -> Result<()> {
        if let Some(cfg) = &self.inner.encryption {
            if !self.inner.enc_meta.is_empty() {
                self.inner.enc_meta.decrypt_row(cfg, row)?;
            }
        }
        for col in &self.inner.rules {
            if !col
                .rules
                .iter()
                .any(|r| matches!(r, Rule::Object | Rule::Array))
            {
                continue;
            }
            let Some(v) = row.get_mut(col.column.as_str()) else {
                continue;
            };
            let text = match v {
                DbValue::Text(t) => t.as_str(),
                DbValue::Bytes(b) => std::str::from_utf8(b).unwrap_or_default(),
                _ => continue,
            };
            if let Ok(j @ (Value::Object(_) | Value::Array(_))) = serde_json::from_str(text) {
                *v = DbValue::Json(j);
            }
        }
        rules::normalize_row(&self.inner.rules, row);
        Ok(())
    }

    /// Outgoing record shape, the same on every adapter: primary key, then the declared
    /// columns in declaration order (NULL when the store omitted them; a missing
    /// soft-delete flag is `false`), then any undeclared columns.
    fn shape(&self, row: &mut RowMap) {
        if self.inner.rules.is_empty() {
            return;
        }
        let mut out = RowMap::with_capacity(row.len() + self.inner.rules.len());
        let cols =
            std::iter::once(self.pk()).chain(self.inner.rules.iter().map(|r| r.column.as_str()));
        for c in cols {
            if !out.contains_key(c) {
                out.insert(
                    CompactString::new(c),
                    row.shift_remove(c).unwrap_or(DbValue::Null),
                );
            }
        }
        if let Some(sd) = self.safe_delete() {
            if let Some(v @ DbValue::Null) = out.get_mut(sd) {
                *v = DbValue::Bool(false);
            }
        }
        out.extend(row.drain(..));
        *row = out;
    }

    /// Object/array values become JSON strings before reaching the adapter (Node
    /// `jsonStringify`).
    fn stringify(row: &mut RowMap) {
        for v in row.values_mut() {
            if let DbValue::Json(j @ (Value::Object(_) | Value::Array(_))) = v {
                *v = DbValue::Text(j.to_string());
            }
        }
    }

    fn emit(&self, operation: &'static str, data: Value) {
        let Some(sink) = &self.inner.events else {
            return;
        };
        let e = PendingEvent {
            sink: sink.clone(),
            table: self.inner.table.to_string(),
            operation,
            data,
        };
        match &self.tx {
            Some(tx) => tx.push(e),
            None => spawn_emit(e),
        }
    }

    async fn get_one(
        &self,
        filter: &FilterExpr,
        safe_delete: Option<&str>,
    ) -> Result<Option<RowMap>> {
        let res = self
            .db
            .get(
                self.table_name(),
                filter,
                &[],
                safe_delete,
                GetOptions {
                    count: false,
                    limit: Some(1),
                    offset: None,
                },
            )
            .await?;
        Ok(res.data.into_iter().next())
    }

    fn pk_filter(&self, id: DbValue) -> FilterExpr {
        FilterExpr::eq(self.pk(), id)
    }

    /// `before_save` + `before_<op>` hooks for one row.
    async fn before(&self, mut row: RowMap, op: BulkOp, ctx: &Context) -> Result<RowMap> {
        let h = &self.inner.hooks;
        row = hooks::run(h.before_save.as_ref(), row, ctx).await?;
        match op {
            BulkOp::Insert => hooks::run(h.before_create.as_ref(), row, ctx).await,
            BulkOp::Update => hooks::run(h.before_update.as_ref(), row, ctx).await,
            BulkOp::Upsert => {
                row = hooks::run(h.before_update.as_ref(), row, ctx).await?;
                hooks::run(h.before_upsert.as_ref(), row, ctx).await
            }
        }
    }

    /// `after_save` + `after_<op>` hooks for one record.
    async fn after(&self, mut rec: RowMap, op: BulkOp, ctx: &Context) -> Result<RowMap> {
        let h = &self.inner.hooks;
        rec = hooks::run(h.after_save.as_ref(), rec, ctx).await?;
        match op {
            BulkOp::Insert => hooks::run(h.after_create.as_ref(), rec, ctx).await,
            BulkOp::Update => hooks::run(h.after_update.as_ref(), rec, ctx).await,
            BulkOp::Upsert => {
                rec = hooks::run(h.after_update.as_ref(), rec, ctx).await?;
                hooks::run(h.after_upsert.as_ref(), rec, ctx).await
            }
        }
    }

    /// Hooks → (PK strip on insert) → timestamps → validate → unknown → plaintext copy →
    /// encrypt → stringify. Returns (rows for the adapter, plaintext rows for events).
    async fn prepare(
        &self,
        rows: Vec<RowMap>,
        op: BulkOp,
        bulk: bool,
        ctx: &Context,
    ) -> Result<(Vec<RowMap>, Vec<RowMap>)> {
        let scope = self.scope(ctx)?;
        let mut prepared = Vec::with_capacity(rows.len());
        for (i, row) in rows.into_iter().enumerate() {
            let mut row = self.before(row, op, ctx).await?;
            self.apply_scope(&mut row, &row_prefix(bulk, i), false, ctx)?;
            prepared.push(row);
        }
        // CREATE validator (PK excluded) for insert/upsert, UPDATE (PK included) for update.
        let pk = self.pk();
        let rules: Vec<&ColumnRules> = self
            .inner
            .rules
            .iter()
            .filter(|r| op == BulkOp::Update || r.column != pk)
            .collect();
        let mut errors = Vec::new();
        for (i, row) in prepared.iter_mut().enumerate() {
            if op == BulkOp::Insert {
                row.shift_remove(pk);
            }
            self.strip_timestamps(row);
            let prefix = if bulk {
                format!("data.{i}.")
            } else {
                String::new()
            };
            rules::collect_errors(rules.iter().copied(), row, &prefix, &mut errors);
        }
        rules::finish(&errors)?;
        for row in &mut prepared {
            rules::normalize_row(&self.inner.rules, row);
        }
        self.check_refs(&prepared, bulk, ctx).await?;
        if op == BulkOp::Insert {
            self.check_unique(&prepared, bulk).await?;
        } else {
            self.check_targets(&prepared, op, bulk, &scope).await?;
        }
        let mut plain = Vec::with_capacity(prepared.len());
        for row in &mut prepared {
            self.strip_unknown(row);
            if op == BulkOp::Insert {
                if let Some(vc) = self.version_col() {
                    row.insert(CompactString::new(vc), DbValue::Int(1));
                }
            }
            self.stamp(row, op == BulkOp::Insert);
            plain.push(row.clone());
            self.encrypt(row)?;
            Self::stringify(row);
        }
        Ok((prepared, plain))
    }

    /// Resolve the written record: adapter `record`, else re-fetch by returned id, else by
    /// the payload PK. `Ok(None)` = written but not found again (D4).
    async fn resolve_record(
        &self,
        summary: &WriteSummary,
        payload_pk: Option<DbValue>,
        safe_delete: Option<&str>,
    ) -> Result<Option<RowMap>> {
        if let Some(rec) = &summary.record {
            return Ok(Some(rec.clone()));
        }
        let id = summary
            .id
            .clone()
            .filter(|v| !v.is_null() && *v != DbValue::Int(0))
            .or(payload_pk.filter(|v| !v.is_null()));
        match id {
            Some(id) => self.get_one(&self.pk_filter(id), safe_delete).await,
            None => Ok(None),
        }
    }

    /// Upsert conflict target: the first unique key whose columns every row supplies,
    /// else the PK when every row supplies it, else the declared keys (adapter falls back
    /// to a plain insert when there are none).
    fn conflict_target(&self, rows: &[RowMap]) -> Vec<Vec<String>> {
        let has = |cols: &[String]| {
            rows.iter()
                .all(|r| cols.iter().all(|c| r.contains_key(c.as_str())))
        };
        if let Some(u) = self.inner.unique.iter().find(|u| has(u)) {
            return vec![u.clone()];
        }
        let pk = vec![self.pk().to_string()];
        if has(&pk) {
            return vec![pk];
        }
        self.inner.unique.clone()
    }

    async fn write_one(&self, row: RowMap, op: BulkOp, ctx: &Context) -> Result<Option<RowMap>> {
        let (rows, _) = self.prepare(vec![row], op, false, ctx).await?;
        if self.version_col().is_some() && op != BulkOp::Insert {
            let row = rows.into_iter().next().unwrap_or_default();
            let Some(key) = self.locked_write(row, op).await? else {
                return Ok(None);
            };
            let Some(mut rec) = self.get_one(&key, None).await? else {
                return Ok(None);
            };
            self.decode(&mut rec)?;
            return self.finish_write(rec, op, op_name(op), ctx).await.map(Some);
        }
        let payload_pk = if op == BulkOp::Insert {
            None
        } else {
            rows[0].get(self.pk()).cloned()
        };
        let summary = match op {
            BulkOp::Insert => {
                self.db
                    .insert_rows(self.table_name(), &rows, &self.inner.unique)
                    .await?
            }
            _ => {
                self.db
                    .upsert_rows(self.table_name(), &rows, &self.conflict_target(&rows))
                    .await?
            }
        };
        // Node: the insert re-fetch ignores safeDelete; update/upsert honour it.
        let sd = if op == BulkOp::Insert {
            None
        } else {
            self.safe_delete()
        };
        let Some(mut rec) = self.resolve_record(&summary, payload_pk, sd).await? else {
            tracing::warn!(
                "[db-model-router] {} on {} succeeded but the record could not be re-fetched",
                op_name(op),
                self.table_name()
            );
            return Ok(None);
        };
        self.decode(&mut rec)?;
        self.finish_write(rec, op, op_name(op), ctx).await.map(Some)
    }

    /// After hooks, then hide columns, then emit.
    async fn finish_write(
        &self,
        rec: RowMap,
        op: BulkOp,
        event: &'static str,
        ctx: &Context,
    ) -> Result<RowMap> {
        let mut rec = self.after(rec, op, ctx).await?;
        self.shape(&mut rec);
        self.conceal(&mut rec);
        self.emit(event, row_to_json(&rec));
        Ok(rec)
    }

    /// Parse a client-supplied version (`3`, `"3"`, `3.0`).
    fn version_value(v: &DbValue) -> Result<i128> {
        let n = match v {
            DbValue::Int(i) => Some(*i),
            DbValue::Text(t) => t.trim().trim_matches('"').parse().ok(),
            DbValue::Float(f) if f.fract() == 0.0 => Some(*f as i128),
            DbValue::Decimal(d) => d.trunc().to_string().parse().ok(),
            _ => None,
        };
        n.ok_or_else(|| AppError::UnprocessableEntity("The version must be an integer.".into()))
    }

    /// Optimistic-lock write for update/upsert of one prepared row: `UPDATE ... SET
    /// version = v + 1 WHERE key AND version = v`. Returns the filter that finds the row
    /// again, or `None` when an update targets no row. `409` when the row exists with a
    /// different version, `428` when the version is missing for an existing row.
    async fn locked_write(&self, mut row: RowMap, op: BulkOp) -> Result<Option<FilterExpr>> {
        let vc = self.version_col().unwrap_or_default().to_string();
        let version = row.shift_remove(vc.as_str()).filter(|v| !v.is_null());
        let key_cols: Vec<String> = if op == BulkOp::Update {
            vec![self.pk().to_string()]
        } else {
            self.conflict_target(std::slice::from_ref(&row))
                .into_iter()
                .next()
                .unwrap_or_default()
        };
        let key = key_cols.iter().try_fold(FilterExpr::default(), |f, c| {
            row.get(c.as_str())
                .filter(|v| !v.is_null())
                .map(|v| f.and(c, Operator::Eq, v.clone()))
        });
        let key = match key {
            Some(k) if !key_cols.is_empty() => k,
            _ if op == BulkOp::Update => {
                return Err(AppError::UnprocessableEntity(
                    "Primary key is required for update".into(),
                ))
            }
            _ => return self.locked_insert(row, None).await.map(Some),
        };
        let sd = self.safe_delete();
        let Some(v) = version else {
            if op == BulkOp::Update || self.get_one(&key, sd).await?.is_some() {
                return Err(AppError::PreconditionRequired(
                    VERSION_REQUIRED_MESSAGE.into(),
                ));
            }
            return self.locked_insert(row, Some(key)).await.map(Some);
        };
        let v = Self::version_value(&v)?;
        let mut set = row.clone();
        for c in &key_cols {
            set.shift_remove(c.as_str());
        }
        set.insert(CompactString::new(&vc), DbValue::Int(v + 1));
        let guarded = key.clone().and(&vc, Operator::Eq, DbValue::Int(v));
        if self
            .db
            .update_where(self.table_name(), &set, &guarded, sd)
            .await?
            > 0
        {
            return Ok(Some(key));
        }
        match self.get_one(&key, sd).await? {
            Some(_) => Err(AppError::Conflict(CONFLICT_MESSAGE.into())),
            None if op == BulkOp::Upsert => self.locked_insert(row, Some(key)).await.map(Some),
            None => Ok(None),
        }
    }

    /// Insert path of a versioned upsert: version 1 and the created-at stamp.
    async fn locked_insert(&self, mut row: RowMap, key: Option<FilterExpr>) -> Result<FilterExpr> {
        if let Some(vc) = self.version_col() {
            row.insert(CompactString::new(vc), DbValue::Int(1));
        }
        self.stamp(&mut row, true);
        let summary = self
            .db
            .insert_rows(self.table_name(), &[row], &self.inner.unique)
            .await?;
        match summary.id.filter(|v| !v.is_null() && *v != DbValue::Int(0)) {
            Some(id) => Ok(self.pk_filter(id)),
            None => key.ok_or_else(|| {
                AppError::Internal("Inserted row has no id to re-read it by".into())
            }),
        }
    }

    async fn write_many(
        &self,
        rows: Vec<RowMap>,
        op: BulkOp,
        ctx: &Context,
    ) -> Result<WriteSummary> {
        let (rows, mut plain) = self.prepare(rows, op, true, ctx).await?;
        for r in &mut plain {
            self.conceal(r);
        }
        if self.version_col().is_some() && op != BulkOp::Insert {
            // ponytail: one statement per row; wrap in a Transaction for all-or-nothing.
            let mut n = 0u64;
            for (i, row) in rows.into_iter().enumerate() {
                match self.locked_write(row, op).await {
                    Ok(Some(_)) => n += 1,
                    Ok(None) => {}
                    Err(e) => return Err(prefix_error(&e, &format!("data.{i}: "))),
                }
            }
            let summary = WriteSummary {
                rows: n,
                message: format!("{n} {} saved", self.table_name()),
                id: None,
                record: None,
            };
            if let Some(h) = &self.inner.hooks.after_bulk {
                h(op, summary.clone(), ctx.clone()).await?;
            }
            self.emit(
                op_name(op),
                Value::Array(plain.iter().map(row_to_json).collect()),
            );
            return Ok(summary);
        }
        let summary = match op {
            BulkOp::Insert => {
                self.db
                    .insert_rows(self.table_name(), &rows, &self.inner.unique)
                    .await?
            }
            _ => {
                self.db
                    .upsert_rows(self.table_name(), &rows, &self.conflict_target(&rows))
                    .await?
            }
        };
        if let Some(h) = &self.inner.hooks.after_bulk {
            h(op, summary.clone(), ctx.clone()).await?;
        }
        self.emit(
            op_name(op),
            Value::Array(plain.iter().map(row_to_json).collect()),
        );
        Ok(summary)
    }

    // ---------------------------------------------------------------- writes

    /// Insert one row. The PK and timestamp columns are stripped, the row is validated
    /// (`422` with node-input-validator messages), unknown columns dropped, encrypted
    /// columns encrypted, then the row is written and re-read.
    ///
    /// Returns the stored record (decrypted), or `None` if it could not be re-fetched.
    ///
    /// # Errors
    /// `422` validation, hook errors, database errors.
    #[tracing::instrument(level = "debug", skip_all, fields(table = %self.inner.table))]
    pub async fn insert(&self, row: RowMap, ctx: &Context) -> Result<Option<RowMap>> {
        self.write_one(row, BulkOp::Insert, ctx).await
    }

    /// Bulk insert; validation messages are prefixed `data.<i>.`. Returns the adapter summary.
    ///
    /// # Errors
    /// See [`Model::insert`].
    #[tracing::instrument(level = "debug", skip_all, fields(table = %self.inner.table))]
    pub async fn insert_many(&self, rows: Vec<RowMap>, ctx: &Context) -> Result<WriteSummary> {
        self.write_many(rows, BulkOp::Insert, ctx).await
    }

    /// Update one row (written via the adapter's upsert). Validates with the PK rule included.
    ///
    /// # Errors
    /// See [`Model::insert`].
    #[tracing::instrument(level = "debug", skip_all, fields(table = %self.inner.table))]
    pub async fn update(&self, row: RowMap, ctx: &Context) -> Result<Option<RowMap>> {
        self.write_one(row, BulkOp::Update, ctx).await
    }

    /// # Errors
    /// See [`Model::insert`].
    #[tracing::instrument(level = "debug", skip_all, fields(table = %self.inner.table))]
    pub async fn update_many(&self, rows: Vec<RowMap>, ctx: &Context) -> Result<WriteSummary> {
        self.write_many(rows, BulkOp::Update, ctx).await
    }

    /// Insert-or-update on the first unique key (or the PK). The PK is optional.
    ///
    /// # Errors
    /// See [`Model::insert`].
    #[tracing::instrument(level = "debug", skip_all, fields(table = %self.inner.table))]
    pub async fn upsert(&self, row: RowMap, ctx: &Context) -> Result<Option<RowMap>> {
        self.write_one(row, BulkOp::Upsert, ctx).await
    }

    /// # Errors
    /// See [`Model::insert`].
    #[tracing::instrument(level = "debug", skip_all, fields(table = %self.inner.table))]
    pub async fn upsert_many(&self, rows: Vec<RowMap>, ctx: &Context) -> Result<WriteSummary> {
        self.write_many(rows, BulkOp::Upsert, ctx).await
    }

    /// Partial update: only supplied known columns change. Requires the PK
    /// (`422 "Primary key is required for patch"`). Returns `None` if there is no such row.
    ///
    /// # Errors
    /// `422` validation (supplied keys only, `required` ignored), hook and database errors.
    #[tracing::instrument(level = "debug", skip_all, fields(table = %self.inner.table))]
    pub async fn patch(&self, row: RowMap, ctx: &Context) -> Result<Option<RowMap>> {
        let scope = self.scope(ctx)?;
        let h = &self.inner.hooks;
        let mut data = hooks::run(h.before_save.as_ref(), row, ctx).await?;
        data = hooks::run(h.before_update.as_ref(), data, ctx).await?;
        self.apply_scope(&mut data, "", true, ctx)?;
        self.strip_timestamps(&mut data);
        let Some(pk_val) = data.get(self.pk()).cloned() else {
            return Err(AppError::UnprocessableEntity(
                "Primary key is required for patch".to_string(),
            ));
        };
        let supplied: Vec<ColumnRules> = self
            .inner
            .rules
            .iter()
            .filter(|r| r.column != self.pk() && data.contains_key(r.column.as_str()))
            .map(ColumnRules::without_required)
            .collect();
        let mut errors = Vec::new();
        rules::collect_errors(&supplied, &data, "", &mut errors);
        rules::finish(&errors)?;
        rules::normalize_row(&self.inner.rules, &mut data);
        self.check_refs(std::slice::from_ref(&data), false, ctx)
            .await?;
        if let Some(vc) = self.version_col() {
            return self.patch_locked(data, pk_val, vc, &scope, ctx).await;
        }

        let sd = self.safe_delete();
        let key = self.pk_filter(pk_val.clone()).and_expr(&scope);
        let Some(mut existing) = self.get_one(&key, sd).await? else {
            return Ok(None);
        };
        self.decode(&mut existing)?;
        for (k, v) in data {
            if k.as_str() != self.pk() && self.inner.structure.contains_key(k.as_str()) {
                existing.insert(k, v);
            }
        }
        self.strip_unknown(&mut existing);
        self.stamp(&mut existing, false);
        self.encrypt(&mut existing)?;
        Self::stringify(&mut existing);
        existing.insert(CompactString::new(self.pk()), pk_val.clone());
        self.db
            .upsert_rows(
                self.table_name(),
                &[existing],
                &[vec![self.pk().to_string()]],
            )
            .await?;
        let Some(mut rec) = self.get_one(&self.pk_filter(pk_val), sd).await? else {
            return Ok(None);
        };
        self.decode(&mut rec)?;
        self.finish_write(rec, BulkOp::Update, "update", ctx)
            .await
            .map(Some)
    }

    /// Versioned patch: only the supplied columns change, guarded by the version.
    async fn patch_locked(
        &self,
        data: RowMap,
        pk_val: DbValue,
        vc: &str,
        scope: &FilterExpr,
        ctx: &Context,
    ) -> Result<Option<RowMap>> {
        let v = data
            .get(vc)
            .filter(|v| !v.is_null())
            .ok_or_else(|| AppError::PreconditionRequired(VERSION_REQUIRED_MESSAGE.into()))
            .and_then(Self::version_value)?;
        let mut set = data;
        set.shift_remove(self.pk());
        set.shift_remove(vc);
        self.strip_unknown(&mut set);
        self.stamp(&mut set, false);
        self.encrypt(&mut set)?;
        Self::stringify(&mut set);
        set.insert(CompactString::new(vc), DbValue::Int(v + 1));
        let key = self.pk_filter(pk_val).and_expr(scope);
        let sd = self.safe_delete();
        let guarded = key.clone().and(vc, Operator::Eq, DbValue::Int(v));
        if self
            .db
            .update_where(self.table_name(), &set, &guarded, sd)
            .await?
            == 0
        {
            return match self.get_one(&key, sd).await? {
                Some(_) => Err(AppError::Conflict(CONFLICT_MESSAGE.into())),
                None => Ok(None),
            };
        }
        let Some(mut rec) = self.get_one(&key, sd).await? else {
            return Ok(None);
        };
        self.decode(&mut rec)?;
        self.finish_write(rec, BulkOp::Update, "update", ctx)
            .await
            .map(Some)
    }

    /// Delete (or soft-delete) rows matching `criteria`, using the HTTP filter rules (§22):
    /// a scalar is a PK value, `[1,2]` is PK IN, `{"status":"old"}`, `{"filter":[...]}`.
    ///
    /// `before_delete` may replace the criteria. An empty filter is refused with
    /// `400 "Request body must contain filter criteria"` unless the model was built with
    /// `allow_destructive(true)` (D15).
    ///
    /// # Errors
    /// `422 "Invalid filter Inputs"`, `400` mass-delete guard, hook and database errors.
    #[tracing::instrument(level = "debug", skip_all, fields(table = %self.inner.table))]
    pub async fn remove(&self, criteria: Value, ctx: &Context) -> Result<WriteSummary> {
        let scope = self.scope(ctx)?;
        let criteria = hooks::run(self.inner.hooks.before_delete.as_ref(), criteria, ctx).await?;
        let parsed = json_safe_parse(&criteria);
        let filter = self.coerce_filter(data_to_filter(&parsed, self.pk())?);
        // The guard looks at the caller's criteria: a scope alone does not allow
        // deleting every row in it.
        if filter.is_empty() && !self.inner.options.allow_destructive {
            return Err(AppError::BadRequest(MASS_DELETE_MESSAGE.to_string()));
        }
        let filter = filter.and_expr(&scope);
        let summary = self
            .db
            .remove_rows(self.table_name(), &filter, self.safe_delete())
            .await?;
        if let Some(h) = &self.inner.hooks.after_delete {
            h(summary.clone(), ctx.clone()).await?;
        }
        self.emit("delete", parsed);
        Ok(summary)
    }

    /// Delete rows matching a prepared filter (same guard and hooks as [`Model::remove`]).
    ///
    /// # Errors
    /// See [`Model::remove`].
    #[tracing::instrument(level = "debug", skip_all, fields(table = %self.inner.table))]
    pub async fn remove_where(&self, filter: &FilterExpr, ctx: &Context) -> Result<WriteSummary> {
        self.remove(filter_to_json(filter), ctx).await
    }

    // ---------------------------------------------------------------- reads

    /// Fetch one row by primary key. `null`, `""` and non-scalar ids are
    /// `422 "Invalid id value"`.
    ///
    /// # Errors
    /// `422`, hook and database errors.
    #[tracing::instrument(level = "debug", skip_all, fields(table = %self.inner.table))]
    pub async fn by_id(&self, id: impl Into<DbValue>, ctx: &Context) -> Result<Option<RowMap>> {
        let id = id.into();
        let valid = match &id {
            DbValue::Text(s) => !s.is_empty(),
            DbValue::Int(_) | DbValue::Float(_) | DbValue::Decimal(_) => true,
            _ => false,
        };
        if !valid {
            return Err(AppError::UnprocessableEntity(
                "Invalid id value".to_string(),
            ));
        }
        let scope = self.scope(ctx)?;
        let filter = hooks::run(
            self.inner.hooks.before_find.as_ref(),
            self.pk_filter(id),
            ctx,
        )
        .await?
        .and_expr(&scope);
        let Some(mut rec) = self.get_one(&filter, self.safe_delete()).await? else {
            return Ok(None);
        };
        self.decode(&mut rec)?;
        self.after_find_one(rec, ctx).await
    }

    async fn after_find_one(&self, rec: RowMap, ctx: &Context) -> Result<Option<RowMap>> {
        self.after_find_one_with(rec, &[], ctx).await
    }

    async fn after_find_one_with(
        &self,
        rec: RowMap,
        include: &[String],
        ctx: &Context,
    ) -> Result<Option<RowMap>> {
        let rec = match &self.inner.hooks.after_find {
            None => rec,
            Some(h) => {
                let mut out = h(vec![rec.clone()], ctx.clone()).await?;
                // Node: take h[0] when non-empty, else keep the record.
                if out.is_empty() {
                    rec
                } else {
                    out.swap_remove(0)
                }
            }
        };
        let mut rows = vec![rec];
        self.attach(&mut rows, include, ctx).await?;
        let mut rec = rows.swap_remove(0);
        self.conceal(&mut rec);
        Ok(Some(rec))
    }

    /// Attach `include`d relations to `rows` (one query per relation).
    /// Declared unique keys behave the same on every adapter: an insert repeating the key
    /// of a stored row (soft-deleted ones included) or of an earlier row in the batch is
    /// 409 `Duplicate value for unique key (a, b).` Keys with a NULL part never conflict.
    // ponytail: check-then-insert is not atomic; SQL unique constraints still back it up
    // under concurrent inserts, MongoDB/Redis/DynamoDB need their own unique index for that.
    async fn check_unique(&self, rows: &[RowMap], bulk: bool) -> Result<()> {
        for key in &self.inner.unique {
            let dup = |i: usize| {
                let at = if bulk {
                    format!("data.{i}: ")
                } else {
                    String::new()
                };
                AppError::Conflict(format!(
                    "{at}Duplicate value for unique key ({}).",
                    key.join(", ")
                ))
            };
            let key_of = |r: &RowMap| -> Option<String> {
                key.iter()
                    .map(|c| {
                        r.get(c.as_str())
                            .filter(|v| !v.is_null())
                            .map(ToString::to_string)
                    })
                    .collect::<Option<Vec<_>>>()
                    .map(|v| v.join(&char::from(0x1f).to_string()))
            };
            let mut keys: Vec<(usize, String)> = Vec::new();
            let mut filter = FilterExpr::default();
            for (i, r) in rows.iter().enumerate() {
                let Some(k) = key_of(r) else { continue };
                if keys.iter().any(|(_, seen)| *seen == k) {
                    return Err(dup(i));
                }
                filter.groups.push(
                    key.iter()
                        .map(|c| crate::filter::condition(c, Operator::Eq, r[c.as_str()].clone()))
                        .collect(),
                );
                keys.push((i, k));
            }
            if keys.is_empty() {
                continue;
            }
            let found = self
                .db
                .list(self.table_name(), &filter, &[], None, 0, keys.len() as u64)
                .await?;
            if let Some(hit) = found.data.first() {
                let hit = key_of(hit);
                let i = keys
                    .iter()
                    .find(|(_, k)| Some(k) == hit.as_ref())
                    .map_or(0, |(i, _)| *i);
                return Err(dup(i));
            }
        }
        Ok(())
    }

    /// `ctx.scope` on the columns this model declares: `=` for a value, `IN` for a list.
    fn scope_filter(&self, ctx: &Context) -> FilterExpr {
        self.scoped(ctx)
            .fold(FilterExpr::default(), |f, (c, v)| match v {
                Value::Array(_) => f.and(c, Operator::In, DbValue::Json(v.clone())),
                _ => f.and(c, Operator::Eq, DbValue::from(v.clone())),
            })
    }

    fn scoped<'a>(&'a self, ctx: &'a Context) -> impl Iterator<Item = (&'a String, &'a Value)> {
        ctx.scope
            .iter()
            .filter(|(c, _)| self.inner.structure.contains_key(c.as_str()))
    }

    /// The rows this call may touch, coerced to the column types. Fails closed with
    /// `401 "Missing scope: <col>"` when a `require_scope` column is missing (unless
    /// `ctx.system`) or a scope value is null.
    fn scope(&self, ctx: &Context) -> Result<FilterExpr> {
        let missing = |c: &str| ctx.scope.get(c).is_none_or(Value::is_null);
        let required = self
            .inner
            .options
            .require_scope
            .iter()
            .filter(|_| !ctx.system)
            .map(String::as_str);
        let nulls = self
            .scoped(ctx)
            .filter(|(_, v)| v.is_null())
            .map(|(c, _)| c.as_str());
        if let Some(c) = required.chain(nulls).find(|c| missing(c)) {
            return Err(AppError::Unauthorized(format!("Missing scope: {c}")));
        }
        Ok(self.coerce_filter(self.scope_filter(ctx)))
    }

    /// Writes stay inside the scope: a scope value is written into the row; with a list,
    /// the row must name one of the values (`422 "The <col> is outside the scope."`).
    /// `partial` (patch): a list column the row does not supply is left alone.
    fn apply_scope(&self, row: &mut RowMap, at: &str, partial: bool, ctx: &Context) -> Result<()> {
        let text = |j: &Value| j.as_str().map_or_else(|| j.to_string(), str::to_string);
        for (c, v) in self.scoped(ctx) {
            let Value::Array(list) = v else {
                row.insert(CompactString::new(c), DbValue::from(v.clone()));
                continue;
            };
            let cur = row.get(c.as_str()).filter(|x| !x.is_null());
            if partial && cur.is_none() {
                continue;
            }
            if !cur.is_some_and(|x| list.iter().any(|j| text(j) == x.to_string())) {
                return Err(AppError::UnprocessableEntity(format!(
                    "{at}The {c} is outside the scope."
                )));
            }
        }
        Ok(())
    }

    /// Update/upsert targets must be inside the scope: a row whose key (the PK for
    /// update, the conflict key for upsert) names an existing row outside it is
    /// `404 "Not Found"`, so that row is neither changed nor revealed.
    // ponytail: check-then-write, not atomic; a row another scope creates in between with
    // the same key could still be overwritten. Push the scope into the adapter upsert if
    // that race matters.
    async fn check_targets(
        &self,
        rows: &[RowMap],
        op: BulkOp,
        bulk: bool,
        scope: &FilterExpr,
    ) -> Result<()> {
        if scope.is_empty() {
            return Ok(());
        }
        let cols = if op == BulkOp::Update {
            vec![self.pk().to_string()]
        } else {
            self.conflict_target(rows)
                .into_iter()
                .next()
                .unwrap_or_default()
        };
        if cols.is_empty() {
            return Ok(());
        }
        let key_of = |r: &RowMap| -> Option<String> {
            cols.iter()
                .map(|c| {
                    r.get(c.as_str())
                        .filter(|v| !v.is_null())
                        .map(ToString::to_string)
                })
                .collect::<Option<Vec<_>>>()
                .map(|p| p.join("\u{1f}"))
        };
        let all = rows
            .iter()
            .filter_map(|r| {
                cols.iter().try_fold(FilterExpr::default(), |f, c| {
                    r.get(c.as_str())
                        .filter(|v| !v.is_null())
                        .map(|v| f.and(c, Operator::Eq, v.clone()))
                })
            })
            .fold(FilterExpr::empty(), FilterExpr::or);
        if all.is_empty() {
            return Ok(());
        }
        let keys = |f: FilterExpr| async move {
            let opts = GetOptions {
                count: false,
                limit: None,
                offset: None,
            };
            let res = self.db.get(self.table_name(), &f, &[], None, opts).await?;
            Ok::<_, AppError>(res.data.iter().filter_map(key_of).collect::<Vec<_>>())
        };
        let existing = keys(all.clone()).await?;
        if existing.is_empty() {
            return Ok(());
        }
        let visible = keys(all.and_expr(scope)).await?;
        for (i, r) in rows.iter().enumerate() {
            if key_of(r).is_some_and(|k| existing.contains(&k) && !visible.contains(&k)) {
                return Err(AppError::NotFound(format!(
                    "{}Not Found",
                    row_prefix(bulk, i)
                )));
            }
        }
        Ok(())
    }

    /// Ids of `col` across `rows` as JSON (text when `model` stores `remote` as text),
    /// deduplicated, nulls skipped.
    fn ref_ids(rows: &[RowMap], col: &str, model: &Model, remote: &str) -> Vec<Value> {
        let text = model
            .structure()
            .get(remote)
            .is_some_and(|r| r.split('|').any(|p| p == "string" || p == "email"));
        let mut ids: Vec<Value> = Vec::new();
        for v in rows
            .iter()
            .filter_map(|r| r.get(col))
            .filter(|v| !v.is_null())
        {
            let j = if text {
                Value::String(v.to_string())
            } else {
                v.to_json()
            };
            if !ids.contains(&j) {
                ids.push(j);
            }
        }
        ids
    }

    /// Write guard for scoped calls: every `belongs_to` foreign key in `rows` must name a
    /// parent row visible inside `ctx.scope`, else 422 `Invalid reference: <fk>`. Skipped
    /// when the call has no scope or the parent declares none of the scope columns.
    async fn check_refs(&self, rows: &[RowMap], bulk: bool, ctx: &Context) -> Result<()> {
        if ctx.scope.is_empty() {
            return Ok(());
        }
        for rel in &self.inner.relations {
            if rel.kind != RelationKind::BelongsTo {
                continue;
            }
            let scope = rel.model.scope_filter(ctx);
            if scope.is_empty() {
                continue;
            }
            let pk = rel.model.pk();
            let ids = Self::ref_ids(rows, &rel.foreign_key, &rel.model, pk);
            if ids.is_empty() {
                continue;
            }
            // rel.model.find applies the scope.
            let q = Query::filter(FilterExpr::default().and(
                pk,
                Operator::In,
                DbValue::Json(Value::Array(ids)),
            ))
            .with_count(false);
            let found: Vec<String> = Box::pin(rel.model.find(q, ctx))
                .await?
                .data
                .iter()
                .filter_map(|r| r.get(pk).map(ToString::to_string))
                .collect();
            for (i, r) in rows.iter().enumerate() {
                let Some(v) = r.get(rel.foreign_key.as_str()).filter(|v| !v.is_null()) else {
                    continue;
                };
                if !found.contains(&v.to_string()) {
                    let at = if bulk {
                        format!("data.{i}: ")
                    } else {
                        String::new()
                    };
                    return Err(AppError::UnprocessableEntity(format!(
                        "{at}Invalid reference: {}",
                        rel.foreign_key
                    )));
                }
            }
        }
        Ok(())
    }

    async fn attach(&self, rows: &mut [RowMap], include: &[String], ctx: &Context) -> Result<()> {
        for name in include {
            let Some(rel) = self.inner.relations.iter().find(|r| &r.name == name) else {
                return Err(AppError::UnprocessableEntity(format!(
                    "Unknown include \"{name}\""
                )));
            };
            let (local, remote) = match rel.kind {
                RelationKind::HasMany => (self.pk(), rel.foreign_key.as_str()),
                RelationKind::BelongsTo => (rel.foreign_key.as_str(), rel.model.pk()),
            };
            // A text foreign key (e.g. a string column holding an integer id) gets text ids.
            let ids = Self::ref_ids(rows, local, &rel.model, remote);
            let found = if ids.is_empty() {
                Vec::new()
            } else {
                // rel.model.find applies the scope.
                let f = FilterExpr::default().and(
                    remote,
                    Operator::In,
                    DbValue::Json(Value::Array(ids)),
                );
                Box::pin(rel.model.find(Query::filter(f).with_count(false), ctx))
                    .await?
                    .data
            };
            for r in rows.iter_mut() {
                let key = r
                    .get(local)
                    .filter(|v| !v.is_null())
                    .map(ToString::to_string);
                let matches = found
                    .iter()
                    .filter(|c| key.is_some() && c.get(remote).map(ToString::to_string) == key);
                let value = match rel.kind {
                    RelationKind::HasMany => Value::Array(matches.map(row_to_json).collect()),
                    RelationKind::BelongsTo => {
                        matches.map(row_to_json).next().unwrap_or(Value::Null)
                    }
                };
                r.insert(CompactString::new(name), DbValue::Json(value));
            }
        }
        Ok(())
    }

    async fn finish_rows(
        &self,
        mut rows: Vec<RowMap>,
        q: &Query,
        ctx: &Context,
    ) -> Result<Vec<RowMap>> {
        for r in &mut rows {
            self.decode(r)?;
            self.shape(r);
        }
        if let Some(cols) = q.select_columns.as_deref().filter(|c| !c.is_empty()) {
            rows = rows.into_iter().map(|r| apply_select(r, cols)).collect();
        }
        let mut rows = hooks::run(self.inner.hooks.after_find.as_ref(), rows, ctx).await?;
        self.attach(&mut rows, &q.include, ctx).await?;
        for r in &mut rows {
            self.conceal(r);
        }
        Ok(rows)
    }

    /// Client filter, then `before_find`, then AND the scope (after the hook, so no
    /// filter shape or hook can drop it).
    async fn find_filter(&self, q: &Query, ctx: &Context) -> Result<FilterExpr> {
        let scope = self.scope(ctx)?;
        let f = self.coerce_filter(q.effective_filter(&self.inner.options.search_columns));
        Ok(hooks::run(self.inner.hooks.before_find.as_ref(), f, ctx)
            .await?
            .and_expr(&scope))
    }

    /// Query-string values are text. Convert them to the declared column type (the same
    /// [`rules::normalize`] as writes and reads), so every adapter compares like with
    /// like: `?is_deleted=true` is a boolean, `?qty=9` an integer. Values that do not
    /// convert are left as they are.
    fn coerce_filter(&self, mut f: FilterExpr) -> FilterExpr {
        for cond in f.groups.iter_mut().flatten() {
            if matches!(cond.operator, Operator::Like | Operator::NotLike) {
                continue;
            }
            let Some(col) = self
                .inner
                .rules
                .iter()
                .find(|r| r.column == cond.column.as_str())
            else {
                continue;
            };
            match &mut cond.value {
                DbValue::Json(Value::Array(items)) => {
                    for it in items.iter_mut() {
                        let mut v = DbValue::from(it.clone());
                        rules::normalize(col, &mut v);
                        *it = v.to_json();
                    }
                }
                v => rules::normalize(col, v),
            }
        }
        f
    }

    /// All matching rows (no pagination). `count` is the total match count unless
    /// `limit` is set or `with_count(false)` (then it is the returned row count).
    ///
    /// # Errors
    /// Hook and database errors.
    #[tracing::instrument(level = "debug", skip_all, fields(table = %self.inner.table))]
    pub async fn find(&self, q: Query, ctx: &Context) -> Result<FindResult> {
        let filter = self.find_filter(&q, ctx).await?;
        let need_count = q.count.unwrap_or(q.limit.is_none());
        let mut res = self
            .db
            .get(
                self.table_name(),
                &filter,
                &q.sort,
                self.safe_delete(),
                GetOptions {
                    count: need_count,
                    limit: q.limit,
                    offset: None,
                },
            )
            .await?;
        res.data = self
            .finish_rows(std::mem::take(&mut res.data), &q, ctx)
            .await?;
        Ok(res)
    }

    /// First matching row (honours `sort`), or `None` (D3).
    ///
    /// # Errors
    /// Hook and database errors.
    #[tracing::instrument(level = "debug", skip_all, fields(table = %self.inner.table))]
    pub async fn find_one(&self, q: Query, ctx: &Context) -> Result<Option<RowMap>> {
        let filter = self.find_filter(&q, ctx).await?;
        let res = self
            .db
            .get(
                self.table_name(),
                &filter,
                &q.sort,
                self.safe_delete(),
                GetOptions {
                    count: false,
                    limit: Some(1),
                    offset: None,
                },
            )
            .await?;
        let Some(mut rec) = res.data.into_iter().next() else {
            return Ok(None);
        };
        self.decode(&mut rec)?;
        if let Some(cols) = q.select_columns.as_deref().filter(|c| !c.is_empty()) {
            rec = apply_select(rec, cols);
        }
        self.after_find_one_with(rec, &q.include, ctx).await
    }

    /// One page: `page` (0-based, default 0) of `size` rows (default 30, 1..=200).
    /// `count` is the total number of matching rows.
    ///
    /// # Errors
    /// Hook and database errors.
    #[tracing::instrument(level = "debug", skip_all, fields(table = %self.inner.table))]
    pub async fn list(&self, q: Query, ctx: &Context) -> Result<FindResult> {
        let filter = self.find_filter(&q, ctx).await?;
        let (page, size) = q.pagination();
        let mut res = self
            .db
            .list(
                self.table_name(),
                &filter,
                &q.sort,
                self.safe_delete(),
                page,
                size,
            )
            .await?;
        res.data = self
            .finish_rows(std::mem::take(&mut res.data), &q, ctx)
            .await?;
        Ok(res)
    }

    /// Count matching rows (search and `before_find` applied).
    ///
    /// # Errors
    /// Hook and database errors.
    #[tracing::instrument(level = "debug", skip_all, fields(table = %self.inner.table))]
    pub async fn count(&self, q: &Query, ctx: &Context) -> Result<u64> {
        let filter = self.find_filter(q, ctx).await?;
        self.db
            .count(self.table_name(), &filter, self.safe_delete())
            .await
    }
}

fn op_name(op: BulkOp) -> &'static str {
    match op {
        BulkOp::Insert => "insert",
        BulkOp::Update => "update",
        BulkOp::Upsert => "upsert",
    }
}

/// Raw `{"filter": [[[col, op, value]]]}` form of a prepared filter.
fn filter_to_json(filter: &FilterExpr) -> Value {
    let groups: Vec<Value> = filter
        .groups
        .iter()
        .map(|g| {
            Value::Array(
                g.iter()
                    .map(|c| {
                        Value::Array(vec![
                            Value::String(c.column.to_string()),
                            Value::String(c.operator.to_string()),
                            c.value.to_json(),
                        ])
                    })
                    .collect(),
            )
        })
        .collect();
    serde_json::json!({ "filter": groups })
}

/// `data.<i>: ` for bulk rows, empty for single writes.
fn row_prefix(bulk: bool, i: usize) -> String {
    if bulk {
        format!("data.{i}: ")
    } else {
        String::new()
    }
}

/// Prefix the message of a bulk-row error (`data.3: ...`), keeping its status.
fn prefix_error(e: &AppError, prefix: &str) -> AppError {
    let m = format!("{prefix}{}", e.message());
    match e {
        AppError::BadRequest(_) => AppError::BadRequest(m),
        AppError::Unauthorized(_) => AppError::Unauthorized(m),
        AppError::NotFound(_) => AppError::NotFound(m),
        AppError::Conflict(_) => AppError::Conflict(m),
        AppError::PreconditionRequired(_) => AppError::PreconditionRequired(m),
        AppError::UnprocessableEntity(_) => AppError::UnprocessableEntity(m),
        AppError::Validation(_) => AppError::Validation(m),
        AppError::Internal(_) => AppError::Internal(m),
        AppError::Database(_) => AppError::Database(m),
        AppError::Encryption(_) => AppError::Encryption(m),
    }
}
