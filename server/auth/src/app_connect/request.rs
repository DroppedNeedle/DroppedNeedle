//! Checking an app connect request, naming its app password, building the
//! addresses the browser hands back, and the PKCE check.

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use utoipa::{IntoParams, ToSchema};

use droppedneedle_kernel::runtime_config::sections::{AppConnect, AppConnectClient};

use crate::session::tokens::secret_eq;
use crate::users::services::MAX_APP_PASSWORD_NAME_LEN;

/// Longest `state`, in bytes.
pub const MAX_STATE_LEN: usize = 512;
/// Between the app's name and the device's in an app password's name.
pub const NAME_SEPARATOR: &str = " · ";
/// The only challenge method: SHA-256 (RFC 7636 section 4.2). `plain`
/// would hand the verifier to anyone who reads the request.
pub const CHALLENGE_METHOD: &str = "S256";
/// The only response type: the authorization code grant.
const RESPONSE_TYPE: &str = "code";
/// Length of a base64url SHA-256 without padding.
const CHALLENGE_LEN: usize = 43;

/// What the app asks for, as it put it in the `/connect` address. Every
/// field is checked here; nothing is trusted for being present.
#[derive(Debug, Clone, Default, Deserialize, ToSchema, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(default)]
pub struct ConnectRequest {
    /// Must be `code`.
    pub response_type: String,
    /// The app's id, one listed in the `app_connect` settings.
    pub client_id: String,
    /// Where the browser hands the result back; exactly one of the app's
    /// listed addresses.
    pub redirect_uri: String,
    /// `BASE64URL(SHA256(code_verifier))`, 43 characters, no padding.
    pub code_challenge: String,
    /// Must be `S256`.
    pub code_challenge_method: String,
    /// The app's own value, handed back unchanged; up to 512 bytes.
    pub state: String,
    /// The device name the app reports, such as `Pixel 9`. Optional.
    pub name: Option<String>,
}

/// Why a request cannot go on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// No app is listed: app connect is off.
    Disabled,
    /// The app or its return address is not listed. Nothing is handed
    /// back: the address is not one the app is known by.
    UnknownClient,
    /// The app is known but the request is malformed. The browser may hand
    /// `redirect` (an address carrying the OAuth error) back to the app.
    Invalid {
        /// What is wrong, for the page.
        reason: &'static str,
        /// The error address for the app.
        redirect: String,
    },
}

/// A request that passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checked {
    /// The listed app.
    pub client: AppConnectClient,
    /// Its return address, as listed.
    pub redirect_uri: String,
    /// The PKCE challenge.
    pub code_challenge: String,
    /// The app's state.
    pub state: String,
    /// The device name as it will be shown and stored, if one was given.
    pub device_name: Option<String>,
    /// The app password's name.
    pub password_name: String,
}

impl Checked {
    /// The address that tells the app the user cancelled.
    pub fn cancel_redirect(&self) -> String {
        error_redirect(&self.redirect_uri, "access_denied", Some(&self.state))
    }

    /// The address that hands the app its code.
    pub fn code_redirect(&self, code: &str) -> String {
        with_params(
            &self.redirect_uri,
            &[("code", code), ("state", &self.state)],
        )
    }
}

/// Check `request` against the listed apps.
pub fn check(apps: &AppConnect, request: &ConnectRequest) -> Result<Checked, Rejection> {
    if !apps.enabled() {
        return Err(Rejection::Disabled);
    }
    let Some(client) = apps.client(&request.client_id, &request.redirect_uri) else {
        return Err(Rejection::UnknownClient);
    };
    let state_ok = !request.state.is_empty()
        && request.state.len() <= MAX_STATE_LEN
        && !request.state.chars().any(char::is_control);
    let invalid = |error: &str, reason: &'static str| Rejection::Invalid {
        reason,
        redirect: error_redirect(
            &request.redirect_uri,
            error,
            state_ok.then_some(request.state.as_str()),
        ),
    };
    if !state_ok {
        return Err(invalid("invalid_request", "The app sent no usable state."));
    }
    if request.response_type != RESPONSE_TYPE {
        return Err(invalid(
            "unsupported_response_type",
            "The app must ask for a code.",
        ));
    }
    if request.code_challenge_method != CHALLENGE_METHOD {
        return Err(invalid(
            "invalid_request",
            "The app must use the S256 challenge method.",
        ));
    }
    if !is_challenge(&request.code_challenge) {
        return Err(invalid(
            "invalid_request",
            "The app sent a malformed code challenge.",
        ));
    }
    let device_name = request.name.as_deref().and_then(|name| {
        device_name(
            name,
            MAX_APP_PASSWORD_NAME_LEN
                .saturating_sub(client.name.chars().count() + NAME_SEPARATOR.chars().count()),
        )
    });
    let password_name = match &device_name {
        Some(device) => format!("{}{NAME_SEPARATOR}{device}", client.name),
        None => client.name.clone(),
    };
    Ok(Checked {
        client: client.clone(),
        redirect_uri: request.redirect_uri.clone(),
        code_challenge: request.code_challenge.clone(),
        state: request.state.clone(),
        device_name,
        password_name,
    })
}

