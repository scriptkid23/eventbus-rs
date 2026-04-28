use bus_core::MessageId;
use bus_macros::Event;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.{self.order_id}.created")]
struct BadEvent {
    id: MessageId,
}

fn main() {}
