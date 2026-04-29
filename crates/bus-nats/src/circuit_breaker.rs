use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// The observable state of a [`CircuitBreaker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitState {
    /// All requests are allowed through; the breaker is healthy.
    Closed,
    /// Too many recent failures; all requests are rejected.
    Open,
    /// The reset timeout has elapsed; one probe request is allowed through.
    HalfOpen,
}

struct Inner {
    window:            VecDeque<bool>,
    window_size:       usize,
    failure_threshold: f64,
    opened_at:         Option<Instant>,
    reset_timeout:     Duration,
}

impl Inner {
    fn state(&self) -> CircuitState {
        if let Some(opened_at) = self.opened_at {
            if opened_at.elapsed() >= self.reset_timeout {
                return CircuitState::HalfOpen;
            }
            return CircuitState::Open;
        }
        CircuitState::Closed
    }

    fn failure_rate(&self) -> f64 {
        if self.window.is_empty() {
            return 0.0;
        }
        let failures = self.window.iter().filter(|&&ok| !ok).count();
        failures as f64 / self.window.len() as f64
    }

    fn record(&mut self, success: bool) {
        if self.window.len() >= self.window_size {
            self.window.pop_front();
        }
        self.window.push_back(success);

        // A success while HalfOpen closes the circuit and resets the window.
        if success && self.opened_at.is_some() {
            let state = self.state();
            if state == CircuitState::HalfOpen {
                self.opened_at = None;
                self.window.clear();
                return;
            }
        }

        // Trip the breaker once the window is full and the failure rate exceeds the threshold.
        if self.opened_at.is_none()
            && self.window.len() >= self.window_size
            && self.failure_rate() > self.failure_threshold
        {
            self.opened_at = Some(Instant::now());
        }
    }
}

/// Sliding-window circuit breaker.
///
/// Tracks the last `window_size` outcomes. When the failure rate exceeds
/// `failure_threshold` the breaker trips to [`CircuitState::Open`]. After
/// `reset_timeout` it moves to [`CircuitState::HalfOpen`], and a single
/// successful call closes it again.
#[derive(Clone)]
pub struct CircuitBreaker(Arc<Mutex<Inner>>);

impl CircuitBreaker {
    /// Creates a new [`CircuitBreaker`].
    ///
    /// # Parameters
    /// - `failure_threshold`: fraction of failures (0.0–1.0) that trips the breaker.
    /// - `window_size`: number of most-recent outcomes to consider.
    /// - `reset_timeout`: how long to stay Open before probing with HalfOpen.
    pub fn new(failure_threshold: f64, window_size: usize, reset_timeout: Duration) -> Self {
        Self(Arc::new(Mutex::new(Inner {
            window: VecDeque::with_capacity(window_size),
            window_size,
            failure_threshold,
            opened_at: None,
            reset_timeout,
        })))
    }

    /// Returns the current [`CircuitState`].
    pub fn state(&self) -> CircuitState {
        self.0.lock().unwrap().state()
    }

    /// Records a successful operation.
    pub fn record_success(&self) {
        self.0.lock().unwrap().record(true);
    }

    /// Records a failed operation.
    pub fn record_failure(&self) {
        self.0.lock().unwrap().record(false);
    }

    /// Returns `true` when the circuit is [`Closed`](CircuitState::Closed) or
    /// [`HalfOpen`](CircuitState::HalfOpen) and a request should be attempted.
    pub fn allow_request(&self) -> bool {
        matches!(self.state(), CircuitState::Closed | CircuitState::HalfOpen)
    }
}
