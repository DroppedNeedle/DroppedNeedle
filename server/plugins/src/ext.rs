//! The guarded gateway to a plugin's own HTTP routes under `/ext/`.
//!
//! One call: match a declared route, check its auth and pacing, cap the
//! body, hand it to the plugin and pass its answer back.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use axum::{
    Json,
    extract::Request,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};

use droppedneedle_auth::{Admin, Principal};

use super::capabilities::routes::ROUTE_BODY_MAX_BYTES;
use super::error::PluginError;
use super::host::PluginHost;
use super::manifest::{Capability, RouteAuth};
use super::runtime::PluginRouteBody;

/// One pacing bucket: (plugin, caller, method, subpath).
type ExtRateKey = (String, String, String, String);

/// Per-route request pacing: one sliding minute window per key, capped so
/// the map cannot grow without bound.
#[derive(Debug, Default)]
pub struct ExtRateLimiter {
    hits: Mutex<HashMap<ExtRateKey, Vec<Instant>>>,
}

impl ExtRateLimiter {
    /// Empty limiter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a call; `None` when allowed, else Retry-After seconds.
    pub fn check(
        &self,
        plugin: &str,
        principal: &str,
        method: &str,
        subpath: &str,
        limit_per_minute: i64,
    ) -> Option<u64> {
        const WINDOW: Duration = Duration::from_secs(60);
        const CAP: usize = 10_000;
        let now = Instant::now();
        let mut guard = self.hits.lock().ok()?;
        let key = (
            plugin.to_owned(),
            principal.to_owned(),
            method.to_owned(),
            subpath.to_owned(),
        );
        if !guard.contains_key(&key) && guard.len() >= CAP {
            guard.retain(|_, hits| hits.iter().any(|hit| now.duration_since(*hit) < WINDOW));
            while guard.len() >= CAP {
                if let Some(first) = guard.keys().next().cloned() {
                    guard.remove(&first);
                } else {
                    break;
                }
            }
        }
        let hits = guard.entry(key).or_default();
        hits.retain(|hit| now.duration_since(*hit) < WINDOW);
        let limit = limit_per_minute.max(1) as usize;
        if hits.len() >= limit {
            let retry_after = hits
                .first()
                .and_then(|oldest| oldest.checked_add(WINDOW))
                .and_then(|reset| reset.checked_duration_since(now))
                .map(|wait| wait.as_secs() + 1)
                .unwrap_or(1)
                .max(1);
            return Some(retry_after);
        }
        hits.push(now);
        None
    }
}

/// Serve one call to a plugin's declared route.
pub async fn serve(
    host: &PluginHost,
    limiter: &ExtRateLimiter,
    caller: &Principal,
    plugin_name: &str,
    subpath: &str,
    method: &str,
    request: Request,
) -> Result<Response, PluginError> {
    if plugin_name.is_empty()
        || subpath.is_empty()
        || subpath.starts_with('/')
        || subpath.split('/').any(|part| part == "..")
    {
        return Err(PluginError::InvalidInput {
            message: "Invalid plugin route".to_owned(),
        });
    }
    let plugin = match host.get(plugin_name) {
        Some(plugin) if plugin.serves(Capability::Publisher) => plugin,
        _ => return Err(PluginError::NotFound),
    };
    let spec = plugin
        .manifest
        .routes
        .iter()
        .find(|route| route.path == subpath && route.method.as_str() == method);
    let Some(spec) = spec else {
        return Err(PluginError::NotFound);
    };
    if spec.auth == RouteAuth::Admin {
        Admin::try_from(caller.clone()).map_err(|refused| PluginError::Forbidden {
            message: refused.to_string(),
        })?;
    }
    if let Some(retry_after) = limiter.check(
        plugin_name,
        caller.user_id(),
        method,
        subpath,
        spec.rate_limit_per_minute,
    ) {
        return Err(PluginError::RateLimited { retry_after });
    }
    let (parts, body) = request.into_parts();
    if let Some(declared) = parts
        .headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.trim().parse::<usize>().ok())
        && declared > ROUTE_BODY_MAX_BYTES
    {
        return Err(PluginError::PayloadTooLarge);
    }
    let raw = axum::body::to_bytes(body, ROUTE_BODY_MAX_BYTES + 1)
        .await
        .map_err(|_| PluginError::PayloadTooLarge)?;
    let route_body = if raw.is_empty() {
        PluginRouteBody::Empty
    } else if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&raw) {
        PluginRouteBody::Json(json)
    } else if let Ok(text) = std::str::from_utf8(&raw) {
        PluginRouteBody::Text(text.to_owned())
    } else {
        PluginRouteBody::Empty
    };
    let mut query = HashMap::new();
    if let Some(raw_query) = parts.uri.query() {
        for pair in raw_query.split('&') {
            if pair.is_empty() {
                continue;
            }
            let (key, value) = match pair.split_once('=') {
                Some((key, value)) => (key, value),
                None => (pair, ""),
            };
            query.insert(
                percent_decode(key).unwrap_or_else(|| key.to_owned()),
                percent_decode(value).unwrap_or_else(|| value.to_owned()),
            );
        }
    }
    let result = host
        .handle_plugin_route(plugin_name, method, subpath, &query, &route_body)
        .await;
    let status =
        StatusCode::from_u16(result.status.max(100) as u16).unwrap_or(StatusCode::BAD_GATEWAY);
    if status == StatusCode::BAD_GATEWAY {
        return Err(PluginError::RouteFailed);
    }
    if status == StatusCode::NOT_FOUND {
        return Err(PluginError::NotFound);
    }
    Ok((status, Json(result.body)).into_response())
}

/// Decode one percent-encoded query part. Malformed escapes fail the part
/// (the caller keeps the raw text) instead of failing the request.
fn percent_decode(part: &str) -> Option<String> {
    let bytes = part.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok()?;
            let byte = u8::from_str_radix(hex, 16).ok()?;
            out.push(byte);
            index += 3;
            continue;
        }
        if bytes[index] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[index]);
        }
        index += 1;
    }
    String::from_utf8(out).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_decoding_tolerates_bad_escapes() {
        assert_eq!(percent_decode("a+b"), Some("a b".to_owned()));
        assert_eq!(percent_decode("a%20b"), Some("a b".to_owned()));
        assert_eq!(percent_decode("a%zz"), None);
        assert_eq!(percent_decode("a%2"), None);
    }

    #[test]
    fn limiter_opens_after_the_first_call_over_budget() {
        let limiter = ExtRateLimiter::new();
        assert_eq!(limiter.check("p", "u", "GET", "s", 1), None);
        assert!(limiter.check("p", "u", "GET", "s", 1).is_some());
        assert_eq!(limiter.check("p", "u", "GET", "other", 1), None);
    }
}
