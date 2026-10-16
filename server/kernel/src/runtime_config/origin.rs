//! A web origin (scheme, host and port), as browsers send it in `Origin`
//! and as an admin states this server's own addresses.

use std::fmt;

use axum::http::{
    Uri,
    uri::{Authority, Scheme},
};

/// A web origin: scheme, host and port, with the scheme's default port
/// filled in so `https://music.lan` and `https://music.lan:443` are equal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    scheme: String,
    host: String,
    port: u16,
}

impl Origin {
    /// The origin of an absolute `http` or `https` URL. `None` for anything
    /// else, including the `null` a sandboxed page sends.
    pub fn from_url(url: &str) -> Option<Self> {
        let uri: Uri = url.trim().parse().ok()?;
        Self::new(uri.scheme_str()?, uri.authority()?)
    }

    /// The origin a request was addressed to: its scheme and its `Host`
    /// value (`host` or `host:port`).
    pub fn from_host(scheme: &str, host: &str) -> Option<Self> {
        Self::new(scheme, &host.trim().parse().ok()?)
    }

    /// An address an admin typed, as pasted from the address bar: `http` or
    /// `https`, a host and an optional port, with at most a trailing `/`.
    /// The error is the reason it was refused, worded to follow the entry.
    pub fn from_stated(text: &str) -> Result<Self, String> {
        const NOT_AN_ADDRESS: &str = "is not a web address";
        const NO_SCHEME: &str = "must start with http:// or https://";
        const WILDCARD: &str = "has a wildcard in it: list each address in full";
        let text = text.trim();
        if text.is_empty() {
            return Err("is empty".to_owned());
        }
        if text.chars().any(|c| c.is_whitespace() || c.is_control()) {
            return Err("has a space in it".to_owned());
        }
        if text.contains('*') {
            return Err(WILDCARD.to_owned());
        }
        let lower = text.to_ascii_lowercase();
        if !lower.starts_with("http://") && !lower.starts_with("https://") {
            return Err(NO_SCHEME.to_owned());
        }
        let url = reqwest::Url::parse(text).map_err(|_| NOT_AN_ADDRESS.to_owned())?;
        if text.contains('@') || !url.username().is_empty() || url.password().is_some() {
            return Err("has a user name or password in it".to_owned());
        }
        if url.query().is_some() || url.fragment().is_some() {
            return Err("must end after the host and port: leave out the ? or # part".to_owned());
        }
        if url.path() != "/" {
            return Err(
                "must end after the host and port: a path, the base path included, is not \
                 part of it"
                    .to_owned(),
            );
        }
        if url.port() == Some(0) {
            return Err("has port 0: a port is between 1 and 65535".to_owned());
        }
        let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
            return Err(NOT_AN_ADDRESS.to_owned());
        };
        // The parser decodes `%2A` in a host to `*`.
        if host.contains('*') {
            return Err(WILDCARD.to_owned());
        }
        // Through the parser the check itself uses, so a saved address is
        // one a request's `Origin` can equal.
        Self::from_url(&format!("{}://{host}:{port}", url.scheme()))
            .ok_or_else(|| NOT_AN_ADDRESS.to_owned())
    }

    /// The text a browser sends as `Origin`: the default port left out.
    pub fn serialized(&self) -> String {
        let default_port = if self.scheme == Scheme::HTTPS.as_str() {
            443
        } else {
            80
        };
        if self.port == default_port {
            format!("{}://{}", self.scheme, self.host)
        } else {
            self.to_string()
        }
    }

    /// The host, lower-cased, an IPv6 address in its brackets.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// True for an `https` origin.
    pub fn is_https(&self) -> bool {
        self.scheme == Scheme::HTTPS.as_str()
    }

    /// True when both are on the same host, whatever the scheme and port.
    pub fn same_host(&self, other: &Self) -> bool {
        self.host == other.host
    }

    fn new(scheme: &str, authority: &Authority) -> Option<Self> {
        let scheme = scheme.to_ascii_lowercase();
        let default_port = if scheme == Scheme::HTTP.as_str() {
            80
        } else if scheme == Scheme::HTTPS.as_str() {
            443
        } else {
            return None;
        };
        let host = authority.host();
        if host.is_empty() {
            return None;
        }
        // Whatever follows the host must be a real port: `Authority` reads
        // one it cannot parse as absent, which would pass for the default.
        let after_userinfo = authority.as_str().rsplit('@').next()?;
        let port = match after_userinfo.strip_prefix(host)? {
            "" => default_port,
            port => port.strip_prefix(':')?.parse().ok()?,
        };
        Some(Self {
            host: host.to_ascii_lowercase(),
            scheme,
            port,
        })
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}:{}", self.scheme, self.host, self.port)
    }
}
