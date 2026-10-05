//! `streaming_source`: plugins serve audio for recordings the library
//! does not have.
//!
//! Local files always win; a plugin is asked only after the library
//! misses, so it can never shadow a track you own. Plugins are asked in
//! name order under a 5 second budget each and the first answer wins. A
//! plugin gets the recording MBID and the signed-in user's id, never a
//! token.
//!
//! An answer is a `path` or a `url`, never both, and the host checks it
//! before a byte moves:
//! - A path (relative paths are inside the plugin folder) must resolve,
//!   after symlinks, to a file inside the plugin folder, the folder in its
//!   `downloads_dir` setting, or a library root.
//! - A URL must be `http(s)` and every address its host resolves to must
//!   be public: loopback, private, link-local, CGNAT, multicast and
//!   unspecified addresses are refused. The proxy re-checks the address it
//!   actually connected to and every redirect (at most 3).

use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::host::{LoadedPlugin, PluginHost};
use super::super::protocol::methods;
use super::{call, decode};

/// Budget per plugin.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
/// DNS budget for a URL answer.
const DNS_TIMEOUT: Duration = Duration::from_secs(5);
/// Redirects the proxy follows, each re-checked.
pub const MAX_REDIRECTS: usize = 3;
/// Connect-and-headers budget for the proxy.
pub const PROXY_TIMEOUT: Duration = Duration::from_secs(10);

/// A plugin's answer.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginStreamRef {
    /// Local file the host may serve or transcode.
    pub path: String,
    /// Remote `http(s)` URL the host proxies.
    pub url: String,
    /// Content type hint; the host decides when empty.
    pub content_type: String,
    /// Track length, when the plugin knows it.
    pub duration_seconds: Option<f64>,
}

/// A checked plugin stream.
#[derive(Debug, Clone, PartialEq)]
pub enum PluginStream {
    /// A file inside the allowed roots, symlinks resolved.
    File {
        /// Plugin that answered.
        plugin: String,
        /// Canonical path.
        path: PathBuf,
        /// Content type hint.
        content_type: String,
        /// Track length, when known.
        duration_seconds: Option<f64>,
    },
    /// A public `http(s)` URL.
    Remote {
        /// Plugin that answered.
        plugin: String,
        /// The URL.
        url: reqwest::Url,
        /// Content type hint.
        content_type: String,
        /// Track length, when known.
        duration_seconds: Option<f64>,
    },
}

impl PluginHost {
    /// Ask `streaming_source` plugins for one recording. `library_roots`
    /// are the music roots a path answer may also point into.
    pub async fn resolve_stream(
        &self,
        recording_mbid: &str,
        user_id: &str,
        library_roots: &[PathBuf],
    ) -> Option<PluginStream> {
        if recording_mbid.is_empty() || user_id.is_empty() {
            return None;
        }
        for plugin in self.serving("streaming_source") {
            let params = json!({ "recording_mbid": recording_mbid, "user_id": user_id });
            let Ok(value) = call(&plugin, methods::RESOLVE_STREAM, params, RESOLVE_TIMEOUT).await
            else {
                continue;
            };
            let Ok(Some(answer)) =
                decode::<Option<PluginStreamRef>>(&plugin, methods::RESOLVE_STREAM, value)
            else {
                continue;
            };
            match self.check_ref(&plugin, answer, library_roots).await {
                Ok(stream) => return Some(stream),
                Err(reason) => {
                    tracing::warn!(
                        plugin = %plugin.manifest.name,
                        %reason,
                        "plugin stream refused"
                    );
                }
            }
        }
        None
    }

    async fn check_ref(
        &self,
        plugin: &LoadedPlugin,
        answer: PluginStreamRef,
        library_roots: &[PathBuf],
    ) -> Result<PluginStream, String> {
        let name = plugin.manifest.name.clone();
        match (answer.path.trim(), answer.url.trim()) {
            (path, "") if !path.is_empty() => {
                let mut roots = vec![PathBuf::from(&plugin.directory)];
                if plugin
                    .manifest
                    .settings
                    .iter()
                    .any(|field| field.key == "downloads_dir")
                    && let Ok(stored) = self.config().get_plugin(&name)
                    && let Some(dir) = stored
                        .settings
                        .get("downloads_dir")
                        .filter(|dir| !dir.trim().is_empty())
                {
                    roots.push(PathBuf::from(dir.trim()));
                }
                roots.extend(library_roots.iter().cloned());
                let base = PathBuf::from(&plugin.directory);
                let wanted = PathBuf::from(path);
                let candidate = if wanted.is_absolute() {
                    wanted
                } else {
                    base.join(wanted)
                };
                let checked =
                    tokio::task::spawn_blocking(move || contained_file(&candidate, &roots))
                        .await
                        .map_err(|error| error.to_string())??;
                Ok(PluginStream::File {
                    plugin: name,
                    path: checked,
                    content_type: answer.content_type,
                    duration_seconds: answer.duration_seconds,
                })
            }
            ("", url) if !url.is_empty() => {
                let url = check_url(url).await?;
                Ok(PluginStream::Remote {
                    plugin: name,
                    url,
                    content_type: answer.content_type,
                    duration_seconds: answer.duration_seconds,
                })
            }
            _ => Err("the answer must set exactly one of path or url".to_owned()),
        }
    }
}

