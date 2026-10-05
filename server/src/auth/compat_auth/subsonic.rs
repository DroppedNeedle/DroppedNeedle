//! Subsonic auth contract: three exclusive schemes, exact error codes,
//! and the binary-vs-envelope split.
//!
//! Schemes (mutually exclusive): `u+t+s` token (`t=md5(secret+s)`),
//! `u+p` password (hex-or-plain app password), lone `apiKey`. Conflicts
//! and duplicated keys fail with 10; bad credentials with 40; bad apiKey
//! with 44; `apiKey` plus `u` with 43. Only
//! `getOpenSubsonicExtensions` is public; every other endpoint (binary
//! stream/download/cover included) needs auth.
//!
//! The dispatch split: binary endpoints render dispatch-path errors with
//! codes outside `_AUTH_CODES` as `text/plain` (70 as 404, anything else
//! as 404); code 50 stays enveloped in the dispatch path, and the only
//! 403-as-text is getAvatar's direct call for a non-self username.

use sha2::{Digest, Sha256};

/// Advertised protocol version, in every envelope.
pub const API_VERSION: &str = "1.16.1";
/// Envelope namespace.
pub const NAMESPACE: &str = "http://subsonic.org/restapi";

/// Generic error.
pub const GENERIC: u8 = 0;
/// Required parameter missing.
pub const PARAM_MISSING: u8 = 10;
/// Wrong username or password.
pub const WRONG_CREDENTIALS: u8 = 40;
/// Multiple conflicting authentication mechanisms.
pub const CONFLICTING_AUTH: u8 = 43;
/// Invalid apiKey.
pub const INVALID_APIKEY: u8 = 44;
/// User not authorized for the operation.
pub const NOT_AUTHORIZED: u8 = 50;
/// Requested data not found.
pub const NOT_FOUND: u8 = 70;

/// v2 length caps, kept verbatim.
pub const MAX_USERNAME_LENGTH: usize = 256;
/// v2 length caps, kept verbatim.
pub const MAX_AUTH_VALUE_LENGTH: usize = 1024;
/// v2 length caps, kept verbatim (hex doubles the secret plus `enc:`).
pub const MAX_ENCODED_PASSWORD_LENGTH: usize = 2 * MAX_AUTH_VALUE_LENGTH + 4;
/// v2 length caps, kept verbatim.
pub const MAX_SALT_LENGTH: usize = 128;
/// v2 length caps, kept verbatim.
pub const MAX_TOKEN_LENGTH: usize = 128;
/// v2 length caps, kept verbatim.
pub const MAX_CLIENT_NAME_LENGTH: usize = 256;

/// Binary endpoints (normalized names): stream, download, getCoverArt,
/// getAvatar, getTranscodeStream.
pub const BINARY_ENDPOINTS: &[&str] = &[
    "stream",
    "download",
    "getcoverart",
    "getavatar",
    "gettranscodestream",
];

/// Codes that stay enveloped on the binary dispatch path.
pub const AUTH_CODES: &[u8] = &[10, 40, 41, 42, 43, 44, 50];

/// The one public endpoint (normalized name).
pub const PUBLIC_ENDPOINT: &str = "getopensubsonicextensions";

/// getAvatar refusal for a non-self username, v2 message verbatim.
pub const AVATAR_FORBIDDEN_MESSAGE: &str = "Avatar access is limited to the authenticated user";

/// Default message per code, v2 `errors.py` verbatim.
pub fn default_message(code: u8) -> &'static str {
    match code {
        PARAM_MISSING => "Required parameter is missing.",
        WRONG_CREDENTIALS => "Wrong username or password.",
        CONFLICTING_AUTH => "Multiple conflicting authentication mechanisms provided.",
        INVALID_APIKEY => "Invalid API key.",
        NOT_AUTHORIZED => "User is not authorized for the given operation.",
        NOT_FOUND => "The requested data was not found.",
        _ => "An error occurred.",
    }
}

