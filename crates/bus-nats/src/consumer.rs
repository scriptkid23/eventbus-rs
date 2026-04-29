use async_nats::jetstream::consumer::{pull, AckPolicy, DeliverPolicy};
use std::time::Duration;

/// Build a pull consumer config for durable, exactly-once-capable consumption.
pub fn build_pull_config(
    durable:     &str,
    filter:      &str,
    max_deliver: i64,
    ack_wait:    Duration,
    backoff:     &[Duration],
) -> pull::Config {
    pull::Config {
        durable_name:   Some(durable.into()),
        filter_subject: filter.into(),
        ack_policy:     AckPolicy::Explicit,
        deliver_policy: DeliverPolicy::All,
        ack_wait,
        max_deliver,
        backoff:        backoff.to_vec(),
        ..Default::default()
    }
}
