//! Example: publish an event with deduplication via Nats-Msg-Id.
//!
//! Run with:
//!   docker compose up -d nats   # or: docker run -p 4222:4222 nats:latest -js
//!   cargo run -p example-01-basic-publish -- nats://localhost:4222

use bus_nats::{NatsClient, NatsKvIdempotencyStore, StreamConfig};
use event_bus::{prelude::*, EventBusBuilder};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "events.orders.created", aggregate = "order")]
struct OrderCreated {
    id:       MessageId,
    order_id: String,
    total:    i64,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "nats://localhost:4222".into());

    // Build idempotency store
    let stream_cfg = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };
    let client = NatsClient::connect(&url, &stream_cfg).await?;
    let store =
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(3600)).await?;

    // Build bus
    let bus = EventBusBuilder::new()
        .url(&url)
        .stream_config(stream_cfg)
        .idempotency(store)
        .build()
        .await?;

    let evt = OrderCreated {
        id:       MessageId::new(),
        order_id: "ord-001".into(),
        total:    4999,
    };

    // First publish
    let r1 = bus.publish(&evt).await?;
    println!("First:  seq={} duplicate={}", r1.sequence, r1.duplicate);

    // Second publish with same message_id — will be deduped by JetStream
    let r2 = bus.publish(&evt).await?;
    println!("Second: seq={} duplicate={}", r2.sequence, r2.duplicate);

    assert!(!r1.duplicate);
    assert!(r2.duplicate, "second publish must be deduplicated");

    bus.shutdown().await?;
    println!("Done.");
    Ok(())
}