/// Normalize an endpoint name: casefold, strip one `.view` suffix.
pub fn normalize_endpoint(raw: &str) -> String {
    let folded = raw.to_lowercase();
    folded.strip_suffix(".view").unwrap_or(&folded).to_owned()
}

/// True for the five binary endpoints (takes a normalized name).
pub fn is_binary_endpoint(normalized: &str) -> bool {
    BINARY_ENDPOINTS.contains(&normalized)
}

/// True only for `getOpenSubsonicExtensions` (takes a normalized name).
pub fn is_public_endpoint(normalized: &str) -> bool {
    normalized == PUBLIC_ENDPOINT
}

/// Dispatch rule: binary endpoints render codes outside [`AUTH_CODES`]
/// as `text/plain`; everything else (and every code on non-binary
/// endpoints) renders as an envelope. Note code 50 in the dispatch path
/// stays enveloped; the getAvatar 403-as-text is a direct call.
pub fn dispatch_uses_envelope(code: u8, normalized_endpoint: &str) -> bool {
    !is_binary_endpoint(normalized_endpoint) || AUTH_CODES.contains(&code)
}

/// Response format from the `f` param (default XML). Anything else is
/// code 10 (v2 raises `SubsonicError(10)` on an invalid `f`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubsonicFormat {
    /// XML (default).
    Xml,
    /// JSON.
    Json,
    /// JSONP (falls back to JSON on a missing/unsafe callback).
    Jsonp,
}

/// Parse the `f` param; `None` (absent) means XML.
pub fn parse_format(value: Option<&str>) -> Result<SubsonicFormat, SubsonicDenied> {
    match value {
        None => Ok(SubsonicFormat::Xml),
        Some("xml") => Ok(SubsonicFormat::Xml),
        Some("json") => Ok(SubsonicFormat::Json),
        Some("jsonp") => Ok(SubsonicFormat::Jsonp),
        Some(_) => Err(SubsonicDenied::new(PARAM_MISSING)),
    }
}

/// A callback is safe only as a bare identifier path, else the response
/// falls back to JSON (v2 `_CALLBACK_RE` parity:
/// `^[A-Za-z_$][\w$.]{0,127}$` with Unicode `\w` and a 128-char cap).
pub fn callback_is_safe(callback: &str) -> bool {
    let mut chars = callback.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_' || first == '$') {
        return false;
    }
    if callback.chars().count() > 128 {
        return false;
    }
    chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$' || c == '.')
}

/// One rendered failure: status, content type, and exact body bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedError {
    /// HTTP status (200 for envelopes, 403/404 for binary text).
    pub status: u16,
    /// Content type without charset suffix.
    pub content_type: &'static str,
    /// Exact body bytes.
    pub body: Vec<u8>,
}

impl RenderedError {
    /// Body as UTF-8 (all our bodies are ASCII-safe by construction).
    pub fn body_text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
}

/// Render a failed envelope (HTTP 200) in the requested format.
pub fn render_error(
    code: u8,
    message: &str,
    format: SubsonicFormat,
    callback: Option<&str>,
    server_name: &str,
    server_version: &str,
) -> RenderedError {
    let safe_callback = callback.filter(|cb| callback_is_safe(cb));
    match format {
        SubsonicFormat::Xml => RenderedError {
            status: 200,
            content_type: "application/xml",
            body: render_xml(code, message, server_name, server_version).into_bytes(),
        },
        SubsonicFormat::Json => RenderedError {
            status: 200,
            content_type: "application/json",
            body: render_json(code, message, server_name, server_version).into_bytes(),
        },
        SubsonicFormat::Jsonp => match safe_callback {
            Some(cb) => RenderedError {
                status: 200,
                content_type: "application/javascript",
                body: format!(
                    "{cb}({});",
                    render_json(code, message, server_name, server_version)
                )
                .into_bytes(),
            },
            None => RenderedError {
                status: 200,
                content_type: "application/json",
                body: render_json(code, message, server_name, server_version).into_bytes(),
            },
        },
    }
}

