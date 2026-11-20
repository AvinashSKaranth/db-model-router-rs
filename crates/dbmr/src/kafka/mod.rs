//! `dbmr-kafka`: Optional Kafka event producer with failure swallowing and transaction buffering.

use chrono::Utc;
use rskafka::client::partition::{Compression, UnknownTopicHandling};
use rskafka::client::ClientBuilder;
use rskafka::record::Record;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};

pub const KAFKA_BATCH_SIZE: usize = 500;

/// Configuration for Kafka connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KafkaConfig {
    pub brokers: Vec<String>,
    pub client_id: String,
    pub topic_prefix: String,
}

impl KafkaConfig {
    #[must_use]
    pub fn from_env_or_options(options: Option<KafkaConfig>) -> Option<Self> {
        let (brokers, client_id, topic_prefix) = if let Some(opts) = options {
            (opts.brokers, opts.client_id, opts.topic_prefix)
        } else {
            let broker_var = std::env::var("KAFKA_BROKER").ok()?;
            let brokers: Vec<String> = broker_var
                .split(',')
                .map(|b| b.trim().to_string())
                .filter(|b| !b.is_empty())
                .collect();
            if brokers.is_empty() {
                return None;
            }
            let client_id =
                std::env::var("KAFKA_CLIENT_ID").unwrap_or_else(|_| "db-model-router".to_string());
            let topic_prefix =
                std::env::var("KAFKA_TOPIC_PREFIX").unwrap_or_else(|_| "dbmr".to_string());
            (brokers, client_id, topic_prefix)
        };

        if brokers.is_empty() {
            None
        } else {
            Some(Self {
                brokers,
                client_id,
                topic_prefix,
            })
        }
    }
}

/// JSON event payload per §25 normative contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KafkaEvent {
    pub table_name: String,
    pub operation_type: String,
    pub data: Value,
    pub timestamp: String,
}

struct ProducerState {
    config: KafkaConfig,
    client: rskafka::client::Client,
}

/// Thread-safe Kafka producer that swallows errors and provides transaction buffering.
#[derive(Clone, Default)]
pub struct KafkaProducer {
    state: Arc<RwLock<Option<ProducerState>>>,
}

impl KafkaProducer {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(RwLock::new(None)),
        }
    }

    /// Initializes producer. Returns true if successfully connected.
    pub async fn init(&self, options: Option<KafkaConfig>) -> bool {
        let Some(config) = KafkaConfig::from_env_or_options(options) else {
            *self.state.write().await = None;
            return false;
        };

        let build_res = tokio::time::timeout(
            std::time::Duration::from_secs(3),
            ClientBuilder::new(config.brokers.clone())
                .client_id(config.client_id.as_str())
                .build(),
        )
        .await;

        match build_res {
            Ok(Ok(client)) => {
                *self.state.write().await = Some(ProducerState { config, client });
                true
            }
            Ok(Err(e)) => {
                tracing::error!("[db-model-router] Kafka initialization failed: {}", e);
                *self.state.write().await = None;
                false
            }
            Err(_) => {
                tracing::error!("[db-model-router] Kafka initialization timed out");
                *self.state.write().await = None;
                false
            }
        }
    }

    /// Returns true if producer is connected and ready.
    pub async fn status(&self) -> bool {
        self.state.read().await.is_some()
    }

    /// Gracefully disconnects producer.
    pub async fn disconnect(&self) {
        *self.state.write().await = None;
    }

    /// Produces events for the operation. All errors are swallowed and logged per §25.
    pub async fn produce(&self, table: &str, operation: &str, data: &Value) {
        let state_guard = self.state.read().await;
        let Some(state) = state_guard.as_ref() else {
            return;
        };

        let topic = format!("{}.{table}", state.config.topic_prefix);
        let timestamp = Utc::now().to_rfc3339();

        let entries: Vec<Value> = match data {
            Value::Array(arr) => arr.clone(),
            other => vec![other.clone()],
        };

        let pc_res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            state
                .client
                .partition_client(topic.clone(), 0, UnknownTopicHandling::Retry),
        )
        .await;

        let partition_client = match pc_res {
            Ok(Ok(pc)) => pc,
            Ok(Err(e)) => {
                tracing::error!("[db-model-router] Kafka produce failed for {topic}: {e}");
                return;
            }
            Err(_) => {
                tracing::error!("[db-model-router] Kafka partition_client timed out for {topic}");
                return;
            }
        };

        for chunk in entries.chunks(KAFKA_BATCH_SIZE) {
            let records: Vec<Record> = chunk
                .iter()
                .map(|item| {
                    let event = KafkaEvent {
                        table_name: table.to_string(),
                        operation_type: operation.to_string(),
                        data: item.clone(),
                        timestamp: timestamp.clone(),
                    };
                    let json_bytes = serde_json::to_vec(&event).unwrap_or_default();
                    Record {
                        key: Some(table.as_bytes().to_vec()),
                        value: Some(json_bytes),
                        timestamp: Utc::now(),
                        headers: BTreeMap::new(),
                    }
                })
                .collect();

            let produce_res = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                partition_client.produce(records, Compression::NoCompression),
            )
            .await;

            match produce_res {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    tracing::error!("[db-model-router] Kafka produce failed for {topic}: {e}");
                }
                Err(_) => {
                    tracing::error!("[db-model-router] Kafka produce timed out for {topic}");
                }
            }
        }
    }
}

/// Buffer for Kafka events during an active transaction (§25/§26).
#[derive(Debug, Default, Clone)]
pub struct KafkaBuffer {
    events: Arc<Mutex<Vec<(String, String, Value)>>>,
}

impl KafkaBuffer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Buffers an event during a transaction.
    pub async fn push(&self, table: &str, operation: &str, data: Value) {
        let mut guard = self.events.lock().await;
        guard.push((table.to_string(), operation.to_string(), data));
    }

    /// Discards all buffered events on transaction rollback.
    pub async fn clear(&self) {
        let mut guard = self.events.lock().await;
        guard.clear();
    }

    /// Flushes all buffered events through the producer upon top-level commit.
    pub async fn flush(&self, producer: &KafkaProducer) -> usize {
        let mut guard = self.events.lock().await;
        let count = guard.len();
        for (table, operation, data) in guard.drain(..) {
            producer.produce(&table, &operation, &data).await;
        }
        count
    }
}

/// Plug into a model with `Model::builder(..).events(Arc::new(producer))`; inside a
/// transaction events are sent after commit.
#[async_trait::async_trait]
impl crate::core::EventSink for KafkaProducer {
    async fn emit(&self, table: &str, operation: &str, data: Value) {
        self.produce(table, operation, &data).await;
    }
}
