//! Request and error identifiers.
//!
//! Every response carries an `x-request-id` header: the caller-supplied value
//! echoed back, or a server-generated id. Server faults reuse that same id as
//! the `error_id` in the 5xx envelope, so one value ties the wire response to
//! the structured server log. Generation sits behind a trait so tests inject
//! a fixed id and assert exact envelopes.

use uuid::Uuid;

/// Request id header, accepted on requests and always set on responses.
pub const REQUEST_ID_HEADER: &str = "x-request-id";

/// A request id resolved for one HTTP exchange.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestId(pub String);

/// Source of fresh identifiers. Implemented by the production generator and
/// by test fakes.
pub trait IdGenerator: Send + Sync {
    /// Mint a fresh identifier.
    fn new_id(&self) -> String;
}

/// Production generator: random v4 UUIDs.
#[derive(Debug, Default, Clone, Copy)]
pub struct UuidGenerator;

impl IdGenerator for UuidGenerator {
    fn new_id(&self) -> String {
        Uuid::new_v4().to_string()
    }
}
