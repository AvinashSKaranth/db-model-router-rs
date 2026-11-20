#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args
)]

use dbmr::kafka::{KafkaBuffer, KafkaConfig, KafkaProducer};
use serde_json::json;

#[tokio::test]
async fn test_disabled_producer_zero_overhead() {
    let producer = KafkaProducer::new();
    assert!(!producer.status().await);

    // Call produce when disabled - must return immediately without panic
    let payload = json!({ "id": 1, "name": "Alice" });
    producer.produce("users", "insert", &payload).await;
    // Same through the EventSink trait a Model calls.
    dbmr::core::EventSink::emit(&producer, "users", "insert", payload.clone()).await;
    assert!(!producer.status().await);

    // Disconnect when disabled is a no-op
    producer.disconnect().await;
    assert!(!producer.status().await);
}

#[tokio::test]
async fn test_transaction_buffer_mechanics() {
    let buffer = KafkaBuffer::new();
    let producer = KafkaProducer::new();

    // 1. Buffer two events
    buffer
        .push("users", "insert", json!({ "id": 1, "name": "Alice" }))
        .await;
    buffer
        .push("users", "update", json!({ "id": 1, "name": "Alice B" }))
        .await;

    // 2. Discard on rollback
    buffer.clear().await;
    let flushed = buffer.flush(&producer).await;
    assert_eq!(flushed, 0);

    // 3. Buffer again and flush on commit
    buffer
        .push("orders", "insert", json!({ "id": 100, "total": 42.5 }))
        .await;
    let flushed = buffer.flush(&producer).await;
    assert_eq!(flushed, 1);

    // Buffer is now empty
    let flushed_again = buffer.flush(&producer).await;
    assert_eq!(flushed_again, 0);
}

#[tokio::test]
#[ignore = "requires live Kafka on 127.0.0.1:9092: docker compose up, then -- --ignored"]
async fn test_live_kafka_producer_integration() {
    let producer = KafkaProducer::new();
    let config = KafkaConfig {
        brokers: vec!["127.0.0.1:9092".to_string()],
        client_id: "dbmr-test-client".to_string(),
        topic_prefix: "dbmr_test".to_string(),
    };

    let ok = producer.init(Some(config)).await;
    assert!(
        ok,
        "Live Kafka container on 127.0.0.1:9092 not ready or unreachable"
    );

    assert!(producer.status().await);

    // Single produce
    producer
        .produce("users", "insert", &json!({ "id": 1, "name": "Bob" }))
        .await;

    // Bulk produce (multi-row batch)
    let bulk_data = json!([
        { "id": 2, "name": "Charlie" },
        { "id": 3, "name": "David" }
    ]);
    producer.produce("users", "insert", &bulk_data).await;

    producer.disconnect().await;
    assert!(!producer.status().await);
}

// ---- coverage: env config, init failures, delivered events, batching, broker errors ----

use dbmr::kafka::{KafkaEvent, KAFKA_BATCH_SIZE};
use rskafka::client::partition::{OffsetAt, UnknownTopicHandling};
use rskafka::client::ClientBuilder;

const BROKER: &str = "127.0.0.1:9092";

/// The only test that touches `KAFKA_*` env vars, so parallel tests cannot race on them.
#[tokio::test]
async fn test_config_from_env_and_options() {
    for k in ["KAFKA_BROKER", "KAFKA_CLIENT_ID", "KAFKA_TOPIC_PREFIX"] {
        std::env::remove_var(k);
    }
    assert_eq!(KafkaConfig::from_env_or_options(None), None);
    // Unconfigured producer stays disabled.
    let p = KafkaProducer::new();
    assert!(!p.init(None).await);
    assert!(!p.status().await);

    std::env::set_var("KAFKA_BROKER", " , ,");
    assert_eq!(KafkaConfig::from_env_or_options(None), None);

    std::env::set_var("KAFKA_BROKER", "h1:9092, h2:9093 ,");
    assert_eq!(
        KafkaConfig::from_env_or_options(None),
        Some(KafkaConfig {
            brokers: vec!["h1:9092".into(), "h2:9093".into()],
            client_id: "db-model-router".into(),
            topic_prefix: "dbmr".into(),
        })
    );
    std::env::set_var("KAFKA_CLIENT_ID", "cid");
    std::env::set_var("KAFKA_TOPIC_PREFIX", "pfx");
    let c = KafkaConfig::from_env_or_options(None).unwrap();
    assert_eq!(
        (c.client_id.as_str(), c.topic_prefix.as_str()),
        ("cid", "pfx")
    );
    for k in ["KAFKA_BROKER", "KAFKA_CLIENT_ID", "KAFKA_TOPIC_PREFIX"] {
        std::env::remove_var(k);
    }

    // Explicit options win; empty broker lists disable the producer.
    let opts = KafkaConfig {
        brokers: vec![],
        client_id: "c".into(),
        topic_prefix: "p".into(),
    };
    assert_eq!(KafkaConfig::from_env_or_options(Some(opts)), None);
}

