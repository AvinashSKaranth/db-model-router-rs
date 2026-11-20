//! Transactions with savepoint nesting and buffered change events (§26, D11).
//!
//! ```no_run
//! # async fn demo(db: std::sync::Arc<dyn dbmr::DatabaseAdapter>, users: dbmr::Model) -> dbmr::Result<()> {
//! use dbmr::{Context, Transaction, core::types::row};
//! let tx = Transaction::begin(db.as_ref()).await?;
//! let users_tx = users.with_tx(&tx);
//! users_tx.insert(row([("name", "Ann".into())]), &Context::new()).await?;
//! let inner = tx.savepoint().await?;      // SAVEPOINT sp_1
//! inner.rollback().await?;                // ROLLBACK TO sp_1, drops its buffered events
//! tx.commit().await?;                     // COMMIT, then emits buffered events
//! # Ok(()) }
//! ```

use crate::core::adapter::{DatabaseAdapter, TransactionAdapter};
use crate::core::error::Result;
use crate::core::events::EventSink;
use serde_json::Value;
use std::sync::{Arc, Mutex};

pub(crate) struct PendingEvent {
    pub sink: Arc<dyn EventSink>,
    pub table: String,
    pub operation: &'static str,
    pub data: Value,
}

pub(crate) struct TxShared {
    adapter: Arc<dyn TransactionAdapter>,
    events: Mutex<Vec<PendingEvent>>,
}

impl TxShared {
    pub(crate) fn push(&self, e: PendingEvent) {
        if let Ok(mut g) = self.events.lock() {
            g.push(e);
        }
    }
}

/// Handle to an open transaction (or a savepoint inside one). Dropping it without
/// `commit` rolls back once every model bound via `with_tx` is dropped too.
pub struct Transaction {
    shared: Arc<TxShared>,
    depth: usize,
    mark: usize,
}

impl Transaction {
    /// Begin a top-level transaction.
    ///
    /// # Errors
    /// `"Active database adapter does not support transactions."` for adapters other than
    /// SQLite, PostgreSQL/CockroachDB, MySQL/MariaDB, MSSQL and Oracle, or a database error.
    pub async fn begin(db: &dyn DatabaseAdapter) -> Result<Self> {
        let adapter = db.begin_tx().await?;
        Ok(Self {
            shared: Arc::new(TxShared {
                adapter,
                events: Mutex::new(Vec::new()),
            }),
            depth: 0,
            mark: 0,
        })
    }

    /// Adapter whose operations run inside this transaction.
    #[must_use]
    pub fn adapter(&self) -> Arc<dyn DatabaseAdapter> {
        self.shared.adapter.clone().as_adapter()
    }

    pub(crate) fn shared(&self) -> Arc<TxShared> {
        self.shared.clone()
    }

    fn buffered(&self) -> usize {
        self.shared.events.lock().map_or(0, |g| g.len())
    }

    /// Open a nested savepoint (`SAVEPOINT sp_<n>`); it shares the event buffer.
    ///
    /// # Errors
    /// Database error.
    pub async fn savepoint(&self) -> Result<Transaction> {
        let depth = self.depth + 1;
        self.shared
            .adapter
            .query(&format!("SAVEPOINT sp_{depth}"), &[])
            .await?;
        Ok(Self {
            shared: self.shared.clone(),
            depth,
            mark: self.buffered(),
        })
    }

    /// Commit. Top level: `COMMIT`, then emit every buffered event. Savepoint: `RELEASE`.
    ///
    /// # Errors
    /// Database error (buffered events are then discarded).
    pub async fn commit(self) -> Result<()> {
        if self.depth > 0 {
            self.shared
                .adapter
                .query(&format!("RELEASE SAVEPOINT sp_{}", self.depth), &[])
                .await?;
            return Ok(());
        }
        let res = self.shared.adapter.commit().await;
        let events: Vec<PendingEvent> = self
            .shared
            .events
            .lock()
            .map(|mut g| g.drain(..).collect())
            .unwrap_or_default();
        res?;
        for e in events {
            spawn_emit(e);
        }
        Ok(())
    }

    /// Roll back. Top level: `ROLLBACK`; savepoint: `ROLLBACK TO`. Events buffered since
    /// this level began are discarded.
    ///
    /// # Errors
    /// Database error.
    pub async fn rollback(self) -> Result<()> {
        if let Ok(mut g) = self.shared.events.lock() {
            g.truncate(self.mark);
        }
        if self.depth > 0 {
            self.shared
                .adapter
                .query(&format!("ROLLBACK TO SAVEPOINT sp_{}", self.depth), &[])
                .await?;
            return Ok(());
        }
        self.shared.adapter.rollback().await
    }
}

pub(crate) fn spawn_emit(e: PendingEvent) {
    if let Ok(h) = tokio::runtime::Handle::try_current() {
        h.spawn(async move { e.sink.emit(&e.table, e.operation, e.data).await });
    } else {
        tracing::warn!(
            "[db-model-router] no Tokio runtime; event for {} dropped",
            e.table
        );
    }
}
