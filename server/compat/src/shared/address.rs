//! The address player apps are told this server is at.
//!
//! Every answer that hands an app an address builds it from
//! [`base_address`], so both protocols agree with each other and with the
//! session origin check on where the server is.

use axum::body::Body;
use axum::http::Request;

use crate::settings::LiveSettings;
use droppedneedle_auth::session::middleware::effective_origin;
use droppedneedle_kernel::config::TrustedProxies;

/// How clients reach this server: the port it listens on, the proxies
/// whose forwarded headers are believed and the deployment base path.
#[derive(Debug, Clone)]
pub struct ListenerConfig {
    /// TCP port the app listens on, reported as `WebSocketPortNumber`.
    pub port: u16,
    /// Proxies whose `X-Forwarded-Host` and `X-Forwarded-Proto` are believed.
    pub trusted_proxies: TrustedProxies,
    /// Deployment base path (`BASE_PATH`): `""` or `/seg[/seg...]`.
    pub base_path: String,
}

/// The address `request` reached this server at, base path included and
/// with no trailing slash, such as `https://music.example.com/music`.
/// `None` when the request names no host.
///
/// The origin is the session check's own: the host the app used, with the
/// forwarded host and scheme believed only from a trusted proxy. A plain
/// `http` origin that is not itself a stated public address becomes the
/// one stated `https` address on the same host name, when there is exactly
/// one: that is a proxy that ends TLS without saying so. The list is used
/// for nothing else, so an app on the LAN keeps its LAN address.
pub fn base_address(
    request: &Request<Body>,
    listener: &ListenerConfig,
    settings: &LiveSettings,
) -> Option<String> {
    let mut origin = effective_origin(request, &listener.trusted_proxies)?;
    if !origin.is_https() {
        let stated = settings.public_addresses();
        if !stated.contains(&origin) {
            let mut secure = stated
                .iter()
                .filter(|address| address.is_https() && address.same_host(&origin));
            if let (Some(only), None) = (secure.next(), secure.next()) {
                origin = only.clone();
            }
        }
    }
    Some(format!("{}{}", origin.serialized(), listener.base_path))
}
