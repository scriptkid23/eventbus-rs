//! JetStream advisory logging: opt-in background task that subscribes to
//! `$JS.EVENT.ADVISORY.>` (configurable) and emits structured [`tracing`] events
//! (target `bus_nats::jetstream_advisory`). Requires NATS permission to subscribe
//! to the chosen pattern.

use std::time::Duration;

use async_nats::jetstream;
use bus_core::error::BusError;
use futures_util::StreamExt;
use tokio::task::JoinHandle;
use tracing::Instrument;

const TRACING_TARGET: &str = "bus_nats::jetstream_advisory";

/// Default NATS subject wildcard for JetStream advisories (excludes metrics).
pub const DEFAULT_ADVISORY_SUBJECT_FILTER: &str = "$JS.EVENT.ADVISORY.>";

/// Options for [`spawn_jetstream_advisory_logger`].
#[derive(Debug, Clone)]
pub struct AdvisoryLogOptions {
    /// Core NATS wildcard, e.g. [`DEFAULT_ADVISORY_SUBJECT_FILTER`].
    pub subject_filter: String,
    /// Max bytes of lossy UTF-8 prefix logged at `debug` when JSON parse fails.
    pub max_payload_bytes_log: usize,
}

impl Default for AdvisoryLogOptions {
    fn default() -> Self {
        Self {
            subject_filter: DEFAULT_ADVISORY_SUBJECT_FILTER.to_string(),
            max_payload_bytes_log: 256,
        }
    }
}

/// Drop aborts the background advisory logging task.
pub struct AdvisoryLoggerHandle {
    task_handle: JoinHandle<()>,
}

impl Drop for AdvisoryLoggerHandle {
    fn drop(&mut self) {
        self.task_handle.abort();
    }
}

/// Fields extracted from an advisory JSON payload for structured logging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExtractedAdvisoryFields {
    pub(crate) advisory_type: Option<String>,
    pub(crate) stream: Option<String>,
    pub(crate) consumer: Option<String>,
    pub(crate) json_ok: bool,
    pub(crate) payload_len: usize,
}

/// Best-effort parse: object with string keys `type`, `stream`, `consumer`.
pub(crate) fn extract_advisory_fields(payload: &[u8]) -> ExtractedAdvisoryFields {
    let payload_len = payload.len();
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(payload) else {
        return ExtractedAdvisoryFields {
            advisory_type: None,
            stream: None,
            consumer: None,
            json_ok: false,
            payload_len,
        };
    };

    let Some(obj) = value.as_object() else {
        return ExtractedAdvisoryFields {
            advisory_type: None,
            stream: None,
            consumer: None,
            json_ok: true,
            payload_len,
        };
    };

    let str_or_none = |key: &str| {
        obj.get(key)
            .and_then(|field_value| field_value.as_str())
            .map(std::string::ToString::to_string)
    };

    ExtractedAdvisoryFields {
        advisory_type: str_or_none("type"),
        stream: str_or_none("stream"),
        consumer: str_or_none("consumer"),
        json_ok: true,
        payload_len,
    }
}

fn log_one_advisory(subject: &str, payload: &[u8], max_payload_bytes_log: usize) {
    let extracted_fields = extract_advisory_fields(payload);
    if !extracted_fields.json_ok {
        tracing::warn!(
            target: TRACING_TARGET,
            subject = subject,
            payload_len = extracted_fields.payload_len,
            "jetstream advisory payload is not JSON"
        );

        if max_payload_bytes_log > 0 {
            let preview_length = max_payload_bytes_log.min(payload.len());
            let preview = String::from_utf8_lossy(&payload[..preview_length]);
            tracing::debug!(
                target: TRACING_TARGET,
                subject = subject,
                preview = %preview,
                "jetstream advisory raw prefix (lossy UTF-8)"
            );
        }

        return;
    }

    tracing::info!(
        target: TRACING_TARGET,
        subject = subject,
        advisory_type = extracted_fields.advisory_type.as_deref(),
        stream = extracted_fields.stream.as_deref(),
        consumer = extracted_fields.consumer.as_deref(),
        payload_len = extracted_fields.payload_len,
        "jetstream advisory"
    );
}

