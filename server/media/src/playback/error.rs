//! Playback-reporting failures and their one mapping onto [`ApiError`].
//!
//! Services return domain failures that handlers convert; the `From` impl
//! below is the only place a status is chosen. Reporting itself almost
//! never errors: a failed forward is logged and swallowed (v2 never fails
//! the player for a report), so these errors cover only unknown tracks
//! and malformed bodies.

use droppedneedle_kernel::error::ApiError;

/// Every failure the playback routes can return to a caller.
#[derive(Debug)]
pub enum PlaybackError {
    /// Unknown track id. Always the same fixed message.
    NotFound,
    /// Bad input. The message is shown to the user.
    InvalidInput {
        /// What was wrong.
        message: String,
    },
}

impl From<PlaybackError> for ApiError {
    fn from(error: PlaybackError) -> Self {
        match error {
            PlaybackError::NotFound => ApiError::not_found(),
            PlaybackError::InvalidInput { message } => ApiError::invalid_input(message),
        }
    }
}
