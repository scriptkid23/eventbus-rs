use bus_macros::Event;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.created")]
struct BadEvent {
    total: i64,
}

fn main() {}
