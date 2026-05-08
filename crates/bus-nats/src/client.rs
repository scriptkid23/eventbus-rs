use crate::stream::{StreamConfig, ensure_stream};
use async_nats::jetstream;
use bus_core::error::BusError;

/// Thin wrapper over `async_nats::Client` + `jetstream::Context`.
#[derive(Clone)]
pub struct NatsClient {
    pub(crate) js: jetstream::Context,
}

impl NatsClient {
    /// Connect with default options. Equivalent to
    /// `connect_with_options(url, ConnectOptions::default(), stream_cfg)`.
    pub async fn connect(url: &str, stream_cfg: &StreamConfig) -> Result<Self, BusError> {
        Self::connect_with_options(url, async_nats::ConnectOptions::default(), stream_cfg).await
    }

    /// Connect with caller-supplied `async_nats::ConnectOptions`. Use for auth,
    /// TLS, cluster URL lists, custom ping interval, etc.
    pub async fn connect_with_options(
        url: &str,
        options: async_nats::ConnectOptions,
        stream_cfg: &StreamConfig,
    ) -> Result<Self, BusError> {
        let client = options
            .connect(url)
            .await
            .map_err(|e| BusError::Nats(e.to_string()))?;
        let js = jetstream::new(client);
        ensure_stream(&js, stream_cfg)
            .await
            .map_err(|e| BusError::Nats(e.to_string()))?;
        Ok(Self { js })
    }

    pub fn jetstream(&self) -> &jetstream::Context {
        &self.js
    }
}
