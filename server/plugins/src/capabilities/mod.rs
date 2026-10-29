//! Capability adapters: typed calls from the server into plugins.
//!
//! Each module turns one capability into plain Rust: the fan-out or
//! lookup, its time budget, and the safe fallback when a plugin fails.
//! Everything goes through [`call`], which logs failures against the
//! plugin so callers only see the fallback.

pub mod acquisition;
pub mod events;
pub mod library_files;
pub mod metadata;
pub mod purchase;
pub mod routes;
pub mod stream;

use std::time::Duration;

use serde_json::Value;

use super::host::LoadedPlugin;
use super::runtime::CallError;

/// What the plugins serving one capability gave, and whether every one of
/// them answered. A plugin that failed, ran late or answered in the wrong
/// shape adds nothing and makes the answer incomplete, so a caller shows
/// what came back but never keeps it as the whole answer. A plugin that
/// does not implement the method answers nothing, which is complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Gathered<T> {
    /// What the plugins that answered gave.
    pub value: T,
    /// False when any plugin failed to answer.
    pub complete: bool,
}

/// Call one method on one plugin, logging any failure with the plugin's
/// name and the method. Disabled plugins answer `NotRunning`.
pub async fn call(
    plugin: &LoadedPlugin,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Value, CallError> {
    let Some(runtime) = plugin.runtime.as_ref() else {
        return Err(CallError::NotRunning("plugin is disabled".to_owned()));
    };
    let outcome = runtime.call(method, params, timeout).await;
    if let Err(error) = &outcome {
        tracing::warn!(
            plugin = %plugin.manifest.name,
            %method,
            %error,
            "plugin call failed"
        );
    }
    outcome
}

/// Call one method and decode its answer. `Ok(None)` when the plugin does
/// not implement the method; any other failure is an error.
async fn answer<T: serde::de::DeserializeOwned>(
    plugin: &LoadedPlugin,
    method: &str,
    params: Value,
    timeout: Duration,
) -> Result<Option<T>, CallError> {
    match call(plugin, method, params, timeout).await {
        Ok(value) => decode(plugin, method, value).map(Some),
        Err(CallError::Unsupported(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Decode one answer, mapping a shape mismatch to [`CallError::Malformed`].
pub fn decode<T: serde::de::DeserializeOwned>(
    plugin: &LoadedPlugin,
    method: &str,
    value: Value,
) -> Result<T, CallError> {
    serde_json::from_value(value).map_err(|error| {
        tracing::warn!(
            plugin = %plugin.manifest.name,
            %method,
            %error,
            "plugin answer has the wrong shape"
        );
        CallError::Malformed(error.to_string())
    })
}