#[tokio::test]
async fn test_init_unreachable_broker_times_out() {
    let p = KafkaProducer::new();
    let started = std::time::Instant::now();
    let ok = p
        .init(Some(KafkaConfig {
            brokers: vec!["127.0.0.1:1".into()],
            client_id: "cov".into(),
            topic_prefix: "cov".into(),
        }))
        .await;
    assert!(!ok);
    assert!(!p.status().await);
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

fn unique_prefix(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("cov_{tag}_{nanos}")
}

async fn live_producer(prefix: &str) -> KafkaProducer {
    let p = KafkaProducer::new();
    let ok = p
        .init(Some(KafkaConfig {
            brokers: vec![BROKER.into()],
            client_id: "dbmr-cov".into(),
            topic_prefix: prefix.into(),
        }))
        .await;
    assert!(ok, "Kafka on {BROKER} not reachable");
    p
}

/// Delivery needs the broker's *advertised* listener (`localhost:9092` in docker-compose) to
/// connect within the producer's 5 s timeout. On Windows `localhost` resolves to `::1` first
/// and Docker's IPv6 port mapping does not answer, so the connect takes ~21 s and the producer
/// drops every event. Fail loudly instead of passing without delivering anything.
async fn require_reachable_leader() {
    let client = ClientBuilder::new(vec![BROKER.into()])
        .build()
        .await
        .unwrap();
    let pc = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        client.partition_client(
            "dbmr_test.users".to_string(),
            0,
            UnknownTopicHandling::Retry,
        ),
    )
    .await;
    assert!(
        matches!(pc, Ok(Ok(_))),
        "Kafka's advertised listener is not reachable within 5 s from this host;          advertise 127.0.0.1:9092 (KAFKA_ADVERTISED_LISTENERS) so events can be delivered"
    );
}

/// Reads every event on `topic`, partition 0.
async fn consume(topic: &str) -> Vec<(Option<Vec<u8>>, KafkaEvent)> {
    let client = ClientBuilder::new(vec![BROKER.into()])
        .build()
        .await
        .unwrap();
    let pc = client
        .partition_client(topic.to_string(), 0, UnknownTopicHandling::Retry)
        .await
        .unwrap();
    let end = pc.get_offset(OffsetAt::Latest).await.unwrap();
    let mut out = Vec::new();
    let mut offset = pc.get_offset(OffsetAt::Earliest).await.unwrap();
    while offset < end {
        let (records, _) = pc.fetch_records(offset, 1..5_000_000, 1_000).await.unwrap();
        for r in records {
            offset = r.offset + 1;
            let ev = serde_json::from_slice(r.record.value.as_deref().unwrap()).unwrap();
            out.push((r.record.key, ev));
        }
    }
    out
}

#[tokio::test]
#[ignore = "requires live Kafka on 127.0.0.1:9092: docker compose up, then -- --ignored"]
async fn test_live_kafka_events_are_delivered() {
    require_reachable_leader().await;
    let prefix = unique_prefix("events");
    let p = live_producer(&prefix).await;

    p.produce("users", "insert", &json!({ "id": 1, "name": "Bob" }))
        .await;
    p.produce("users", "delete", &json!([{ "id": 2 }, { "id": 3 }]))
        .await;
    // Through the EventSink trait, as a Model would call it.
    dbmr::core::EventSink::emit(&p, "users", "update", json!({ "id": 1 })).await;
    // Buffered events are sent on flush.
    let buf = KafkaBuffer::new();
    buf.push("users", "upsert", json!({ "id": 4 })).await;
    assert_eq!(buf.flush(&p).await, 1);

    let events = consume(&format!("{prefix}.users")).await;
    let summary: Vec<(String, serde_json::Value)> = events
        .iter()
        .map(|(_, e)| (e.operation_type.clone(), e.data.clone()))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("insert".into(), json!({ "id": 1, "name": "Bob" })),
            ("delete".into(), json!({ "id": 2 })),
            ("delete".into(), json!({ "id": 3 })),
            ("update".into(), json!({ "id": 1 })),
            ("upsert".into(), json!({ "id": 4 })),
        ]
    );
    for (key, e) in &events {
        assert_eq!(key.as_deref(), Some(&b"users"[..]));
        assert_eq!(e.table_name, "users");
        assert!(chrono::DateTime::parse_from_rfc3339(&e.timestamp).is_ok());
    }
    p.disconnect().await;
    assert!(!p.status().await);
}

#[tokio::test]
#[ignore = "requires live Kafka on 127.0.0.1:9092: docker compose up, then -- --ignored"]
async fn test_live_kafka_large_bulk_is_chunked() {
    require_reachable_leader().await;
    let prefix = unique_prefix("bulk");
    let p = live_producer(&prefix).await;
    let n = KAFKA_BATCH_SIZE + 7;
    let rows: Vec<serde_json::Value> = (0..n).map(|i| json!({ "id": i })).collect();
    p.produce("items", "insert", &serde_json::Value::Array(rows))
        .await;
    let events = consume(&format!("{prefix}.items")).await;
    assert_eq!(events.len(), n);
    assert_eq!(events[n - 1].1.data, json!({ "id": n - 1 }));
}

#[tokio::test]
#[ignore = "requires live Kafka on 127.0.0.1:9092: docker compose up, then -- --ignored"]
async fn test_live_kafka_broker_errors_are_swallowed() {
    require_reachable_leader().await;
    let prefix = unique_prefix("errors");
    let p = live_producer(&prefix).await;

    // Batch larger than the broker's message.max.bytes (1 MB): rejected without panicking,
    // and later events still go through.
    let huge = "x".repeat(2 * 1024 * 1024);
    p.produce("big", "insert", &json!({ "blob": huge })).await;
    p.produce("big", "insert", &json!({ "id": 1 })).await;
    let events = consume(&format!("{prefix}.big")).await;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].1.data, json!({ "id": 1 }));
}

#[tokio::test]
#[ignore = "requires live Kafka on 127.0.0.1:9092: docker compose up, then -- --ignored"]
async fn test_live_kafka_invalid_topic_is_swallowed() {
    let p = live_producer(&unique_prefix("invalid")).await;
    // The broker rejects the topic name at metadata time; produce returns at once.
    let started = std::time::Instant::now();
    p.produce("bad topic/name", "insert", &json!({ "id": 1 }))
        .await;
    assert!(started.elapsed() < std::time::Duration::from_secs(4));
    assert!(p.status().await, "errors never tear down the producer");
}
