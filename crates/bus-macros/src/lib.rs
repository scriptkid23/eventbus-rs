mod event;

use proc_macro::TokenStream;

/// Derive macro that generates an `impl bus_core::Event` for a struct.
///
/// # Required
/// - The struct must have a field `id: bus_core::MessageId`.
/// - The `#[event(subject = "...")]` attribute must be present.
///
/// # Subject templates
/// Use `{self.field_name}` to interpolate struct fields into the subject.
/// Example: `#[event(subject = "orders.{self.order_id}.created")]`
///
/// # Optional attributes
/// - `aggregate = "order"` sets `aggregate_type()`, defaulting to `"default"`.
#[proc_macro_derive(Event, attributes(event))]
pub fn derive_event(input: TokenStream) -> TokenStream {
    event::derive_event_impl(input)
}
