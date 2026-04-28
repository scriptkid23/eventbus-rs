use bus_core::MessageId;
use bus_macros::Event;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Event)]
struct BadEvent {
    id: MessageId,
}

fn main() {}
