use crate::bus::EventBus;
use bus_core::{error::BusError, idempotency::IdempotencyStore};
use bus_nats::{DlqConfig, NatsClient, StreamConfig};
use std::{path::PathBuf, sync::Arc, time::Duration};

pub struct EventBusBuilder {
    url: Option<String>,
    stream_cfg: StreamConfig,
    idempotency: Option<Arc<dyn IdempotencyStore>>,
    sqlite_path: Option<PathBuf>,
    otel: bool,
    dlq: Option<DlqConfig>,
}

impl EventBusBuilder {
    pub fn new() -> Self {
        Self {
            url: None,
            stream_cfg: StreamConfig::default(),
            idempotency: None,
            sqlite_path: None,
            otel: false,
            dlq: None,
        }
    }

    pub fn url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    pub fn stream_config(mut self, cfg: StreamConfig) -> Self {
        self.stream_cfg = cfg;
        self
    }

    pub fn replicas(mut self, n: usize) -> Self {
        self.stream_cfg.num_replicas = n;
        self
    }

    pub fn dedup_window(mut self, d: Duration) -> Self {
        self.stream_cfg.duplicate_window = d;
        self
    }

    pub fn stream_name(mut self, name: impl Into<String>) -> Self {
        self.stream_cfg.name = name.into();
        self
    }

    /// Required — no default backend. Must call exactly once before build().
    pub fn idempotency(mut self, store: impl IdempotencyStore + 'static) -> Self {
        self.idempotency = Some(Arc::new(store));
        self
    }

    pub fn sqlite_buffer(mut self, path: impl Into<PathBuf>) -> Self {
        self.sqlite_path = Some(path.into());
        self
    }

    pub fn with_otel(mut self) -> Self {
        self.otel = true;
        self
    }

    /// Enable per-consumer DLQ streams for subscribers created by this bus.
    pub fn with_dlq(mut self, cfg: DlqConfig) -> Self {
        self.dlq = Some(cfg);
        self
    }

    /// Build the `EventBus`. Returns `Err` if `idempotency()` was not called.
    pub async fn build(self) -> Result<EventBus, BusError> {
        let url = self
            .url
            .ok_or_else(|| BusError::Publish("url is required".into()))?;
        let idempotency = self.idempotency.ok_or_else(|| {
            BusError::Idempotency(
                "idempotency store is required — call .idempotency(store) before build()".into(),
            )
        })?;

        let client = NatsClient::connect(&url, &self.stream_cfg).await?;

        Ok(EventBus::new(client, idempotency, self.dlq))
    }
}

impl Default for EventBusBuilder {
    fn default() -> Self {
        Self::new()
    }
}
