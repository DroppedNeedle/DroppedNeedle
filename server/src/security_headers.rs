//! The response security headers this server adds.
//!
//! Two layers. [`content_security`] gives the web UI's page its
//! content-security policy and every other native response a policy under
//! which nothing runs, plus `nosniff`. [`strict_transport_security`] wraps
//! everything, the compat APIs included, and adds the HSTS header.
//!
//! A route sets a policy of its own only to be stricter (the remote image
//! relay, the plugin panel page), and the layer never replaces one. The
//! compat APIs (`/subsonic`, `/jellyfin`) get nothing from
//! [`content_security`]: their answers are compared with recorded real
//! servers and their callers are apps, not pages.
//!
//! Nothing a request sends takes part in a policy. Its text comes from this
//! file and from the hashes of the page's inline scripts, worked out at
//! boot in `web.rs`.

use std::sync::Arc;

use axum::{
    extract::{Request, State},
    http::{HeaderName, HeaderValue, header},
    middleware::Next,
    response::Response,
};

use crate::{
    auth::session::middleware::{TrustedProxies, arrived_over_https},
    compat::shared::path_case::is_compat_path,
    runtime_config::{
        ConfigStore,
        sections::{HSTS_MAX_AGE_LIMIT, SecuritySettings},
    },
    state::AppState,
};

/// Put on a response by `web.rs` when its body is the web UI's page
/// (`index.html`), with the hashes of the inline scripts in that page. The
/// layer gives such a response the page's policy.
#[derive(Debug, Clone)]
pub struct AppShell {
    /// One `sha256-<base64>` per inline script, as a policy names it.
    pub script_hashes: Arc<[String]>,
}

/// The policy for every native response that is not the web UI's page: the
/// API, covers, streams, avatars, static files, `/health`, the 404 envelope.
/// On an image, a script or a fetch result a policy does nothing. It counts
/// when such an address is opened as a page of its own: then nothing in it
/// loads or runs (`default-src 'none'`, `sandbox`) and nothing may frame it.
const BARE_POLICY: &str = "default-src 'none'; frame-ancestors 'none'; sandbox";

/// The referrer value sent with the page. It is the browser default written
/// down: other sites get the origin only, the app's own requests the full
/// address, which the session origin check falls back on.
const REFERRER_POLICY: &str = "strict-origin-when-cross-origin";

/// The header the page's policy goes under: the browser reports what the
/// policy would refuse and refuses nothing.
const SHELL_POLICY_HEADER: HeaderName = header::CONTENT_SECURITY_POLICY_REPORT_ONLY;

/// The page's policy, one directive each, without `script-src` and
/// `frame-ancestors`, which [`shell_policy`] adds.
///
/// Outside hosts, each checked read-only on 2026-10-13:
/// - `https://www.youtube.com/iframe_api` is the player loader the app adds.
///   It loads `https://www.youtube.com/s/player/<id>/www-widgetapi.vflset/
///   www-widgetapi.js`. Both are listed by path so nothing else on that host
///   runs. A source path ending in `/` matches everything under it.
/// - `https://www.youtube.com/embed/<id>` is the only outside frame.
/// - 30-second previews play from `cdnt-preview.dzcdn.net` (Deezer, whose
///   host prefix changes) and `audio-ssl.itunes.apple.com`.
/// - `https://musicbrainz.org/release/add` takes the one form that leaves
///   the app, the release editor seed.
///
/// A `style-src-attr` that allows no style attribute and an `img-src` with
/// no outside host are stricter than the page can live with. This is the candidate sent while
/// the policy only reports, so that a browser lists every style attribute
/// and every outside picture host.
const SHELL_DIRECTIVES: &[&str] = &[
    // Whatever is not named below is refused, so a new kind of load has to
    // be added here on purpose.
    "default-src 'none'",
    // No handler attributes, whatever `script-src` becomes.
    "script-src-attr 'none'",
    // Stylesheets from the app's own files. The page builds no `<style>`.
    // `'report-sample'` allows nothing. It makes the browser quote the
    // start of the refused style in its report, which is how a style
    // attribute in a bundle is traced to its component.
    "style-src 'self'",
    "style-src-elem 'self' 'report-sample'",
    "style-src-attr 'report-sample'",
    // The app's pictures, inline placeholders and upload previews.
    "img-src 'self' data: blob:",
    // The app's streams, the silent `data:` unlock clip, the preview hosts.
    "media-src 'self' data: https://*.dzcdn.net https://audio-ssl.itunes.apple.com",
    "font-src 'self'",
    // Every request the app makes goes to its own address.
    "connect-src 'self'",
    // The plugin panel page and YouTube embeds. It also holds a frame to
    // those addresses when the frame's own script navigates it.
    "frame-src 'self' https://www.youtube.com",
    // No worker, worklet or plugin content exists.
    "worker-src 'none'",
    "manifest-src 'self'",
    "object-src 'none'",
    // No `<base>` is used; an injected one would repoint relative addresses.
    "base-uri 'none'",
    "form-action 'self' https://musicbrainz.org",
];

/// The scripts the page may run: the app's own files, its inline scripts by
/// hash, and YouTube's player by path. Never `unsafe-inline`, `unsafe-eval`,
/// `strict-dynamic` or a scheme: this directive is what stops injected
/// markup from running code. `'report-sample'` allows nothing: it makes the
/// browser quote the start of a refused inline script in its report.
const SCRIPT_SOURCES: &str = "'self' 'report-sample' https://www.youtube.com/iframe_api \
     https://www.youtube.com/s/player/";

/// The page's policy for these inline-script hashes.
fn shell_policy(script_hashes: &[String]) -> String {
    let mut policy = SHELL_DIRECTIVES.join("; ");
    policy.push_str("; script-src ");
    policy.push_str(SCRIPT_SOURCES);
    for hash in script_hashes {
        policy.push_str(" '");
        policy.push_str(hash);
        policy.push('\'');
    }
    // Nobody may put the app in a frame.
    policy.push_str("; frame-ancestors 'none'");
    policy
}

/// Add `X-Content-Type-Options: nosniff` and a content-security policy to a
/// native response: the page's policy on the web UI's page, with
/// `Referrer-Policy`, and [`BARE_POLICY`] on everything else. A compat
/// response is left exactly as it is, and so is a policy a route set.
pub async fn content_security(request: Request, next: Next) -> Response {
    let compat = is_compat_path(request.uri().path());
    let mut response = next.run(request).await;
    if compat {
        return response;
    }
    let shell = response.extensions().get::<AppShell>().cloned();
    let headers = response.headers_mut();
    if !headers.contains_key(header::X_CONTENT_TYPE_OPTIONS) {
        headers.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
    }
    if shell.is_some() && !headers.contains_key(header::REFERRER_POLICY) {
        headers.insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static(REFERRER_POLICY),
        );
    }
    if headers.contains_key(header::CONTENT_SECURITY_POLICY) {
        return response;
    }
    match shell {
        Some(shell) => match HeaderValue::from_str(&shell_policy(&shell.script_hashes)) {
            Ok(value) => {
                headers.insert(SHELL_POLICY_HEADER, value);
            }
            Err(error) => tracing::error!(%error, "the page's policy is not a header value"),
        },
        None => {
            headers.insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(BARE_POLICY),
            );
        }
    }
    response
}

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
