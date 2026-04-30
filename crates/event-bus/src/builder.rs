use crate::bus::EventBus;
use bus_core::error::BusError;

pub struct EventBusBuilder;

impl EventBusBuilder {
    pub fn new() -> Self {
        Self
    }

    pub async fn build(self) -> Result<EventBus, BusError> {
        todo!("implemented in Task 6")
    }
}

impl Default for EventBusBuilder {
    fn default() -> Self {
        Self::new()
    }
}