/// Render a binary-path failure as `text/plain`: 70 maps to 404, 50 maps
/// to 403, anything else maps to 404 (v2 `_binary_error` parity).
pub fn render_binary_error(code: u8, message: &str) -> RenderedError {
    let status = match code {
        NOT_FOUND => 404,
        NOT_AUTHORIZED => 403,
        _ => 404,
    };
    RenderedError {
        status,
        content_type: "text/plain",
        body: message.as_bytes().to_vec(),
    }
}

fn render_json(code: u8, message: &str, server_name: &str, server_version: &str) -> String {
    format!(
        "{{\"subsonic-response\":{{\"status\":\"failed\",\"version\":\"{API_VERSION}\",\"type\":\"{}\",\"serverVersion\":\"{}\",\"openSubsonic\":true,\"error\":{{\"code\":{code},\"message\":\"{}\"}}}}}}",
        json_escape(server_name),
        json_escape(server_version),
        json_escape(message),
    )
}

fn render_xml(code: u8, message: &str, server_name: &str, server_version: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><subsonic-response xmlns=\"{NAMESPACE}\" status=\"failed\" version=\"{API_VERSION}\" type=\"{}\" serverVersion=\"{}\" openSubsonic=\"true\"><error code=\"{code}\" message=\"{}\"/></subsonic-response>",
        xml_escape_attr(server_name),
        xml_escape_attr(server_version),
        xml_escape_attr(message),
    )
}

fn json_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn xml_escape_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if matches!(c, '\u{0}'..='\u{8}' | '\u{b}' | '\u{c}' | '\u{e}'..='\u{1f}') {
            continue;
        }
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

// --- auth ---

/// Query-plus-form params, repeats preserved (duplicates are an error).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubsonicParams {
    entries: Vec<(String, String)>,
}

impl SubsonicParams {
    /// Build from pairs in arrival order.
    pub fn new(entries: Vec<(&str, &str)>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
                .collect(),
        }
    }

    /// All values for a key, in order.
    pub fn all(&self, key: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// First value for a key, if present (even when empty).
    pub fn first(&self, key: &str) -> Option<&str> {
        self.all(key).into_iter().next()
    }
}

/// Auth denial: a code plus its default message. Auth paths raise bare
/// codes in v2, so the message is always [`default_message`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubsonicDenied {
    /// The Subsonic error code.
    pub code: u8,
}

impl SubsonicDenied {
    /// Deny with a code.
    pub fn new(code: u8) -> Self {
        Self { code }
    }

    /// The wire message for this denial.
    pub fn message(&self) -> &'static str {
        default_message(self.code)
    }

    /// Render as an envelope failure.
    pub fn render(
        &self,
        format: SubsonicFormat,
        callback: Option<&str>,
        server_name: &str,
        server_version: &str,
    ) -> RenderedError {
        render_error(
            self.code,
            self.message(),
            format,
            callback,
            server_name,
            server_version,
        )
    }
}

/// Authenticated principal: the owning user id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsonicPrincipal {
    /// `auth_users.id`.
    pub user_id: String,
}

/// One active app password: lookup hash plus recoverable plaintext (the
/// token scheme needs the secret to compute `md5(secret+s)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSecret {
    /// SHA-256 hex of the secret.
    pub sha256: String,
    /// The secret itself.
    pub plaintext: String,
}

/// Store failure. The router maps this to code 0 (generic), never to an
/// auth code: a broken store must not look like bad credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubsonicStoreError;

/// App-password lookups. The production adapter reads
/// `connect_app_passwords` (decrypting `secret_encrypted`) and
/// `auth_users` only: account passwords and native tokens are unreachable
/// here by construction, so presenting one fails exactly like an unknown
/// credential.
pub trait SubsonicPasswordStore: Clone + Send + Sync + 'static {
    /// User id for a lowercased username.
    fn user_id_for_username(
        &self,
        username_lower: &str,
    ) -> impl Future<Output = Result<Option<String>, SubsonicStoreError>> + Send;

    /// Active secrets for a user (plaintext needed for `t/s` auth).
    fn active_secrets(
        &self,
        user_id: &str,
    ) -> impl Future<Output = Result<Vec<AppSecret>, SubsonicStoreError>> + Send;

    /// Owning user id for a secret hash.
    fn owner_of_secret(
        &self,
        secret_sha256: &str,
    ) -> impl Future<Output = Result<Option<String>, SubsonicStoreError>> + Send;

    /// Best-effort use stamp (production throttles to ~5 min per secret).
    fn note_use(
        &self,
        secret_plaintext: &str,
        client: Option<&str>,
    ) -> impl Future<Output = ()> + Send;
}

