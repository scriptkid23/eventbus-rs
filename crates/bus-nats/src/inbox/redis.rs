use async_trait::async_trait;
use bus_core::{
    error::BusError,
    id::MessageId,
    idempotency::{ClaimOutcome, IdempotencyStore},
};
use std::time::Duration;

const SECONDS_PER_DAY: u64 = 24 * 60 * 60;

const CLAIM_LUA: &str = r#"
local current = redis.call('GET', KEYS[1])
if current == false then
  redis.call('SET', KEYS[1], 'pending', 'PX', ARGV[1])
  return 'claimed'
elseif current == 'done' then
  return 'already_done'
else
  return 'already_pending'
end
"#;

/// Configuration for [`RedisIdempotencyStore`].
#[derive(Debug, Clone)]
pub struct RedisIdempotencyConfig {
    /// Redis URL — `redis://host:6379` or `rediss://...` for TLS.
    pub url: String,
    /// Prefix prepended to msg_id when storing keys. Default: `eventbus:processed:`.
    pub key_prefix: String,
    /// TTL applied to every claim/done entry. Default: 7 days.
    /// Mirrors `NatsKvIdempotencyConfig.max_age` semantics.
    pub ttl: Duration,
}

impl Default for RedisIdempotencyConfig {
    fn default() -> Self {
        Self {
            url: "redis://localhost:6379".into(),
            key_prefix: "eventbus:processed:".into(),
            ttl: Duration::from_secs(7 * SECONDS_PER_DAY),
        }
    }
}

/// Redis-backed [`IdempotencyStore`] using an atomic Lua claim.
#[derive(Clone)]
pub struct RedisIdempotencyStore {
    conn: redis::aio::ConnectionManager,
    key_prefix: String,
    ttl_ms: u64,
    claim_script: redis::Script,
}

impl RedisIdempotencyStore {
    pub async fn connect(cfg: RedisIdempotencyConfig) -> Result<Self, BusError> {
        let client = redis::Client::open(cfg.url)
            .map_err(|e| BusError::Idempotency(format!("redis: {e}")))?;
        let conn = redis::aio::ConnectionManager::new(client)
            .await
            .map_err(|e| BusError::Idempotency(format!("redis: {e}")))?;
        let ttl_ms = u64::try_from(cfg.ttl.as_millis()).unwrap_or(u64::MAX);
        Ok(Self {
            conn,
            key_prefix: cfg.key_prefix,
            ttl_ms,
            claim_script: redis::Script::new(CLAIM_LUA),
        })
    }

    fn full_key(&self, key: &MessageId) -> String {
        format!("{}{}", self.key_prefix, key)
    }
}

#[async_trait]
impl IdempotencyStore for RedisIdempotencyStore {
    async fn try_claim(&self, key: &MessageId, _ttl: Duration) -> Result<ClaimOutcome, BusError> {
        let full_key = self.full_key(key);
        let mut conn = self.conn.clone();
        let outcome: String = self
            .claim_script
            .key(&full_key)
            .arg(self.ttl_ms)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| BusError::Idempotency(format!("redis: {e}")))?;

        match outcome.as_str() {
            "claimed" => Ok(ClaimOutcome::Claimed),
            "already_done" => Ok(ClaimOutcome::AlreadyDone),
            "already_pending" => Ok(ClaimOutcome::AlreadyPending),
            other => Err(BusError::Idempotency(format!(
                "redis: unexpected claim outcome {other}"
            ))),
        }
    }

    async fn mark_done(&self, key: &MessageId) -> Result<(), BusError> {
        let full_key = self.full_key(key);
        let mut conn = self.conn.clone();
        redis::cmd("SET")
            .arg(&full_key)
            .arg("done")
            .arg("PX")
            .arg(self.ttl_ms)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| BusError::Idempotency(format!("redis: {e}")))?;
        Ok(())
    }

    async fn release(&self, key: &MessageId) -> Result<(), BusError> {
        let full_key = self.full_key(key);
        let mut conn = self.conn.clone();
        redis::cmd("DEL")
            .arg(&full_key)
            .query_async::<()>(&mut conn)
            .await
            .map_err(|e| BusError::Idempotency(format!("redis: {e}")))?;
        Ok(())
    }
}