/// Resolve `candidate` and require a regular file inside one of `roots`
/// (each resolved too). Symlinks pointing out of the roots are refused.
pub fn contained_file(candidate: &Path, roots: &[PathBuf]) -> Result<PathBuf, String> {
    let resolved = candidate
        .canonicalize()
        .map_err(|_| "the file does not exist".to_owned())?;
    let inside = roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| resolved.starts_with(&root));
    if !inside {
        return Err(
            "the file is outside the plugin folder, its downloads folder and the library"
                .to_owned(),
        );
    }
    let meta = std::fs::metadata(&resolved).map_err(|error| error.to_string())?;
    if !meta.is_file() {
        return Err("the path is not a file".to_owned());
    }
    Ok(resolved)
}

/// Whether an address is one a plugin URL may reach.
pub fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, ..] = v4.octets();
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                || v4.is_documentation()
                || a == 0
                || (a == 100 && (64..128).contains(&b))
                || (a == 198 && (b == 18 || b == 19))
                || a >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return public_ip(IpAddr::V4(mapped));
            }
            let first = v6.segments()[0];
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
                || first == 0x2001 && v6.segments()[1] == 0x0db8)
        }
    }
}

/// Check one URL: `http(s)`, a host, and only public addresses behind it.
pub async fn check_url(raw: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(raw).map_err(|_| "the URL does not parse".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err("only http and https URLs are allowed".to_owned());
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err("URLs with credentials are not allowed".to_owned());
    }
    let host = url
        .host_str()
        .ok_or_else(|| "the URL has no host".to_owned())?;
    if host.eq_ignore_ascii_case("localhost") || host.to_ascii_lowercase().ends_with(".localhost") {
        return Err("local addresses are not allowed".to_owned());
    }
    let port = url.port_or_known_default().unwrap_or(443);
    let host = host.trim_matches(['[', ']']).to_owned();
    let addresses: Vec<SocketAddr> =
        match tokio::time::timeout(DNS_TIMEOUT, tokio::net::lookup_host((host.as_str(), port)))
            .await
        {
            Ok(Ok(found)) => found.collect(),
            Ok(Err(_)) => return Err("the host does not resolve".to_owned()),
            Err(_) => return Err("the host took too long to resolve".to_owned()),
        };
    if addresses.is_empty() || addresses.iter().any(|address| !public_ip(address.ip())) {
        return Err("the host resolves to a local or private address".to_owned());
    }
    Ok(url)
}

/// Open a checked URL through a client that does not follow redirects:
/// re-check the address actually connected to, follow at most
/// [`MAX_REDIRECTS`] redirects with each target checked again, and pass
/// through a `Range` header.
pub async fn open_url(
    client: &reqwest::Client,
    url: reqwest::Url,
    range: Option<&str>,
) -> Result<reqwest::Response, String> {
    let mut current = url;
    for _ in 0..=MAX_REDIRECTS {
        let mut request = client.get(current.clone()).timeout(PROXY_TIMEOUT);
        if let Some(range) = range {
            request = request.header(reqwest::header::RANGE, range);
        }
        let response = request.send().await.map_err(|error| error.to_string())?;
        match response.remote_addr() {
            Some(address) if public_ip(address.ip()) => {}
            _ => return Err("the connection went to a local or private address".to_owned()),
        }
        if !response.status().is_redirection() {
            return Ok(response);
        }
        let location = response
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| "a redirect had no location".to_owned())?;
        let next = current
            .join(location)
            .map_err(|_| "a redirect location does not parse".to_owned())?;
        current = check_url(next.as_str()).await?;
    }
    Err("too many redirects".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_public_addresses_pass() {
        for blocked in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "0.0.0.0",
            "100.64.0.1",
            "::1",
            "fe80::1",
            "fd00::1",
            "::ffff:127.0.0.1",
        ] {
            assert!(!public_ip(blocked.parse().unwrap()), "{blocked}");
        }
        for open in ["93.184.216.34", "2606:4700::1111"] {
            assert!(public_ip(open.parse().unwrap()), "{open}");
        }
    }

    #[tokio::test]
    async fn urls_to_local_hosts_are_refused() {
        assert!(check_url("http://127.0.0.1/a.flac").await.is_err());
        assert!(check_url("http://localhost:8080/a.flac").await.is_err());
        assert!(check_url("file:///etc/passwd").await.is_err());
        assert!(
            check_url("https://user:pw@example.com/a.flac")
                .await
                .is_err()
        );
    }

    #[test]
    fn files_must_stay_inside_the_roots() {
        let root = std::env::temp_dir().join(format!("dn-stream-roots-{}", std::process::id()));
        let inside = root.join("inside");
        let outside = root.join("outside");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(inside.join("a.flac"), b"x").unwrap();
        std::fs::write(outside.join("b.flac"), b"x").unwrap();
        std::os::unix::fs::symlink(outside.join("b.flac"), inside.join("link.flac")).unwrap();
        let roots = vec![inside.clone()];
        assert!(contained_file(&inside.join("a.flac"), &roots).is_ok());
        assert!(contained_file(&inside.join("../outside/b.flac"), &roots).is_err());
        assert!(contained_file(&inside.join("link.flac"), &roots).is_err());
        assert!(contained_file(&inside, &roots).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
