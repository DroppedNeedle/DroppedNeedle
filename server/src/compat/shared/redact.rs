//! Log-safe request targets for the compat surface.
//!
//! Ports v2's compat log redaction (the keys are pinned by tests).
//! Subsonic puts credentials in the query string (`p/t/s/apiKey`) on
//! every request, so access logs must never record the raw target: use
//! [`redact_request_target`]. Matching is case-insensitive over the
//! percent-decoded key name, repeats are preserved, and non-secret pairs
//! pass through untouched.

/// Query keys whose values are secrets (v2 `_SECRET_QS_KEYS`).
pub const SECRET_KEYS: [&str; 9] = [
    "p",
    "t",
    "s",
    "apikey",
    "api_key",
    "pw",
    "password",
    "token",
    "transcodeparams",
];

/// Mask substituted for secret values (v2 `_MASK`).
pub const MASK: &str = "***";

/// Whether a (decoded) query key names a secret.
pub fn is_secret_key(decoded_key: &str) -> bool {
    let folded = decoded_key.to_lowercase();
    SECRET_KEYS.iter().any(|key| *key == folded)
}

/// Return an access-log-safe path-plus-query target. Pairs without a
/// query pass through; secrets become `***`.
pub fn redact_request_target(target: &str) -> String {
    let Some(query_start) = target.find('?') else {
        return target.to_owned();
    };
    let (path, query) = target.split_at(query_start);
    let query = &query[1..];
    let mut out = String::with_capacity(target.len());
    out.push_str(path);
    out.push('?');
    for (index, pair) in query.split('&').enumerate() {
        if index > 0 {
            out.push('&');
        }
        let (raw_key, raw_value) = match pair.find('=') {
            Some(eq) => (&pair[..eq], Some(&pair[eq + 1..])),
            None => (pair, None),
        };
        let key = form_decode(raw_key);
        out.push_str(&form_encode(raw_key, &key));
        if let Some(value) = raw_value {
            out.push('=');
            if is_secret_key(&key) {
                out.push_str(MASK);
            } else {
                out.push_str(&form_encode(value, &form_decode(value)));
            }
        }
    }
    out
}

/// `application/x-www-form-urlencoded` decoding (what v2 `parse_qsl`
/// does): `+` becomes space, `%XX` becomes the byte.
fn form_decode(raw: &str) -> String {
    let mut bytes = Vec::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '+' => bytes.push(b' '),
            '%' => {
                let hex: String = chars.by_ref().take(2).collect();
                if hex.len() == 2
                    && let Ok(byte) = u8::from_str_radix(&hex, 16)
                {
                    bytes.push(byte);
                } else {
                    bytes.push(b'%');
                    bytes.extend_from_slice(hex.as_bytes());
                }
            }
            _ => bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Re-encode a decoded pair member the way v2 `urlencode` does,
/// preserving already-safe input byte-for-byte.
fn form_encode(raw: &str, decoded: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for byte in decoded.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char);
            }
            b' ' => out.push('+'),
            _ => {
                out.push('%');
                out.push_str(&format!("{byte:02X}"));
            }
        }
    }
    out
}
