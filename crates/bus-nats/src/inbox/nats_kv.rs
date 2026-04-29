use async_nats::jetstream::{self, kv};
use async_trait::async_trait;
use bus_core::{error::BusError, id::MessageId, idempotency::IdempotencyStore};
use bytes::Bytes;
use std::time::Duration;

/// Idempotency store backed by NATS JetStream Key-Value store.
///
/// Uses `kv::Store::create` for atomic, first-writer-wins semantics, ensuring
/// that concurrent consumers cannot both claim the same message ID.
#[derive(Clone)]
pub struct NatsKvIdempotencyStore {
    store: kv::Store,
}

impl NatsKvIdempotencyStore {
    /// Create or bind to the `eventbus_processed` KV bucket.
    ///
    /// `max_age` controls how long processed keys are retained before
    /// automatic expiry.
    pub async fn new(js: jetstream::Context, max_age: Duration) -> Result<Self, BusError> {
        let store = js
            .create_key_value(kv::Config {
                bucket: "eventbus_processed".into(),
                history: 1,
                max_age,
                num_replicas: 1,
                ..Default::default()
            })
            .await
            .map_err(|e| BusError::Idempotency(e.to_string()))?;

        Ok(Self { store })
    }
}

#[async_trait]
impl IdempotencyStore for NatsKvIdempotencyStore {
    /// Atomically claim a message ID.
    ///
    /// Returns `Ok(true)` when this call is the first to claim the key.
    /// Returns `Ok(false)` when the key already exists (duplicate delivery).
    async fn try_insert(&self, key: &MessageId, _ttl: Duration) -> Result<bool, BusError> {
        match self
            .store
            .create(key.to_string(), Bytes::from_static(b"pending"))
            .await
        {
            Ok(_) => Ok(true),
            Err(_) => Ok(false),
        }
    }

    /// Update a previously inserted key to `"done"`.
    ///
    /// Idempotent: a second call on an already-done key is harmless.
    async fn mark_done(&self, key: &MessageId) -> Result<(), BusError> {
        self.store
            .put(key.to_string(), Bytes::from_static(b"done"))
            .await
            .map_err(|e| BusError::Idempotency(e.to_string()))?;
        Ok(())
    }
}
