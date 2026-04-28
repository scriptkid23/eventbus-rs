use bus_core::{Event, MessageId};
use bus_macros::Event;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.created")]
struct OrderCreated {
    id: MessageId,
    total: i64,
}

#[test]
fn static_subject() {
    let event = OrderCreated {
        id: MessageId::new(),
        total: 100,
    };

    assert_eq!(event.subject(), "orders.created");
}

#[test]
fn message_id_returns_id_field() {
    let message_id = MessageId::new();
    let event = OrderCreated {
        id: message_id.clone(),
        total: 50,
    };

    assert_eq!(event.message_id(), message_id);
}

#[test]
fn default_aggregate_type() {
    assert_eq!(OrderCreated::aggregate_type(), "default");
}

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.{self.order_id}.shipped", aggregate = "order")]
struct OrderShipped {
    id: MessageId,
    order_id: Uuid,
}

#[test]
fn interpolated_subject() {
    let order_id = Uuid::now_v7();
    let event = OrderShipped {
        id: MessageId::new(),
        order_id,
    };

    assert_eq!(event.subject(), format!("orders.{order_id}.shipped"));
}

#[test]
fn custom_aggregate_type() {
    assert_eq!(OrderShipped::aggregate_type(), "order");
}
