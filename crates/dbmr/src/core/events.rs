//! Change-event emission (§25). `dbmr-kafka` implements [`EventSink`] for its producer;
//! any other sink (NATS, a channel, a test recorder) can be plugged into a model.

use async_trait::async_trait;
use serde_json::Value;

/// Receives one event per successful write: `operation` is `insert`, `update`, `upsert`
/// or `delete`; `data` is the plaintext record (or rows / delete criteria).
///
/// Emission is fire-and-forget: the model spawns `emit` on the Tokio runtime and never
/// waits for or fails on it. Inside a transaction events are buffered and emitted only
/// after the outermost commit (D11).
#[async_trait]
pub trait EventSink: Send + Sync + 'static {
    async fn emit(&self, table: &str, operation: &str, data: Value);
}
