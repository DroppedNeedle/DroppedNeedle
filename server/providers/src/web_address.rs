//! Addresses an outside server hands us that may reach a browser as a link.
//!
//! A provider's page link, a ticket address, a release page: each is held
//! to [`web_address`] before it is kept. A field that must hold one decodes
//! as [`WebAddress`] and fails otherwise; an optional one decodes through
//! [`or_none`], so a bad value reads as no link.

use std::fmt;

use crate::executor::Url;
use serde::{Deserialize, Deserializer, de};

/// `raw` as a web address: http or https, with a host and no credentials.
/// Anything else is `None`.
#[must_use]
pub fn web_address(raw: &str) -> Option<Url> {
    let url = Url::parse(raw.trim()).ok()?;
    let usable = matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some_and(|host| !host.is_empty())
        && url.username().is_empty()
        && url.password().is_none();
    usable.then_some(url)
}

/// An http or https address with a host and no credentials. Anything else
/// (`javascript:`, `data:`, a bare path) fails decoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebAddress(String);

impl WebAddress {
    /// `raw` when it is a web address.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        web_address(raw).map(|url| Self(url.into()))
    }

    /// The address as text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WebAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for WebAddress {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).ok_or_else(|| de::Error::custom("not an http or https address"))
    }
}

impl From<WebAddress> for String {
    fn from(address: WebAddress) -> Self {
        address.0
    }
}

/// An optional address for `#[serde(default, deserialize_with = ...)]`:
/// missing, null, not text or not a web address all read as `None`.
///
/// # Errors
///
/// Only when the input itself cannot be read.
pub fn or_none<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<WebAddress>, D::Error> {
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value.as_str().and_then(WebAddress::parse))
}

/// An address read from an answer: the [`WebAddress`] text, or empty when
/// the answer had none.
#[must_use]
pub fn or_empty(address: Option<WebAddress>) -> String {
    address.map(String::from).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_http_and_https_with_a_host_and_no_credentials() {
        assert!(WebAddress::parse("https://www.last.fm/music/Cher").is_some());
        assert!(WebAddress::parse(" http://example.invalid/a ").is_some());
        for bad in [
            "javascript:alert(1)",
            "data:text/html,x",
            "/relative",
            "//example.invalid/x",
            "ftp://example.invalid/x",
            "https://user:pass@example.invalid/",
            "",
        ] {
            assert!(WebAddress::parse(bad).is_none(), "{bad}");
        }
    }
}
