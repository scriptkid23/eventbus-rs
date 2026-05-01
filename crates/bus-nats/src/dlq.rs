//! Dead Letter Queue (DLQ) support using Pattern B: worker-side republish.
//!
//! A subscriber publishes terminal failures to a per-consumer DLQ stream and
//! waits for the JetStream pub-ack before terming the original message.

use async_nats::{
    HeaderMap,
    jetstream::{self, stream},
};
use bus_core::error::BusError;
use bytes::Bytes;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SECONDS_PER_MINUTE: u64 = 60;
const SECONDS_PER_HOUR: u64 = 60 * SECONDS_PER_MINUTE;
const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;

pub const DEFAULT_DLQ_MAX_AGE_DAYS: u64 = 30;
pub const DEFAULT_DLQ_DUPLICATE_WINDOW_MIN: u64 = 5;
pub const DEFAULT_DLQ_REPLICAS: usize = 3;

pub const DEFAULT_DLQ_MAX_AGE: Duration =
    Duration::from_secs(DEFAULT_DLQ_MAX_AGE_DAYS * SECONDS_PER_DAY);

pub const DEFAULT_DLQ_DUPLICATE_WINDOW: Duration =
    Duration::from_secs(DEFAULT_DLQ_DUPLICATE_WINDOW_MIN * SECONDS_PER_MINUTE);

pub const FALLBACK_NAK_DELAY: Duration = Duration::from_secs(5);
pub const DEFAULT_DLQ_FAILURE_NAK_DELAY: Duration = Duration::from_secs(5);

pub const CLASS_PERMANENT: &str = "permanent";
pub const CLASS_POISON: &str = "poison";
pub const CLASS_TRANSIENT_EXHAUSTED: &str = "transient_exhausted";

pub const REASON_INVALID_PAYLOAD: &str = "invalid_payload";
pub const REASON_HANDLER_PERMANENT: &str = "handler_permanent";
pub const REASON_MAX_RETRIES_EXCEEDED: &str = "max_retries_exceeded";

pub const HDR_NATS_MSG_ID: &str = "Nats-Msg-Id";
pub const HDR_ORIGINAL_SUBJECT: &str = "X-Original-Subject";
pub const HDR_ORIGINAL_STREAM: &str = "X-Original-Stream";
pub const HDR_ORIGINAL_SEQ: &str = "X-Original-Seq";
pub const HDR_ORIGINAL_MSG_ID: &str = "X-Original-Msg-Id";
pub const HDR_FAILURE_REASON: &str = "X-Failure-Reason";
pub const HDR_FAILURE_CLASS: &str = "X-Failure-Class";
pub const HDR_FAILURE_DETAIL: &str = "X-Failure-Detail";
pub const HDR_RETRY_COUNT: &str = "X-Retry-Count";
pub const HDR_CONSUMER: &str = "X-Consumer";
pub const HDR_FAILED_AT: &str = "X-Failed-At";

/// Configuration for a per-consumer DLQ stream.
#[derive(Debug, Clone)]
pub struct DlqConfig {
    pub num_replicas: usize,
    pub max_age: Duration,
    pub max_bytes: Option<i64>,
    pub duplicate_window: Duration,
    pub deny_delete: bool,
    pub deny_purge: bool,
    pub allow_direct: bool,
    /// Maximum time to wait for the JetStream pub-ack on a DLQ publish.
    /// `None` (default) defers to the async-nats client request timeout.
    pub publish_ack_timeout: Option<Duration>,
    /// Delay applied via `Nak` on the source message when DLQ publish fails.
    /// Default: [`DEFAULT_DLQ_FAILURE_NAK_DELAY`].
    pub failure_nak_delay: Duration,
}

impl Default for DlqConfig {
    fn default() -> Self {
        Self {
            num_replicas: DEFAULT_DLQ_REPLICAS,
            max_age: DEFAULT_DLQ_MAX_AGE,
            max_bytes: None,
            duplicate_window: DEFAULT_DLQ_DUPLICATE_WINDOW,
            deny_delete: true,
            deny_purge: false,
            allow_direct: true,
            publish_ack_timeout: None,
            failure_nak_delay: DEFAULT_DLQ_FAILURE_NAK_DELAY,
        }
    }
}

