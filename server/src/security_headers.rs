//! The response security headers this server adds.
//!
//! The HSTS layer wraps the request scope and adds one response header, see
//! [`strict_transport_security`].

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    middleware::Next,
    response::Response,
};

use crate::{
    auth::session::middleware::{TrustedProxies, arrived_over_https},
    runtime_config::{
        ConfigStore,
        sections::{HSTS_MAX_AGE_LIMIT, SecuritySettings},
    },
    state::AppState,
};

/// What the HSTS layer needs: the live settings and the proxies whose
/// forwarded scheme counts.
#[derive(Debug, Clone)]
pub struct TransportSecurity {
    config: Arc<ConfigStore>,
    trusted_proxies: TrustedProxies,
}

impl TransportSecurity {
    /// Read the settings and the trusted proxies the session gate uses.
    pub fn from_state(state: &AppState) -> Self {
        Self {
            config: state.auth.config_store.clone(),
            trusted_proxies: state.auth.session_auth.trusted_proxies.clone(),
        }
    }
}

/// Add `Strict-Transport-Security` to the response when the Security
/// settings ask for it and the request arrived over HTTPS.
///
/// A browser that has seen the header refuses plain HTTP to this host for
/// `max-age` seconds, so it is never sent on a plain-HTTP request, on a
/// forwarded scheme from a peer that is not a trusted proxy, or when the
/// settings cannot be read. The settings are read after the handler ran, so
/// the response to the save that turns HSTS off already goes without it.
pub async fn strict_transport_security(
    State(security): State<TransportSecurity>,
    request: Request,
    next: Next,
) -> Response {
    let https = arrived_over_https(&request, &security.trusted_proxies);
    let mut response = next.run(request).await;
    if !https {
        return response;
    }
    match security.config.get::<SecuritySettings>() {
        Ok(settings) => {
            if let Some(value) = hsts_value(&settings) {
                response
                    .headers_mut()
                    .insert(axum::http::header::STRICT_TRANSPORT_SECURITY, value);
            }
        }
        Err(error) => tracing::debug!(%error, "security settings unreadable; no HSTS header"),
    }
    response
}

/// The header value for these settings, the v2 format. `None` when HSTS is
/// off: a max-age of zero or less. Saves refuse a max-age outside
/// `0..=HSTS_MAX_AGE_LIMIT`, but a config file written before that check,
/// or by hand, can still hold one: a negative reads as off and anything
/// above the limit is sent as the limit.
fn hsts_value(settings: &SecuritySettings) -> Option<axum::http::HeaderValue> {
    if settings.hsts_max_age <= 0 {
        return None;
    }
    let max_age = settings.hsts_max_age.min(HSTS_MAX_AGE_LIMIT);
    let mut value = format!("max-age={max_age}");
    if settings.hsts_include_subdomains {
        value.push_str("; includeSubDomains");
    }
    if settings.hsts_preload {
        value.push_str("; preload");
    }
    axum::http::HeaderValue::from_str(&value).ok()
}

#[cfg(test)]
mod tests {
    use super::{HSTS_MAX_AGE_LIMIT, SecuritySettings, hsts_value};

    #[test]
    fn stored_max_age_out_of_range_is_off_or_capped() {
        let stored = |hsts_max_age| SecuritySettings {
            hsts_max_age,
            ..SecuritySettings::default()
        };
        assert_eq!(hsts_value(&stored(-1)), None);
        assert_eq!(
            hsts_value(&stored(i64::MAX)).unwrap(),
            format!("max-age={HSTS_MAX_AGE_LIMIT}").as_str()
        );
    }
}
