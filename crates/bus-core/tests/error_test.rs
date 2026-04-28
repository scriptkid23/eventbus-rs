use bus_core::{BusError, HandlerError};

#[test]
fn handler_error_display_transient() {
    let handler_error = HandlerError::Transient("db timeout".into());

    assert_eq!(handler_error.to_string(), "transient: db timeout");
}

#[test]
fn handler_error_display_permanent() {
    let handler_error = HandlerError::Permanent("invalid payload".into());

    assert_eq!(handler_error.to_string(), "permanent: invalid payload");
}

#[test]
fn bus_error_from_handler_error() {
    let handler_error = HandlerError::Transient("x".into());
    let bus_error: BusError = handler_error.into();

    assert!(matches!(bus_error, BusError::Handler(_)));
}

#[test]
fn bus_error_nats_unavailable_display() {
    let bus_error = BusError::NatsUnavailable;

    assert_eq!(bus_error.to_string(), "nats unavailable");
}

#[test]
fn bus_error_from_serde_json() {
    let json_error = serde_json::from_str::<serde_json::Value>("not json").unwrap_err();
    let bus_error: BusError = json_error.into();

    assert!(matches!(bus_error, BusError::Serde(_)));
}