/// Subscribe to JetStream advisory subjects and log each message via [`tracing`].
///
/// Call once per NATS connection (or process). Requires the NATS user to have
/// subscribe permission on the configured pattern (default `$JS.EVENT.ADVISORY.>`).
///
/// On subscription stream failure, reconnects with exponential backoff (200ms .. 30s).
pub async fn spawn_jetstream_advisory_logger(
    js: &jetstream::Context,
    opts: AdvisoryLogOptions,
) -> Result<AdvisoryLoggerHandle, BusError> {
    let client = js.client();
    let subject_filter = opts.subject_filter.clone();
    let max_payload_bytes_log = opts.max_payload_bytes_log;

    let mut subscriber = client
        .subscribe(subject_filter.clone())
        .await
        .map_err(|error| BusError::Nats(error.to_string()))?;

    let cloned_client = client.clone();
    let task_handle = tokio::spawn(
        async move {
            let mut backoff_duration = Duration::from_millis(200);
            loop {
                while let Some(message) = subscriber.next().await {
                    let subject = message.subject.as_str();
                    log_one_advisory(subject, &message.payload, max_payload_bytes_log);
                }

                tracing::error!(
                    target: TRACING_TARGET,
                    "jetstream advisory subscription stream ended; reconnecting after backoff"
                );
                tokio::time::sleep(backoff_duration).await;
                backoff_duration = (backoff_duration * 2).min(Duration::from_secs(30));

                loop {
                    match cloned_client.subscribe(subject_filter.clone()).await {
                        Ok(resubscribed_stream) => {
                            subscriber = resubscribed_stream;
                            backoff_duration = Duration::from_millis(200);
                            break;
                        }
                        Err(error) => {
                            tracing::error!(
                                target: TRACING_TARGET,
                                error = %error,
                                "jetstream advisory resubscribe failed; retrying after backoff"
                            );
                            tokio::time::sleep(backoff_duration).await;
                            backoff_duration = (backoff_duration * 2).min(Duration::from_secs(30));
                        }
                    }
                }
            }
        }
        .instrument(tracing::debug_span!(
            target: TRACING_TARGET,
            "jetstream_advisory_logger"
        )),
    );

    Ok(AdvisoryLoggerHandle { task_handle })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_invalid_json() {
        let payload_bytes = [0xff, 0xfe, 0xfd];
        let extracted_fields = extract_advisory_fields(&payload_bytes);
        assert!(!extracted_fields.json_ok);
        assert_eq!(extracted_fields.payload_len, 3);
        assert!(extracted_fields.advisory_type.is_none());
    }

    #[test]
    fn extract_full_object() {
        let payload_json = br#"{"type":"io.nats.jetstream.advisory.v1.max_deliver","stream":"EVENTS","consumer":"w1"}"#;
        let extracted_fields = extract_advisory_fields(payload_json);
        assert!(extracted_fields.json_ok);
        assert_eq!(
            extracted_fields.advisory_type.as_deref(),
            Some("io.nats.jetstream.advisory.v1.max_deliver")
        );
        assert_eq!(extracted_fields.stream.as_deref(), Some("EVENTS"));
        assert_eq!(extracted_fields.consumer.as_deref(), Some("w1"));
    }

    #[test]
    fn extract_array_json_no_keys() {
        let payload_json = br#"[1,2]"#;
        let extracted_fields = extract_advisory_fields(payload_json);
        assert!(extracted_fields.json_ok);
        assert!(extracted_fields.advisory_type.is_none());
    }

    #[test]
    fn extract_non_string_type_ignored() {
        let payload_json = br#"{"type":1}"#;
        let extracted_fields = extract_advisory_fields(payload_json);
        assert!(extracted_fields.json_ok);
        assert!(extracted_fields.advisory_type.is_none());
    }
}
