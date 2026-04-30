use bus_core::MessageId;
use bus_macros::Event;
use event_bus::saga::orchestration::{SagaDefinition, SagaTransition};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.created")]
struct OrderCreated {
    id:     MessageId,
    amount: i64,
}

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "inventory.reserve")]
struct ReserveInventory {
    id:       MessageId,
    order_id: String,
}

#[derive(Debug, Serialize, Deserialize, Event)]
#[event(subject = "orders.cancelled")]
struct OrderCancelled {
    id:     MessageId,
    reason: String,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq)]
enum OrderSagaState {
    WaitingInventory,
    WaitingPayment,
    Completed,
}

struct OrderSaga {
    id: String,
}

impl SagaDefinition for OrderSaga {
    type State = OrderSagaState;
    type Event = OrderCreated;

    fn saga_id(&self) -> &str {
        &self.id
    }

    fn transition(
        &self,
        state: &Self::State,
        event: &Self::Event,
    ) -> SagaTransition<Self::State> {
        match state {
            OrderSagaState::WaitingInventory => SagaTransition::Advance {
                next_state: OrderSagaState::WaitingPayment,
                emit:       vec![Box::new(ReserveInventory {
                    id:       MessageId::new(),
                    order_id: event.id.to_string(),
                })],
            },
            OrderSagaState::WaitingPayment => SagaTransition::Complete,
            OrderSagaState::Completed => SagaTransition::Fail("already completed".into()),
        }
    }
}

#[test]
fn transition_advance_emits_command() {
    let saga = OrderSaga {
        id: "order-123".into(),
    };
    let evt = OrderCreated {
        id:     MessageId::new(),
        amount: 100,
    };

    let result = saga.transition(&OrderSagaState::WaitingInventory, &evt);
    assert!(matches!(result, SagaTransition::Advance { .. }));
}

#[test]
fn transition_from_waiting_payment_completes() {
    let saga = OrderSaga {
        id: "order-456".into(),
    };
    let evt = OrderCreated {
        id:     MessageId::new(),
        amount: 50,
    };

    let result = saga.transition(&OrderSagaState::WaitingPayment, &evt);
    assert!(matches!(result, SagaTransition::Complete));
}

#[test]
fn transition_from_completed_fails() {
    let saga = OrderSaga {
        id: "order-789".into(),
    };
    let evt = OrderCreated {
        id:     MessageId::new(),
        amount: 25,
    };

    let result = saga.transition(&OrderSagaState::Completed, &evt);
    assert!(matches!(result, SagaTransition::Fail(_)));
}

// OrderCancelled is referenced as a potential compensation event in the design;
// the assertion below pins it to make sure the type stays in scope.
#[allow(dead_code)]
fn _compensation_type_is_event(_e: OrderCancelled) {}
