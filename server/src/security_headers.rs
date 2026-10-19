//! The response security headers this server adds.
//!
//! Two layers. [`content_security`] gives the web UI's page its
//! content-security policy and every other native response a policy under
//! which nothing runs, plus `nosniff`. A JSON answer under `/api/v3` with no
//! `Cache-Control` of its own gets `no-store`, so per-user data is not kept by
//! a proxy or the browser. [`strict_transport_security`] wraps everything, the
//! compat APIs included, and adds the HSTS header.
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
use sha2::Digest as _;

use crate::state::AppState;
use droppedneedle_auth::session::middleware::arrived_over_https;
use droppedneedle_compat::shared::path_case::is_compat_path;
use droppedneedle_kernel::{
    config::TrustedProxies,
    runtime_config::{
        Live, Plain,
        sections::{ContentSecurityPolicyMode, HSTS_MAX_AGE_LIMIT, SecuritySettings},
    },
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

/// The header the page's policy goes under in each mode. `None` when the
/// admin turned the policy off.
fn shell_policy_header(mode: ContentSecurityPolicyMode) -> Option<HeaderName> {
    match mode {
        ContentSecurityPolicyMode::Enforce => Some(header::CONTENT_SECURITY_POLICY),
        ContentSecurityPolicyMode::ReportOnly => Some(header::CONTENT_SECURITY_POLICY_REPORT_ONLY),
        ContentSecurityPolicyMode::Off => None,
    }
}

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
/// Style attributes, checked 2026-10-17: the built bundle holds one static
/// `style` attribute, SvelteKit's route announcer ([`ANNOUNCER_STYLE`]),
/// and no `<style>` built at run time. A Chromium walk of the pages on a
/// scratch server (no outside network, the test fixture library) refused
/// only the announcer. What it could not reach (Discover content, outside
/// art, YouTube, previews, remote media servers, the plugin panel, dialogs
/// and drawers) is walked on the dev instance. Outside pictures seen on an
/// earlier walk (TheAudioDB's `r2.theaudiodb.com` on artist pages) are
/// covered by `img-src` allowing any web address.
const SHELL_DIRECTIVES: &[&str] = &[
    // Whatever is not named below is refused, so a new kind of load has to
    // be added here on purpose.
    "default-src 'none'",
    // No handler attributes, whatever `script-src` becomes.
    "script-src-attr 'none'",
    // Stylesheets from the app's own files. The page builds no `<style>`.
    // `'report-sample'` allows nothing. It makes the browser quote the
    // start of a refused style, which is how a style attribute in a bundle
    // is traced to its component. The app sets its own inline styles
    // through the style object, which this policy does not govern; a
    // `style` attribute written into markup is refused, except the
    // announcer's. A browser that does not know `style-src-attr` falls back
    // to `style-src` and shows the announcer unstyled: the page title as
    // text below the app after each navigation.
    "style-src 'self'",
    "style-src-elem 'self' 'report-sample'",
    ANNOUNCER_STYLE,
    // The app's pictures, inline placeholders and upload previews, and
    // pictures from any web address: artist and album art, chart covers,
    // account pictures from Plex, Jellyfin (often plain http on a LAN) and
    // single sign-on, plugin images. A picture cannot run code, and text
    // from outside renders no images.
    "img-src 'self' data: blob: https: http:",
    // The app's streams, the silent `data:` unlock clip, the preview hosts.
    "media-src 'self' data: https://*.dzcdn.net https://audio-ssl.itunes.apple.com",
    "font-src 'self'",
    // Every request the app makes goes to its own address.
    "connect-src 'self'",
    FRAME_SRC,
    // No worker, worklet or plugin content exists.
    "worker-src 'none'",
    "manifest-src 'self'",
    "object-src 'none'",
    // No `<base>` is used; an injected one would repoint relative addresses.
    "base-uri 'none'",
    "form-action 'self' https://musicbrainz.org",
];

/// The one style attribute the page holds: SvelteKit's route announcer,
/// which hides itself with `style="position: absolute; left: 0; top: 0;
/// clip: rect(0 0 0 0); clip-path: inset(50%); overflow: hidden;
/// white-space: nowrap; width: 1px; height: 1px"` (`write_root.js` in
/// SvelteKit 2.70.3), allowed by the hash of that text. If SvelteKit changes
/// the text, the browser journeys fail on the refusal and this hash is
/// updated.
const ANNOUNCER_STYLE: &str = "style-src-attr 'report-sample' 'unsafe-hashes' 'sha256-S8qMpvofolR8Mpjy4kQvEm7m1q8clzU4dfDH0AmvZjo='";

/// The scripts the page may run: the app's own files, its inline scripts by
/// hash, and YouTube's player by path. Never `unsafe-inline`, `unsafe-eval`,
/// `strict-dynamic` or a scheme: this directive is what stops injected
/// markup from running code. `'report-sample'` allows nothing: it makes the
/// browser quote the start of a refused inline script in its report.
const SCRIPT_SOURCES: &str = "'self' 'report-sample' https://www.youtube.com/iframe_api \
     https://www.youtube.com/s/player/";

/// The frames the page may hold: the plugin panel page and YouTube embeds.
/// It also holds a frame to those addresses when the frame's own script
/// navigates it.
const FRAME_SRC: &str = "frame-src 'self' https://www.youtube.com";

/// Who may frame the page: nobody, but the addresses an admin listed.
fn frame_ancestors(framing: &[String]) -> String {
    if framing.is_empty() {
        "frame-ancestors 'none'".to_owned()
    } else {
        format!("frame-ancestors {}", framing.join(" "))
    }
}

/// What is enforced about frames while the page's policy only reports.
/// Report-only is there so a mismatched boot script cannot blank the page;
/// framing has no such risk, so who may frame the page and where its frames
/// may go are enforced from the start.
fn framing_policy(framing: &[String]) -> String {
    format!("{FRAME_SRC}; {}", frame_ancestors(framing))
}

/// The page's policy for these inline-script hashes and these framing
/// sources (see [`SecuritySettings::framing_sources`]).
fn shell_policy(script_hashes: &[String], framing: &[String]) -> String {
    let mut policy = SHELL_DIRECTIVES.join("; ");
    policy.push_str("; script-src ");
    policy.push_str(SCRIPT_SOURCES);
    for hash in script_hashes {
        policy.push_str(" '");
        policy.push_str(hash);
        policy.push('\'');
    }
    policy.push_str("; ");
    policy.push_str(&frame_ancestors(framing));
    policy
}

/// What the content-security layer needs: the live settings.
#[derive(Debug, Clone)]
pub struct ContentSecurity {
    settings: Live<SecuritySettings>,
}

impl ContentSecurity {
    /// Follow the saved security settings.
    pub fn new(settings: Live<SecuritySettings>) -> Self {
        Self { settings }
    }

    /// Follow the settings the rest of the server reads.
    pub fn from_state(state: &AppState) -> Self {
        Self::new(state.auth.security.clone())
    }

    /// The saved settings. Unreadable ones give the default mode and no
    /// framing, never a missing policy.
    fn settings(&self) -> Arc<Plain<SecuritySettings>> {
        self.settings.get().unwrap_or_else(|error| {
            tracing::debug!(%error, "security settings unreadable; default content-security policy");
            Arc::default()
        })
    }
}

/// A short mark for how these settings send the page's policy, added to the
/// page's `ETag`.
///
/// A browser that revalidates a cached page and gets a 304 keeps every
/// header of the cached answer that the 304 does not carry. Without the
/// mark, a page cached while the policy was enforced would stay enforced
/// after an admin chose report-only or off, and would keep an old framing
/// list. With it, a page cached under other settings no longer matches and
/// is sent again in full.
fn policy_mark(settings: &SecuritySettings) -> String {
    let mode = match settings.content_security_policy {
        ContentSecurityPolicyMode::Enforce => "enforce",
        ContentSecurityPolicyMode::ReportOnly => "report",
        ContentSecurityPolicyMode::Off => "off",
    };
    let digest = sha2::Sha256::digest(format!("{mode} {}", settings.framing_sources().join(" ")));
    let short: String = digest
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("-csp{short}")
}

/// Take the current mark off the entity tags a browser sent back, so the
/// file server compares the tag it made. A tag with another mark, from other
/// settings, is left as it is and matches nothing.
fn unmark_if_none_match(request: &mut Request, mark: &str) {
    let Some(sent) = request
        .headers()
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
    else {
        return;
    };
    let unmarked = sent.replace(&format!("{mark}\""), "\"");
    if let Ok(value) = HeaderValue::from_str(&unmarked) {
        request.headers_mut().insert(header::IF_NONE_MATCH, value);
    }
}

/// Add the mark to the page's entity tag, inside the quotes.
fn mark_etag(headers: &mut axum::http::HeaderMap, mark: &str) {
    let marked = headers
        .get(header::ETAG)
        .and_then(|value| value.to_str().ok())
        .and_then(|tag| tag.strip_suffix('"'))
        .and_then(|open| HeaderValue::from_str(&format!("{open}{mark}\"")).ok());
    if let Some(value) = marked {
        headers.insert(header::ETAG, value);
    }
}

/// Add `X-Content-Type-Options: nosniff` and a content-security policy to a
/// native response: the page's policy on the web UI's page, with
/// `Referrer-Policy`, and [`BARE_POLICY`] on everything else. A JSON answer
/// under `/api/v3` that has no `Cache-Control` gets `no-store`. A compat
/// response is left exactly as it is, and so is a policy or cache rule a
/// route set.
///
/// The Security settings choose how the page's policy is sent and who may
/// frame the page. They are read once per request, before the handler, so
/// the policy sent and the mark on the page's `ETag` always agree; a change
/// applies from the next request. In report-only mode the framing rules
/// are still enforced ([`framing_policy`]). Turned off, no policy is added
/// anywhere and `nosniff` stays.
///
/// Of the request, only its path and `If-None-Match` are read. The path picks
/// the compat and `/api/v3` rules, and `If-None-Match` is only compared (see
/// [`policy_mark`]). Nothing a request sends reaches a policy.
pub async fn content_security(
    State(security): State<ContentSecurity>,
    mut request: Request,
    next: Next,
) -> Response {
    if is_compat_path(request.uri().path()) {
        return next.run(request).await;
    }
    let api_path = request.uri().path().starts_with("/api/v3/");
    let settings = security.settings();
    let mark = policy_mark(&settings);
    if request.headers().contains_key(header::IF_NONE_MATCH) {
        unmark_if_none_match(&mut request, &mark);
    }
    let mut response = next.run(request).await;
    let shell = response.extensions().get::<AppShell>().cloned();
    let headers = response.headers_mut();
    if shell.is_some() {
        mark_etag(headers, &mark);
    }
    let json = headers
        .get(header::CONTENT_TYPE)
        .is_some_and(|value| value.as_bytes().starts_with(b"application/json"));
    if api_path && json && !headers.contains_key(header::CACHE_CONTROL) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
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
    let Some(shell_header) = shell_policy_header(settings.content_security_policy) else {
        return response;
    };
    if headers.contains_key(header::CONTENT_SECURITY_POLICY) {
        return response;
    }
    match shell {
        Some(shell) => {
            let framing = settings.framing_sources();
            let mut policies = vec![(shell_header, shell_policy(&shell.script_hashes, &framing))];
            if settings.content_security_policy == ContentSecurityPolicyMode::ReportOnly {
                policies.push((header::CONTENT_SECURITY_POLICY, framing_policy(&framing)));
            }
            for (name, policy) in policies {
                match HeaderValue::from_str(&policy) {
                    Ok(value) => {
                        headers.insert(name, value);
                    }
                    Err(error) => {
                        tracing::error!(%error, "the page's policy is not a header value");
                    }
                }
            }
        }
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
    settings: Live<SecuritySettings>,
    trusted_proxies: TrustedProxies,
}

impl TransportSecurity {
    /// Read the settings and the trusted proxies the session gate uses.
    pub fn from_state(state: &AppState) -> Self {
        Self {
            settings: state.auth.security.clone(),
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
    match security.settings.get() {
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