/// Per-subscribe DLQ options.
#[derive(Debug, Clone, Default)]
pub struct DlqOptions {
    pub config: DlqConfig,
}

/// Failure metadata copied into DLQ headers.
#[derive(Debug, Clone)]
pub struct FailureInfo {
    pub original_subject: String,
    pub original_stream: String,
    pub original_seq: u64,
    pub original_msg_id: String,
    pub consumer: String,
    pub delivered: u64,
    pub failure_reason: String,
    pub failure_class: String,
    pub failure_detail: String,
}

pub fn dlq_stream_name(source_stream: &str, durable: &str) -> String {
    format!("DLQ_{}_{}", source_stream, durable)
}

pub fn dlq_subject(source_stream: &str, durable: &str) -> String {
    format!("dlq.{}.{}", source_stream, durable)
}

pub fn build_dlq_headers(failure_info: &FailureInfo) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let dedup_id = format!(
        "{}:{}:{}:{}",
        failure_info.original_stream,
        failure_info.original_seq,
        failure_info.consumer,
        failure_info.delivered
    );

    headers.insert(HDR_NATS_MSG_ID, dedup_id.as_str());
    headers.insert(HDR_ORIGINAL_SUBJECT, failure_info.original_subject.as_str());
    headers.insert(HDR_ORIGINAL_STREAM, failure_info.original_stream.as_str());
    headers.insert(
        HDR_ORIGINAL_SEQ,
        failure_info.original_seq.to_string().as_str(),
    );
    headers.insert(HDR_ORIGINAL_MSG_ID, failure_info.original_msg_id.as_str());
    headers.insert(HDR_FAILURE_REASON, failure_info.failure_reason.as_str());
    headers.insert(HDR_FAILURE_CLASS, failure_info.failure_class.as_str());
    headers.insert(HDR_FAILURE_DETAIL, failure_info.failure_detail.as_str());
    headers.insert(HDR_RETRY_COUNT, failure_info.delivered.to_string().as_str());
    headers.insert(HDR_CONSUMER, failure_info.consumer.as_str());
    headers.insert(HDR_FAILED_AT, unix_timestamp_seconds().as_str());

    headers
}

pub async fn ensure_dlq_stream(
    js: &jetstream::Context,
    stream_name: &str,
    subject: &str,
    config: &DlqConfig,
) -> Result<stream::Stream, BusError> {
    let mut stream_config = stream::Config {
        name: stream_name.to_string(),
        subjects: vec![subject.to_string()],
        retention: stream::RetentionPolicy::Limits,
        storage: stream::StorageType::File,
        discard: stream::DiscardPolicy::Old,
        max_age: config.max_age,
        duplicate_window: config.duplicate_window,
        num_replicas: config.num_replicas,
        deny_delete: config.deny_delete,
        deny_purge: config.deny_purge,
        allow_direct: config.allow_direct,
        ..Default::default()
    };

    if let Some(max_bytes) = config.max_bytes {
        stream_config.max_bytes = max_bytes;
    }

    js.get_or_create_stream(stream_config)
        .await
        .map_err(|error| BusError::Nats(format!("ensure dlq stream {stream_name}: {error}")))
}

pub async fn publish_to_dlq(
    js: &jetstream::Context,
    subject: &str,
    payload: Bytes,
    headers: HeaderMap,
    publish_ack_timeout: Option<Duration>,
) -> Result<(), BusError> {
    let publish_fut = js.publish_with_headers(subject.to_string(), headers, payload);

    let publish_ack = match publish_ack_timeout {
        Some(timeout) => tokio::time::timeout(timeout, publish_fut)
            .await
            .map_err(|_| BusError::Nats("dlq publish send timeout".to_string()))?,
        None => publish_fut.await,
    }
    .map_err(|error| BusError::Nats(format!("dlq publish send: {error}")))?;

    match publish_ack_timeout {
        Some(timeout) => tokio::time::timeout(timeout, publish_ack)
            .await
            .map_err(|_| BusError::Nats("dlq publish ack timeout".to_string()))?,
        None => publish_ack.await,
    }
    .map_err(|error| BusError::Nats(format!("dlq publish ack: {error}")))?;

    Ok(())
}

fn unix_timestamp_seconds() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}
