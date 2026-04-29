pub mod ack;
pub mod advisory;
pub mod circuit_breaker;
pub mod client;
pub mod consumer;
pub mod dlq;
pub mod inbox;
pub mod publisher;
pub mod stream;
pub mod subscriber;

#[cfg(test)]
pub(crate) mod testing;

pub use client::NatsClient;
pub use publisher::NatsPublisher;
pub use stream::StreamConfig;
pub use subscriber::{SubscribeOptions, SubscriptionHandle};

#[cfg(feature = "nats-kv-inbox")]
pub use inbox::nats_kv::NatsKvIdempotencyStore;

#[cfg(feature = "redis-inbox")]
pub use inbox::redis::RedisIdempotencyStore;
