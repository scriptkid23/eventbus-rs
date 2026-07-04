use async_nats::jetstream::{self, kv};
use async_trait::async_trait;
use bus_core::{
    error::BusError,
    id::MessageId,
    idempotency::{ClaimOutcome, IdempotencyStore},
};
use bytes::Bytes;
use std::time::Duration;

const KV_PENDING: &[u8] = b"pending";
const KV_DONE: &[u8] = b"done";

const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

/// Configuration for [`NatsKvIdempotencyStore`].
#[derive(Debug, Clone)]
pub struct NatsKvIdempotencyConfig {
    /// KV bucket name. Default: "eventbus_processed".
    pub bucket: String,
    /// Number of replicas. Default: 1 (single-node dev). For production set to
    /// match your stream's replica count (typically 3).
    pub num_replicas: usize,
    /// Bucket-level TTL applied uniformly to every key.
    pub max_age: Duration,
}

impl Default for NatsKvIdempotencyConfig {
    fn default() -> Self {
        Self {
            bucket: "eventbus_processed".into(),
            num_replicas: 1,
            max_age: Duration::from_secs(7 * SECONDS_PER_DAY),
        }
    }
}

/// Idempotency store backed by NATS JetStream Key-Value store.
///
/// Uses `kv::Store::create` for atomic, first-writer-wins claim semantics.
/// On conflict, `try_claim` reads the existing entry to distinguish a still
/// pending claim from a completed one.
#[derive(Clone)]
pub struct NatsKvIdempotencyStore {
    store: kv::Store,
}

impl NatsKvIdempotencyStore {
    pub async fn new(
        js: jetstream::Context,
        cfg: NatsKvIdempotencyConfig,
    ) -> Result<Self, BusError> {
        let store = js
            .create_key_value(kv::Config {
                bucket: cfg.bucket,
                history: 1,
                max_age: cfg.max_age,
                num_replicas: cfg.num_replicas,
                ..Default::default()
            })
            .await
            .map_err(|e| BusError::Idempotency(e.to_string()))?;

        Ok(Self { store })
    }
}

#[async_trait]
impl IdempotencyStore for NatsKvIdempotencyStore {
    async fn try_claim(&self, key: &MessageId) -> Result<ClaimOutcome, BusError> {
        let key_str = key.to_string();

        match self
            .store
            .create(&key_str, Bytes::from_static(KV_PENDING))
            .await
        {
            Ok(_) => Ok(ClaimOutcome::Claimed),
            Err(_) => match self
                .store
                .get(&key_str)
                .await
                .map_err(|e| BusError::Idempotency(e.to_string()))?
            {
                Some(value) if value.as_ref() == KV_DONE => Ok(ClaimOutcome::AlreadyDone),
                Some(_) => Ok(ClaimOutcome::AlreadyPending),
                None => Ok(ClaimOutcome::Claimed),
            },
        }
    }

    async fn mark_done(&self, key: &MessageId) -> Result<(), BusError> {
        self.store
            .put(key.to_string(), Bytes::from_static(KV_DONE))
            .await
            .map_err(|e| BusError::Idempotency(e.to_string()))?;
        Ok(())
    }

    async fn release(&self, key: &MessageId) -> Result<(), BusError> {
        self.store
            .purge(key.to_string())
            .await
            .map_err(|e| BusError::Idempotency(e.to_string()))?;
        Ok(())
    }
}
