pub mod builder;
pub mod bus;
pub mod prelude;

pub use builder::EventBusBuilder;
pub use bus::{EventBus, SubscriptionHandle};

// Re-export the underlying transport crate as a namespace, so application code
// that depends only on `eventbus-nats` can still reach less-common types via
// `eventbus_nats::nats::...` without adding `bus-nats` to its `Cargo.toml`.
pub use bus_nats as nats;

// Re-export the trait crate. Useful when an app needs `bus_core` types
// directly (manual `Event` impls, custom `IdempotencyStore`, …) without
// declaring `bus-core` itself.
pub use bus_core as core;

// Curated shortcuts for the most common transport types so app boot code
// reads `use eventbus_nats::{NatsClient, StreamConfig, ...};` instead of
// reaching into the namespace.
pub use bus_nats::{ConnectOptions, NatsClient, NatsPublisher, StreamConfig, SubscribeOptions};

#[cfg(feature = "nats-kv-inbox")]
pub use bus_nats::NatsKvIdempotencyConfig;

#[cfg(feature = "redis-inbox")]
pub use bus_nats::{RedisIdempotencyConfig, RedisIdempotencyStore};
