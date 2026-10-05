//! `/ext/` routes: a `publisher` plugin answers the HTTP routes its
//! manifest declares under `/api/v3/plugins/ext/<name>/<path>`.
//!
//! The handler layer authenticates, rate-limits and caps the request body
//! first. Here: disabled plugins, undeclared paths and other methods are
//! 404 (no oracle); the plugin has 5 seconds; its status passes only when
//! it is 200-299, 400 or 404 (500-599 become 502, anything else 200, so a
//! plugin cannot redirect through the API's origin); a body over 1 MiB or
//! any failure is a fixed 502, never the plugin's error text.

use std::collections::HashMap;
use std::time::Duration;

use serde_json::{Value, json};

use super::super::host::PluginHost;
use super::super::protocol::methods;
use super::super::runtime::{PluginRouteBody, PluginRouteResponse};
use super::call;

/// Per-request budget.
const ROUTE_TIMEOUT: Duration = Duration::from_secs(5);
/// Largest `/ext/` answer body passed through (1 MiB).
pub const ROUTE_BODY_MAX_BYTES: usize = 1024 * 1024;
/// Methods a route may declare.
const ROUTE_METHODS: &[&str] = &["GET", "POST", "DELETE"];

fn failed() -> PluginRouteResponse {
    PluginRouteResponse {
        status: 502,
        body: json!({"error": {"code": "EXTERNAL_SERVICE_UNAVAILABLE", "message": "Plugin route failed", "details": null}}),
    }
}

fn not_found() -> PluginRouteResponse {
    PluginRouteResponse {
        status: 404,
        body: json!({"error": {"code": "NOT_FOUND", "message": "Not found", "details": null}}),
    }
}

/// Map a plugin-chosen status onto the fixed table.
pub fn clamp_status(status: i64) -> i32 {
    if (200..=299).contains(&status) || status == 400 || status == 404 {
        status as i32
    } else if (500..=599).contains(&status) {
        502
    } else {
        200
    }
}

impl PluginHost {
    /// Run one declared plugin route.
    pub async fn handle_plugin_route(
        &self,
        plugin_name: &str,
        method: &str,
        subpath: &str,
        query: &HashMap<String, String>,
        body: &PluginRouteBody,
    ) -> PluginRouteResponse {
        let Some(plugin) = self
            .get(plugin_name)
            .filter(|plugin| plugin.serves("publisher"))
        else {
            return not_found();
        };
        let verb = method.to_ascii_uppercase();
        if !ROUTE_METHODS.contains(&verb.as_str()) {
            return not_found();
        }
        let declared = plugin
            .manifest
            .routes
            .iter()
            .any(|route| route.path == subpath && route.method.eq_ignore_ascii_case(&verb));
        if !declared {
            return not_found();
        }
        let body = match body {
            PluginRouteBody::Empty => Value::Null,
            PluginRouteBody::Json(value) => value.clone(),
            PluginRouteBody::Text(text) => Value::String(text.clone()),
        };
        let params = json!({
            "method": verb,
            "subpath": subpath,
            "query": query,
            "body": body,
        });
        let Ok(answer) = call(&plugin, methods::ROUTE, params, ROUTE_TIMEOUT).await else {
            return failed();
        };
        let Some(status) = answer.get("status").and_then(Value::as_i64) else {
            tracing::warn!(plugin = %plugin_name, path = %subpath, "plugin route answer has no status");
            return failed();
        };
        let body = answer.get("body").cloned().unwrap_or(Value::Null);
        if serde_json::to_vec(&body)
            .map(|raw| raw.len())
            .unwrap_or(usize::MAX)
            > ROUTE_BODY_MAX_BYTES
        {
            tracing::warn!(plugin = %plugin_name, path = %subpath, "plugin route body over 1 MiB");
            return failed();
        }
        PluginRouteResponse {
            status: clamp_status(status),
            body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_clamp_to_the_fixed_table() {
        assert_eq!(clamp_status(201), 201);
        assert_eq!(clamp_status(400), 400);
        assert_eq!(clamp_status(404), 404);
        assert_eq!(clamp_status(302), 200);
        assert_eq!(clamp_status(403), 200);
        assert_eq!(clamp_status(500), 502);
    }
}