/// Run the three schemes. Error precedence mirrors v2 exactly: duplicate
/// keys, presence conflicts, apiKey rules, presence of `u`, length caps,
/// user lookup, then per-scheme verification.
pub async fn authenticate<S: SubsonicPasswordStore>(
    store: &S,
    params: &SubsonicParams,
) -> Result<SubsonicPrincipal, SubsonicDenied> {
    for key in ["u", "t", "s", "p", "apiKey", "c"] {
        if params.all(key).len() > 1 {
            return Err(SubsonicDenied::new(PARAM_MISSING));
        }
    }
    let u = params.first("u");
    let t = params.first("t");
    let s = params.first("s");
    let p = params.first("p");
    let api_key = params.first("apiKey");
    let client = params.first("c");

    if t.is_some() != s.is_some() {
        return Err(SubsonicDenied::new(PARAM_MISSING));
    }
    if p.is_some() && t.is_some() {
        return Err(SubsonicDenied::new(PARAM_MISSING));
    }
    if api_key.is_some() && (t.is_some() || p.is_some()) {
        return Err(SubsonicDenied::new(PARAM_MISSING));
    }

    if client.is_some_and(|value| value.len() > MAX_CLIENT_NAME_LENGTH) {
        return Err(SubsonicDenied::new(PARAM_MISSING));
    }
    if api_key.is_some_and(|value| value.len() > MAX_AUTH_VALUE_LENGTH) {
        return Err(SubsonicDenied::new(INVALID_APIKEY));
    }
    if api_key.is_some_and(|value| !value.is_empty()) && u.is_some_and(|value| !value.is_empty()) {
        return Err(SubsonicDenied::new(CONFLICTING_AUTH));
    }
    if let Some(key) = api_key
        && !key.is_empty()
    {
        let owner = store
            .owner_of_secret(&sha256_hex(key))
            .await
            .map_err(|_| SubsonicDenied::new(GENERIC))?;
        let Some(user_id) = owner else {
            return Err(SubsonicDenied::new(INVALID_APIKEY));
        };
        store.note_use(key, None).await;
        return Ok(SubsonicPrincipal { user_id });
    }

    let Some(username) = u else {
        return Err(SubsonicDenied::new(PARAM_MISSING));
    };
    if username.is_empty() {
        return Err(SubsonicDenied::new(PARAM_MISSING));
    }
    if username.len() > MAX_USERNAME_LENGTH {
        return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
    }
    if t.is_some_and(|value| value.len() > MAX_TOKEN_LENGTH) {
        return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
    }
    if s.is_some_and(|value| value.len() > MAX_SALT_LENGTH) {
        return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
    }
    if p.is_some_and(|value| value.len() > MAX_ENCODED_PASSWORD_LENGTH) {
        return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
    }

    let user_id = store
        .user_id_for_username(&username.trim().to_lowercase())
        .await
        .map_err(|_| SubsonicDenied::new(GENERIC))?;
    let Some(user_id) = user_id else {
        return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
    };

    if let (Some(token), Some(salt)) = (t, s)
        && !token.is_empty()
        && !salt.is_empty()
    {
        let target = token.to_lowercase();
        let secrets = store
            .active_secrets(&user_id)
            .await
            .map_err(|_| SubsonicDenied::new(GENERIC))?;
        for secret in &secrets {
            if constant_time_eq(&md5_hex(&format!("{}{salt}", secret.plaintext)), &target) {
                store.note_use(&secret.plaintext, client).await;
                return Ok(SubsonicPrincipal { user_id });
            }
        }
        return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
    }

    if let Some(password) = p {
        let secret = decode_subsonic_password(password);
        if secret.len() > MAX_AUTH_VALUE_LENGTH {
            return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
        }
        let owner = store
            .owner_of_secret(&sha256_hex(&secret))
            .await
            .map_err(|_| SubsonicDenied::new(GENERIC))?;
        if owner.as_deref() == Some(user_id.as_str()) {
            store.note_use(&secret, client).await;
            return Ok(SubsonicPrincipal { user_id });
        }
        return Err(SubsonicDenied::new(WRONG_CREDENTIALS));
    }

    Err(SubsonicDenied::new(PARAM_MISSING))
}

