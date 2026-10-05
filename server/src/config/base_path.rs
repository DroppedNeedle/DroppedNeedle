//! Reverse-proxy mount prefix (`BASE_PATH`) validation.
//!
//! One strict normalizer, ported from v2's `core/base_path.py`: the value is
//! either empty (served at the domain root) or a canonical `/seg[/seg...]`
//! path. Anything else fails at config load instead of being coerced, so
//! the router, the session gate and the stamped web UI all agree on one
//! byte-exact prefix.

use thiserror::Error;

/// Longest accepted base path, in bytes (ASCII only, so also characters).
pub const MAX_BASE_PATH_LENGTH: usize = 256;

/// Why a `BASE_PATH` value was refused.
#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum BasePathError {
    /// The value does not start with `/`.
    #[error("must be an absolute path starting with '/'")]
    NotAbsolute,
    /// The value is longer than [`MAX_BASE_PATH_LENGTH`].
    #[error("exceeds the {MAX_BASE_PATH_LENGTH}-character limit")]
    TooLong,
    /// The value ends with `/`.
    #[error("must not end with '/'")]
    TrailingSlash,
    /// Two slashes in a row.
    #[error("contains an empty path segment")]
    EmptySegment,
    /// A `.` or `..` segment.
    #[error("relative '.' and '..' segments are forbidden")]
    DotSegment,
    /// A segment with characters outside `A-Z a-z 0-9 . _ ~ -`.
    #[error("segment {0:?} has characters outside A-Z a-z 0-9 . _ ~ -")]
    BadSegment(String),
    /// The first segment is `api`, which would shadow the API prefix.
    #[error("must not start with /api: that prefix belongs to the API")]
    ShadowsApi,
}

/// Return `""` or a canonical `/seg[/seg...]` base path.
///
/// Fails closed on surrounding whitespace, escapes, query or fragment
/// characters, non-ASCII bytes, dot segments, empty segments and overlong
/// values. A first segment of `api` (any case) is refused too: the session
/// gate and the web UI fallback both treat `/api` as the API namespace.
pub fn normalize_base_path(raw: &str) -> Result<String, BasePathError> {
    if raw.is_empty() {
        return Ok(String::new());
    }
    if !raw.starts_with('/') {
        return Err(BasePathError::NotAbsolute);
    }
    if raw.len() > MAX_BASE_PATH_LENGTH {
        return Err(BasePathError::TooLong);
    }
    if raw.ends_with('/') {
        return Err(BasePathError::TrailingSlash);
    }
    for segment in raw[1..].split('/') {
        if segment.is_empty() {
            return Err(BasePathError::EmptySegment);
        }
        if segment == "." || segment == ".." {
            return Err(BasePathError::DotSegment);
        }
        let canonical = segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'~' | b'-'));
        if !canonical {
            return Err(BasePathError::BadSegment(segment.to_owned()));
        }
    }
    let first = raw[1..].split('/').next().unwrap_or_default();
    if first.eq_ignore_ascii_case("api") {
        return Err(BasePathError::ShadowsApi);
    }
    Ok(raw.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_canonical_and_refuses_the_rest() {
        assert_eq!(normalize_base_path("").unwrap(), "");
        assert_eq!(normalize_base_path("/music").unwrap(), "/music");
        assert_eq!(normalize_base_path("/a/b-c_d.e~f").unwrap(), "/a/b-c_d.e~f");
        assert_eq!(normalize_base_path("/apis").unwrap(), "/apis");
        for (raw, expected) in [
            ("music", BasePathError::NotAbsolute),
            (" /music", BasePathError::NotAbsolute),
            ("/music/", BasePathError::TrailingSlash),
            ("/", BasePathError::TrailingSlash),
            ("/a//b", BasePathError::EmptySegment),
            ("/a/../b", BasePathError::DotSegment),
            ("/a%2fb", BasePathError::BadSegment("a%2fb".to_owned())),
            ("/a?b", BasePathError::BadSegment("a?b".to_owned())),
            (
                "/caf\u{e9}",
                BasePathError::BadSegment("caf\u{e9}".to_owned()),
            ),
            ("/api", BasePathError::ShadowsApi),
            ("/API/v3", BasePathError::ShadowsApi),
        ] {
            assert_eq!(normalize_base_path(raw), Err(expected), "{raw:?}");
        }
        let long = format!("/{}", "a".repeat(MAX_BASE_PATH_LENGTH));
        assert_eq!(normalize_base_path(&long), Err(BasePathError::TooLong));
    }
}
