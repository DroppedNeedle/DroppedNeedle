//! Strict, bounded Subsonic parameter decoding.
//! v2: `backend/api/compat/subsonic/parameters.py` (all caps verbatim).

use super::error::{PARAM_MISSING, SubsonicError};

/// Max raw bytes of query string or POST body.
pub const MAX_REQUEST_PARAMETER_BYTES: usize = 64 * 1024;
/// Max total parameter pairs.
pub const MAX_PARAMETER_COUNT: usize = 1024;
/// Max parameter name length.
pub const MAX_PARAMETER_KEY_LENGTH: usize = 128;
/// Max single parameter value length.
pub const MAX_PARAMETER_VALUE_LENGTH: usize = 8192;
/// Max repeats of one key via [`SubsonicParameters::strings`].
pub const MAX_REPEAT_COUNT: usize = 500;
/// Max length accepted by [`SubsonicParameters::string`].
pub const MAX_STRING_LENGTH: usize = 4096;

/// Query-plus-form params with repeats preserved (duplicates are
/// significant: auth keys reject them, `id`/`songId` accept them).
#[derive(Debug, Clone, Default)]
pub struct SubsonicParameters {
    entries: Vec<(String, String)>,
}

impl SubsonicParameters {
    /// Build from decoded pairs (arrival order, query first then body).
    pub fn new(entries: Vec<(String, String)>) -> Self {
        Self { entries }
    }

    /// All values for a key, in order.
    pub fn all(&self, name: &str) -> Vec<&str> {
        self.entries
            .iter()
            .filter(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    /// Single value for a key; repeats or overlong values are code 10.
    /// v2: `len(values) != 1` raises, so a duplicated key fails even
    /// for ordinary params.
    pub fn string(
        &self,
        name: &str,
        default: Option<&str>,
    ) -> Result<Option<String>, SubsonicError> {
        self.string_max(name, default, MAX_STRING_LENGTH)
    }

    /// [`SubsonicParameters::string`] with an explicit cap.
    pub fn string_max(
        &self,
        name: &str,
        default: Option<&str>,
        max_length: usize,
    ) -> Result<Option<String>, SubsonicError> {
        let values = self.all(name);
        if values.is_empty() {
            return Ok(default.map(str::to_owned));
        }
        if values.len() != 1 || values[0].len() > max_length {
            return Err(SubsonicError::invalid(name));
        }
        Ok(Some(values[0].to_owned()))
    }

    /// Repeated values for a key (plural params like `id`, `songId`).
    pub fn strings(&self, name: &str) -> Result<Vec<String>, SubsonicError> {
        let values = self.all(name);
        if values.len() > MAX_REPEAT_COUNT
            || values.iter().any(|value| value.len() > MAX_STRING_LENGTH)
        {
            return Err(SubsonicError::invalid(name));
        }
        Ok(values.into_iter().map(str::to_owned).collect())
    }

    /// Integer param: optional sign + ASCII digits, max 20 chars, then
    /// range-checked. Empty, non-numeric, or out-of-range is code 10.
    pub fn integer(
        &self,
        name: &str,
        default: Option<i64>,
        minimum: Option<i64>,
        maximum: Option<i64>,
    ) -> Result<Option<i64>, SubsonicError> {
        let raw = match self.string(name, None)? {
            Some(raw) => raw,
            None => return Ok(default),
        };
        if raw.is_empty() || raw.len() > 20 || !is_integer(&raw) {
            return Err(SubsonicError::invalid(name));
        }
        let value: i64 = raw.parse().map_err(|_| SubsonicError::invalid(name))?;
        if minimum.is_some_and(|lo| value < lo) || maximum.is_some_and(|hi| value > hi) {
            return Err(SubsonicError::invalid(name));
        }
        Ok(Some(value))
    }

    /// Float param: parses via f64, rejects NaN/inf, then range-checked.
    pub fn number(
        &self,
        name: &str,
        default: Option<f64>,
        minimum: Option<f64>,
        maximum: Option<f64>,
    ) -> Result<Option<f64>, SubsonicError> {
        let raw = match self.string(name, None)? {
            Some(raw) => raw,
            None => return Ok(default),
        };
        if raw.is_empty() || raw.len() > 64 {
            return Err(SubsonicError::invalid(name));
        }
        let value: f64 = raw.parse().map_err(|_| SubsonicError::invalid(name))?;
        if !value.is_finite() {
            return Err(SubsonicError::invalid(name));
        }
        if minimum.is_some_and(|lo| value < lo) || maximum.is_some_and(|hi| value > hi) {
            return Err(SubsonicError::invalid(name));
        }
        Ok(Some(value))
    }

    /// Boolean param: `true`/`1` vs `false`/`0`, case-insensitive.
    /// Anything else is code 10 (v2 has no lenient fallback).
    pub fn boolean(&self, name: &str, default: bool) -> Result<bool, SubsonicError> {
        let raw = match self.string(name, None)? {
            Some(raw) => raw,
            None => return Ok(default),
        };
        match raw.to_lowercase().as_str() {
            "true" | "1" => Ok(true),
            "false" | "0" => Ok(false),
            _ => Err(SubsonicError::invalid(name)),
        }
    }

    /// Enum param: value must be an exact member.
    pub fn one_of(
        &self,
        name: &str,
        allowed: &[&str],
        default: Option<&str>,
    ) -> Result<Option<String>, SubsonicError> {
        let value = self.string(name, None)?;
        match value {
            None => Ok(default.map(str::to_owned)),
            Some(value) if allowed.contains(&value.as_str()) => Ok(Some(value)),
            Some(_) => Err(SubsonicError::invalid(name)),
        }
    }
}

/// `^[+-]?[0-9]+$` (v2 `_INTEGER_RE`).
fn is_integer(raw: &str) -> bool {
    let digits = raw.strip_prefix(['+', '-']).unwrap_or(raw);
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Decode one `application/x-www-form-urlencoded` blob into pairs.
/// Malformed percent escapes and non-UTF-8 are code 10
/// (v2 `_decode_pairs`).
pub fn decode_pairs(raw: &[u8]) -> Result<Vec<(String, String)>, SubsonicError> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| SubsonicError::new(PARAM_MISSING, "Invalid request parameter encoding"))?;
    if has_malformed_percent(text) {
        return Err(SubsonicError::new(
            PARAM_MISSING,
            "Invalid request parameter encoding",
        ));
    }
    let mut pairs = Vec::new();
    for pair in text.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match pair.split_once('=') {
            Some((key, value)) => (key, value),
            None => (pair, ""),
        };
        pairs.push((form_decode(key)?, form_decode(value)?));
    }
    Ok(pairs)
}

