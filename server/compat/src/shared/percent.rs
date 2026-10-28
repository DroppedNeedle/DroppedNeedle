//! Percent-decoding for compat query strings and form bodies, in the two
//! modes the protocols need.
//!
//! Strict refuses a `%` without two hex digits after it and bytes that are
//! not UTF-8: credentials and Subsonic parameters, where a guess would be
//! wrong. Lossy keeps a bad escape as text and turns bytes that are not
//! UTF-8 into U+FFFD: Jellyfin queries and the log redactor, which must
//! read anything a client sends.
//!
//! Written here because no decoder among the dependencies is strict enough
//! for a credential: `percent-encoding` keeps a bad escape as text, and the
//! form decoders turn `+` into a space and replace bytes that are not UTF-8.

/// What a `+` means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plus {
    /// A space, as in a form body or a query string.
    Space,
    /// A `+`, as in a credential that may hold one.
    Literal,
}

/// A bad escape, or bytes that are not UTF-8.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed;

/// Decode `raw`, refusing a bad escape or bytes that are not UTF-8.
pub fn strict(raw: &str, plus: Plus) -> Result<String, Malformed> {
    let bytes = decode(raw, plus, true)?;
    String::from_utf8(bytes).map_err(|_| Malformed)
}

/// Decode `raw` with `+` as a space. A bad escape stays as text and bytes
/// that are not UTF-8 become U+FFFD.
pub fn lossy(raw: &str) -> String {
    match decode(raw, Plus::Space, false) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(Malformed) => raw.to_owned(),
    }
}

/// The bytes `raw` encodes. A bad escape fails when `refuse_bad_escape`,
/// else its `%` is kept and decoding goes on after it.
fn decode(raw: &str, plus: Plus, refuse_bad_escape: bool) -> Result<Vec<u8>, Malformed> {
    let mut out = Vec::with_capacity(raw.len());
    let mut rest = raw.as_bytes();
    while let Some((&byte, tail)) = rest.split_first() {
        rest = tail;
        match byte {
            b'+' if plus == Plus::Space => out.push(b' '),
            b'%' => match escape(tail) {
                Some((value, after)) => {
                    out.push(value);
                    rest = after;
                }
                None if refuse_bad_escape => return Err(Malformed),
                None => out.push(b'%'),
            },
            _ => out.push(byte),
        }
    }
    Ok(out)
}

/// The byte two hex digits at the start of `tail` spell, and what follows.
fn escape(tail: &[u8]) -> Option<(u8, &[u8])> {
    let [hi, lo, after @ ..] = tail else {
        return None;
    };
    Some((hex(*hi)? << 4 | hex(*lo)?, after))
}

fn hex(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}
