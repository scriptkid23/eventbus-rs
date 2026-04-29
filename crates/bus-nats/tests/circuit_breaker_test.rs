use bus_nats::circuit_breaker::{CircuitBreaker, CircuitState};
use std::time::Duration;
use tokio::time::sleep;

#[tokio::test]
async fn starts_closed() {
    let cb = CircuitBreaker::new(0.5, 10, Duration::from_millis(100));
    assert_eq!(cb.state(), CircuitState::Closed);
}

#[tokio::test]
async fn opens_after_failure_threshold() {
    let cb = CircuitBreaker::new(0.5, 4, Duration::from_millis(200));
    // 3 failures out of 4 calls = 75% > 50% threshold
    cb.record_failure();
    cb.record_failure();
    cb.record_success();
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);
}

#[tokio::test]
async fn transitions_to_half_open_after_timeout() {
    let cb = CircuitBreaker::new(0.5, 2, Duration::from_millis(100));
    cb.record_failure();
    cb.record_failure();
    assert_eq!(cb.state(), CircuitState::Open);

    sleep(Duration::from_millis(150)).await;
    assert_eq!(cb.state(), CircuitState::HalfOpen);
}

#[tokio::test]
async fn closes_after_half_open_success() {
    let cb = CircuitBreaker::new(0.5, 2, Duration::from_millis(100));
    cb.record_failure();
    cb.record_failure();
    sleep(Duration::from_millis(150)).await;
    assert_eq!(cb.state(), CircuitState::HalfOpen);

    cb.record_success();
    assert_eq!(cb.state(), CircuitState::Closed);
}
