//! Which playlist entry covers a viewer's browser may load.
//!
//! An entry's cover address comes from whoever added the entry, and every
//! reader of a public or shared playlist loads it. Only DroppedNeedle's own
//! cover routes and a few known art hosts are kept, so nobody can plant an
//! address that logs viewers, or a same-origin path the browser would
//! fetch with the viewer's cookies. This list is kept apart from
//! `droppedneedle_providers::images` on purpose: that one says what the
//! server may fetch, this one what viewers' browsers may load.

use url::Url;

/// Where an own route starts; anything before it is a base path.
const API_PREFIX: &str = "/api/v3/";

/// The longest id an own cover route takes.
const MAX_ID_BYTES: usize = 256;

/// Spotify's image CDN, as `providers/src/images.rs` names it.
const SPOTIFY_SUFFIX: &str = ".scdn.co";

/// Deezer's image CDN, as `providers/src/images.rs` names it.
const DEEZER_HOST: &str = "cdn-images.dzcdn.net";

/// TheAudioDB, itself or any subdomain.
const AUDIODB_HOST: &str = "theaudiodb.com";

/// Last.fm's image host, as every image in the answers recorded from
/// ws.audioscrobbler.com/2.0/ on 2026-10-26 names it
/// (`tests/fixtures/lastfm/`). Exact only: anyone on Fastly can get a
/// `*.freetls.fastly.net` name.
const LASTFM_HOST: &str = "lastfm-img.freetls.fastly.net";

/// YouTube's thumbnail hosts, from manual YouTube links.
const YOUTUBE_HOSTS: [&str; 2] = ["i.ytimg.com", "img.youtube.com"];

/// The cover address to store or show for an entry, or None when it is not
/// one a viewer's browser may load.
pub(crate) fn entry_cover(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.starts_with('/') && !raw.starts_with("//") {
        own_route(raw)
    } else {
        outside_image(raw)
    }
}

/// A root-relative own cover route, cut to start at `/api/v3/`.
fn own_route(raw: &str) -> Option<String> {
    // A base path may hold dots; only the cut route is kept, so only it is
    // held to the plain characters. A backslash anywhere is refused.
    if raw.contains('\\') {
        return None;
    }
    let route = &raw[raw.find(API_PREFIX)?..];
    if route.bytes().any(|byte| matches!(byte, b'.' | b'%' | b'#')) {
        return None;
    }
    let (path, query) = match route.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (route, None),
    };
    if let Some(query) = query
        && !query
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'=' | b'&' | b'_' | b'-'))
    {
        return None;
    }
    let parts: Vec<&str> = path[API_PREFIX.len()..].split('/').collect();
    let id = match parts.as_slice() {
        ["covers", "release-group" | "release" | "artist", id] => id,
        ["library", "albums", id, "artwork"] => id,
        ["remotes", "plex" | "jellyfin" | "navidrome", "images", id] => id,
        _ => return None,
    };
    let plain_id = !id.is_empty()
        && id.len() <= MAX_ID_BYTES
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':' | b'-'));
    plain_id.then(|| route.to_owned())
}

/// An https address on a known art host. TheAudioDB's own http addresses
/// are upgraded to https.
fn outside_image(raw: &str) -> Option<String> {
    let mut url = Url::parse(raw).ok()?;
    if !url.username().is_empty() || url.password().is_some() || url.port().is_some() {
        return None;
    }
    let host = url.domain()?.to_ascii_lowercase();
    let audiodb = host == AUDIODB_HOST || host.ends_with(&format!(".{AUDIODB_HOST}"));
    match url.scheme() {
        "https" => {}
        "http" if audiodb => url.set_scheme("https").ok()?,
        _ => return None,
    }
    let known = audiodb
        || host.ends_with(SPOTIFY_SUFFIX)
        || host == DEEZER_HOST
        || host == LASTFM_HOST
        || YOUTUBE_HOSTS.contains(&host.as_str());
    known.then(|| url.to_string())
}

#[cfg(test)]
mod tests {
    use super::entry_cover;

    #[test]
    fn entry_cover_keeps_only_own_routes_and_known_hosts() {
        let mbid = "8a7b6c5d-1234-4abc-9def-0123456789ab";
        for (raw, kept) in [
            (
                format!("/music/api/v3/covers/release-group/{mbid}?size=250"),
                format!("/api/v3/covers/release-group/{mbid}?size=250"),
            ),
            (
                format!("/dn.music/api/v3/covers/release/{mbid}"),
                format!("/api/v3/covers/release/{mbid}"),
            ),
            (
                "/api/v3/remotes/navidrome/images/dc-x:1_0".to_owned(),
                "/api/v3/remotes/navidrome/images/dc-x:1_0".to_owned(),
            ),
            (
                "/api/v3/library/albums/a1/artwork?size=250&v=3".to_owned(),
                "/api/v3/library/albums/a1/artwork?size=250&v=3".to_owned(),
            ),
            (
                "https://i.scdn.co/image/ab".to_owned(),
                "https://i.scdn.co/image/ab".to_owned(),
            ),
            (
                "http://www.theaudiodb.com/x.jpg".to_owned(),
                "https://www.theaudiodb.com/x.jpg".to_owned(),
            ),
        ] {
            assert_eq!(entry_cover(&raw), Some(kept), "{raw:?}");
        }
        for raw in [
            "https://tracker.example/pixel.png",
            "/api/v3/auth/logout",
            "/api/v3/covers/release-group/../../auth/x",
            "/api/v3/covers/release-group/%2e%2e/%2e%2e/auth/x",
            "/api/v3/covers/release-group\\x",
            "//evil.example/x",
            "/\\evil.example/x",
            "/\\evil.example/api/v3/covers/release/x",
            "https://u@i.scdn.co/x",
            "https://i.scdn.co:8443/x",
            "http://i.scdn.co/x",
            "https://i.scdn.co.evil.example/x",
            "https://evil.freetls.fastly.net/x",
            "data:image/png;base64,AAAA",
            "javascript:alert(1)",
        ] {
            assert_eq!(entry_cover(raw), None, "{raw:?}");
        }
    }
}
