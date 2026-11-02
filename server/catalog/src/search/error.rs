//! Search errors and their one mapping onto [`ApiError`].
//!
//! Client faults (400, 404) carry a message safe for users; a server fault
//! is the fixed internal error, its cause going to the log only.

use droppedneedle_kernel::error::ApiError;

/// Handler-layer search errors.
#[derive(Debug)]
pub enum SearchError {
    /// Bad query text, limit, or bucket filter.
    InvalidQuery {
        /// User-safe reason.
        message: String,
    },
    /// Unknown drill-down bucket.
    UnknownBucket,
}

impl From<SearchError> for ApiError {
    fn from(error: SearchError) -> Self {
        match error {
            SearchError::InvalidQuery { message } => ApiError::invalid_input(message),
            SearchError::UnknownBucket => ApiError::not_found(),
        }
    }
}