/// `%` not followed by two hex digits (v2 `_MALFORMED_PERCENT`).
fn has_malformed_percent(text: &str) -> bool {
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let followed_by_hex_pair = i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit();
            if !followed_by_hex_pair {
                return true;
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    false
}

/// `+` becomes space, `%XX` becomes the byte; the result must be UTF-8.
/// (`parse_qsl` semantics; invalid UTF-8 is code 10.)
fn form_decode(part: &str) -> Result<String, SubsonicError> {
    let mut bytes = Vec::with_capacity(part.len());
    let raw = part.as_bytes();
    let mut i = 0;
    while i < raw.len() {
        match raw[i] {
            b'+' => {
                bytes.push(b' ');
                i += 1;
            }
            b'%' => {
                let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
                let pair = (i + 2 < raw.len()).then(|| (hex(raw[i + 1]), hex(raw[i + 2])));
                match pair {
                    Some((Some(hi), Some(lo))) => {
                        bytes.push(hi * 16 + lo);
                        i += 3;
                    }
                    _ => {
                        return Err(SubsonicError::new(
                            PARAM_MISSING,
                            "Invalid request parameter encoding",
                        ));
                    }
                }
            }
            b => {
                bytes.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(bytes)
        .map_err(|_| SubsonicError::new(PARAM_MISSING, "Invalid request parameter encoding"))
}

/// Validate pair counts and lengths (v2 `parse_request_parameters` tail).
pub fn check_limits(pairs: &[(String, String)]) -> Result<(), SubsonicError> {
    if pairs.len() > MAX_PARAMETER_COUNT {
        return Err(SubsonicError::new(
            PARAM_MISSING,
            "Too many request parameters",
        ));
    }
    for (key, value) in pairs {
        if key.is_empty() || key.len() > MAX_PARAMETER_KEY_LENGTH {
            return Err(SubsonicError::new(PARAM_MISSING, "Invalid parameter name"));
        }
        if value.len() > MAX_PARAMETER_VALUE_LENGTH {
            return Err(SubsonicError::invalid(key));
        }
    }
    Ok(())
}
