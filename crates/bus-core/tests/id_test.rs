use bus_core::MessageId;
use std::str::FromStr;

#[test]
fn new_ids_are_unique() {
    let first_message_id = MessageId::new();
    let second_message_id = MessageId::new();

    assert_ne!(first_message_id, second_message_id);
}

#[test]
fn id_roundtrips_through_string() {
    let message_id = MessageId::new();
    let message_id_string = message_id.to_string();
    let parsed_message_id = MessageId::from_str(&message_id_string).unwrap();

    assert_eq!(message_id, parsed_message_id);
}

#[test]
fn ids_are_monotonically_increasing() {
    let earlier_message_id = MessageId::new();
    std::thread::sleep(std::time::Duration::from_millis(2));
    let later_message_id = MessageId::new();

    assert!(
        earlier_message_id < later_message_id,
        "earlier MessageId must sort before later one"
    );
    assert!(
        earlier_message_id.to_string() < later_message_id.to_string(),
        "string representation must preserve order"
    );
}

#[test]
fn from_str_rejects_invalid_uuid() {
    let parse_result = "not-a-uuid".parse::<MessageId>();

    assert!(parse_result.is_err());
}
