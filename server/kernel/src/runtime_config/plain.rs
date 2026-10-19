//! [`Plain`]: a section as the server uses it, secrets decrypted.
//!
//! A section has three forms. [`Masked`](super::Masked) crosses the API,
//! the stored form is what the store writes, and `Plain` is what the
//! server reads. `Plain` is not `Serialize`, so a decrypted section cannot
//! reach a response, a log line in JSON or a file:
//!
//! ```compile_fail
//! # use axum::response::IntoResponse;
//! # use droppedneedle_kernel::runtime_config::{ConfigStore, secret_sections::SlskdConnection};
//! fn leak(store: &ConfigStore) -> axum::response::Response {
//!     axum::Json(store.get_raw::<SlskdConnection>().unwrap()).into_response()
//! }
//! ```
//!
//! The masked form builds:
//!
//! ```
//! # use axum::response::IntoResponse;
//! # use droppedneedle_kernel::runtime_config::{ConfigStore, secret_sections::SlskdConnection};
//! fn show(store: &ConfigStore) -> axum::response::Response {
//!     axum::Json(store.get_masked::<SlskdConnection>().unwrap()).into_response()
//! }
//! ```
//!
//! A reference taken through the deref still holds secrets that refuse to
//! serialize ([`Secret`](super::Secret)).

/// A section (or an indexer, or a plugin's values) with its secrets
/// decrypted. Only the store makes one.
#[derive(Clone)]
pub struct Plain<T>(T);

impl<T> Plain<T> {
    /// Wrap a value the store just opened.
    pub(super) fn opened(value: T) -> Self {
        Self(value)
    }
}

/// The default holds no secret, so a reader that falls back to it builds
/// one itself.
impl<T: Default> Default for Plain<T> {
    fn default() -> Self {
        Self(T::default())
    }
}

impl<T> std::ops::Deref for Plain<T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.0
    }
}

/// One plugin's values with its secrets decrypted. Its values are plain
/// strings, which carry no refusal of their own, so it has no other way
/// out than [`PluginValues::into_settings`]: no `Deref`, no `Serialize`.
pub struct PluginValues(std::collections::HashMap<String, String>);

impl PluginValues {
    /// Wrap values the store just opened.
    pub(super) fn opened(values: std::collections::HashMap<String, String>) -> Self {
        Self(values)
    }

    /// The values, for the plugin host to hand to the plugin process.
    #[must_use]
    pub fn into_settings(self) -> std::collections::HashMap<String, String> {
        self.0
    }
}

/// Shows the keys only.
impl std::fmt::Debug for PluginValues {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_set().entries(self.0.keys()).finish()
    }
}

impl<T: std::fmt::Debug> std::fmt::Debug for Plain<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}
