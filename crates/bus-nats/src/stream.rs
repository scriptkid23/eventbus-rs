use async_nats::jetstream::{self, stream};
use std::time::Duration;

/// Configuration for the JetStream stream.
/// Defaults are production-safe: R3 replication, 5-minute dedup window.
#[derive(Debug, Clone)]
pub struct StreamConfig {
    pub name:             String,
    pub subjects:         Vec<String>,
    pub num_replicas:     usize,
    pub duplicate_window: Duration,
    pub max_age:          Duration,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            name:             "EVENTS".into(),
            subjects:         vec!["events.>".into()],
            num_replicas:     3,
            duplicate_window: Duration::from_secs(5 * 60),
            max_age:          Duration::from_secs(7 * 24 * 3600),
        }
    }
}

/// Ensure the stream exists, creating it if absent. Idempotent.
pub async fn ensure_stream(
    js: &jetstream::Context,
    cfg: &StreamConfig,
) -> Result<stream::Stream, async_nats::Error> {
    js.get_or_create_stream(stream::Config {
        name:             cfg.name.clone(),
        subjects:         cfg.subjects.clone(),
        num_replicas:     cfg.num_replicas,
        storage:          stream::StorageType::File,
        duplicate_window: cfg.duplicate_window,
        discard:          stream::DiscardPolicy::Old,
        max_age:          cfg.max_age,
        allow_direct:     true,
        ..Default::default()
    })
    .await
    .map_err(|e| Box::new(e) as async_nats::Error)
}
