use crate::stream::{ensure_stream, StreamConfig};
use async_nats::jetstream;
use bus_core::error::BusError;

/// Thin wrapper over `async_nats::Client` + `jetstream::Context`.
#[derive(Clone)]
pub struct NatsClient {
    pub(crate) js: jetstream::Context,
}

impl NatsClient {
    /// Connect to NATS at `url` and ensure the stream exists.
    pub async fn connect(url: &str, stream_cfg: &StreamConfig) -> Result<Self, BusError> {
        let client = async_nats::connect(url)
            .await
            .map_err(|e| BusError::Nats(e.to_string()))?;

        let js = jetstream::new(client);

        ensure_stream(&js, stream_cfg)
            .await
            .map_err(|e| BusError::Nats(e.to_string()))?;

        Ok(Self { js })
    }

    /// Return a reference to the JetStream context.
    pub fn jetstream(&self) -> &jetstream::Context {
        &self.js
    }
}