/// 43 characters of the base64url alphabet: what a SHA-256 encodes to.
fn is_challenge(value: &str) -> bool {
    value.len() == CHALLENGE_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Characters that change how text around them is shown without being
/// seen: zero-width marks and the bidirectional overrides and isolates. A
/// device name with them could show the confirmation page something other
/// than what is stored.
fn is_invisible(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

/// The device name as shown and stored: control and invisible characters
/// out, runs of whitespace as one space, trimmed, at most `max` characters.
/// `None` when nothing is left.
pub fn device_name(raw: &str, max: usize) -> Option<String> {
    let cleaned: String = raw
        .chars()
        .filter(|&c| !is_invisible(c))
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let cut: String = joined.chars().take(max).collect();
    let cut = cut.trim_end();
    (!cut.is_empty()).then(|| cut.to_owned())
}

/// Percent-encode everything but the RFC 3986 unreserved characters, so
/// a space is `%20` (never `+`) and every app's decoder reads it back.
fn encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `base` with `params` added to its query.
fn with_params(base: &str, params: &[(&str, &str)]) -> String {
    let mut out = base.to_owned();
    let mut separator = if base.contains('?') { '&' } else { '?' };
    for (key, value) in params {
        out.push(separator);
        out.push_str(key);
        out.push('=');
        out.push_str(&encode(value));
        separator = '&';
    }
    out
}

/// The address that hands `error` back to the app, with its state when
/// it had a usable one.
fn error_redirect(base: &str, error: &str, state: Option<&str>) -> String {
    match state {
        Some(state) => with_params(base, &[("error", error), ("state", state)]),
        None => with_params(base, &[("error", error)]),
    }
}

/// Whether `verifier` is a PKCE verifier (43-128 unreserved characters,
/// RFC 7636 section 4.1) that hashes to `challenge`. The comparison takes
/// the same time wherever the two differ.
pub fn verifier_matches(verifier: &str, challenge: &str) -> bool {
    let well_formed = (43..=128).contains(&verifier.len())
        && verifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte));
    let computed = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    well_formed & secret_eq(&computed, challenge)
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 7636 appendix B.
    const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    #[test]
    fn pkce_checks_the_rfc_7636_example() {
        assert!(verifier_matches(VERIFIER, CHALLENGE));
        assert!(!verifier_matches(&VERIFIER.replace('d', "e"), CHALLENGE));
        assert!(!verifier_matches(VERIFIER, &CHALLENGE.replace('E', "F")));
        assert!(!verifier_matches("", CHALLENGE));
        // Well-formed bounds, whatever it hashes to.
        let short = "a".repeat(42);
        assert!(!verifier_matches(
            &short,
            &URL_SAFE_NO_PAD.encode(Sha256::digest(short.as_bytes()))
        ));
        let long = "a".repeat(129);
        assert!(!verifier_matches(
            &long,
            &URL_SAFE_NO_PAD.encode(Sha256::digest(long.as_bytes()))
        ));
        let spaced = format!("{} ", "a".repeat(43));
        assert!(!verifier_matches(
            &spaced,
            &URL_SAFE_NO_PAD.encode(Sha256::digest(spaced.as_bytes()))
        ));
        let max = "~".repeat(128);
        assert!(verifier_matches(
            &max,
            &URL_SAFE_NO_PAD.encode(Sha256::digest(max.as_bytes()))
        ));
    }

    #[test]
    fn device_names_lose_what_could_mislead_and_fit_the_limit() {
        assert_eq!(
            device_name("  Pixel\u{0}\n 9\u{202E}lacol\u{200B} ", 64).as_deref(),
            Some("Pixel 9lacol")
        );
        assert_eq!(device_name("abcdef", 3).as_deref(), Some("abc"));
        assert_eq!(device_name("ab cdef", 3).as_deref(), Some("ab"));
        assert_eq!(device_name("", 3), None);
    }
}