/// getAvatar rule: the requested username must be one of the caller's own
/// names (username, display casing, or display name), case-insensitively.
/// Anything else renders [`render_binary_error`] with 50 (403-as-text).
pub fn avatar_is_self(caller_names: &[&str], requested: Option<&str>) -> bool {
    let Some(requested) = requested else {
        return false;
    };
    let wanted = requested.to_lowercase();
    caller_names
        .iter()
        .any(|name| name.to_lowercase() == wanted)
}

/// Decode the `p` param: `enc:<hex>` decodes to UTF-8, anything else
/// (bad hex, odd length, non-UTF-8, or no prefix) passes through raw.
pub fn decode_subsonic_password(p: &str) -> String {
    let Some(hex) = p.strip_prefix("enc:") else {
        return p.to_owned();
    };
    let digits: String = hex.chars().filter(|c| !c.is_ascii_whitespace()).collect();
    if digits.len() % 2 != 0 {
        return p.to_owned();
    }
    let mut bytes = Vec::with_capacity(digits.len() / 2);
    let chars: Vec<char> = digits.chars().collect();
    for pair in chars.chunks(2) {
        let (Some(hi), Some(lo)) = (pair[0].to_digit(16), pair[1].to_digit(16)) else {
            return p.to_owned();
        };
        bytes.push((hi * 16 + lo) as u8);
    }
    String::from_utf8(bytes).unwrap_or_else(|_| p.to_owned())
}

/// SHA-256 hex of a secret (lookup hash).
pub fn sha256_hex(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

/// Fixed-time string equality for hashes.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    let (x, y) = (a.as_bytes(), b.as_bytes());
    if x.len() != y.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..x.len() {
        diff |= x[i] ^ y[i];
    }
    diff == 0
}

/// Lowercase hex of `value` bytes.
pub fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// MD5 hex digest (RFC 1321). Used only for the Subsonic token scheme
/// (`t=md5(secret+s)`), which mandates MD5 for interop; never for anything
/// security-sensitive. The RFC vectors below plus the v2 spot value in the
/// compat goldens pin the bytes across the crate swap.
pub fn md5_hex(input: &str) -> String {
    format!("{:x}", md5::compute(input.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_matches_rfc1321_vectors() {
        assert_eq!(md5_hex(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex("a"), "0cc175b9c0f1b6a831c399e269772661");
        assert_eq!(md5_hex("abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex("message digest"),
            "f96b697d7cb7938d525a2f31aaf161d0"
        );
        assert_eq!(
            md5_hex("abcdefghijklmnopqrstuvwxyz"),
            "c3fcd3d76192e4007dfb496cca67e13b"
        );
        assert_eq!(
            md5_hex("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"),
            "d174ab98d277d9f5a5611c2c9f419d9f"
        );
    }

    #[test]
    fn password_decode_matches_v2() {
        assert_eq!(decode_subsonic_password("enc:616263"), "abc");
        assert_eq!(decode_subsonic_password("plain"), "plain");
        assert_eq!(decode_subsonic_password("enc:zzz"), "enc:zzz");
        assert_eq!(decode_subsonic_password("enc:6162636"), "enc:6162636");
        assert_eq!(decode_subsonic_password("enc:ff"), "enc:ff");
        assert_eq!(decode_subsonic_password("enc:61 62"), "ab");
        assert_eq!(decode_subsonic_password("enc:"), "");
    }
}
