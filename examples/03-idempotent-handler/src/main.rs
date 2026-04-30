//! Example: subscribe with idempotent handler — processes each message exactly once.
//!
//! Run with:
//!   docker compose up -d nats   # or: docker run -p 4222:4222 nats:latest -js
//!   cargo run -p example-03-idempotent-handler -- nats://localhost:4222

use async_trait::async_trait;
use bus_nats::{
    subscriber::SubscribeOptions, NatsClient, NatsKvIdempotencyStore, StreamConfig,
};
use event_bus::{prelude::*, EventBusBuilder};
use serde::{Deserialize, Serialize};
use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "events.payments.processed")]
struct PaymentProcessed {
    id:         MessageId,
    payment_id: String,
    amount:     i64,
}

struct PaymentHandler {
    count: Arc<AtomicU32>,
}

#[async_trait]
impl EventHandler<PaymentProcessed> for PaymentHandler {
    async fn handle(
        &self,
        ctx: HandlerCtx,
        evt: PaymentProcessed,
    ) -> Result<(), HandlerError> {
        self.count.fetch_add(1, Ordering::SeqCst);
        println!(
            "[delivery #{}] payment_id={} amount={} msg_id={}",
            ctx.delivered, evt.payment_id, evt.amount, ctx.msg_id
        );
        Ok(())
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    let url = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "nats://localhost:4222".into());
    let stream_cfg = StreamConfig {
        num_replicas: 1,
        ..Default::default()
    };
    let client = NatsClient::connect(&url, &stream_cfg).await?;
    let store =
        NatsKvIdempotencyStore::new(client.jetstream().clone(), Duration::from_secs(3600)).await?;

    let count = Arc::new(AtomicU32::new(0));
    let bus = EventBusBuilder::new()
        .url(&url)
        .stream_config(stream_cfg)
        .idempotency(store)
        .build()
        .await?;

    let _handle = bus
        .subscribe(
            SubscribeOptions {
                durable: "payment-handler".into(),
                filter: "events.payments.>".into(),
                concurrency: 2,
                ..Default::default()
            },
            PaymentHandler {
                count: count.clone(),
            },
        )
        .await?;

    // Publish same event twice — handler must run exactly once
    let evt = PaymentProcessed {
        id:         MessageId::new(),
        payment_id: "pay-001".into(),
        amount:     9999,
    };
    bus.publish(&evt).await?;
    bus.publish(&evt).await?; // duplicate — deduped

    tokio::time::sleep(Duration::from_secs(1)).await;
    println!("Handler invocations: {}", count.load(Ordering::SeqCst));
    assert_eq!(count.load(Ordering::SeqCst), 1);

    bus.shutdown().await?;
    Ok(())
}
