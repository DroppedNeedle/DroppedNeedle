//! Redirect hops and BrainzMash URL checks.
//!
//! Merged MusicBrainz entities answer lookups with a 3xx. The client
//! follows a hop only after it validates here as a same-origin,
//! same-entity lookup between two well-formed MBIDs; BrainzMash requests
//! must also stay on the one approved origin and path allowlist.

use super::{BRAINZMASH_API_BASE, MbError, is_valid_mbid};

/// One followed redirect hop: same entity on both ends, both MBIDs valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectHop {
    /// Entity kind (`artist`, `release`, `release-group`, `recording`).
    pub entity: String,
    /// Retired MBID the client requested.
    pub from_mbid: String,
    /// Canonical MBID the provider pointed at.
    pub to_mbid: String,
}

/// Entities whose lookup redirects persist (v2 `_MB_REDIRECT_ENTITY_KINDS`).
const REDIRECT_ENTITY_KINDS: [&str; 4] = ["artist", "release", "release-group", "recording"];

/// Validate one followed hop as an entity/from/to triple. Only
/// lookup-shaped paths persist: exactly two segments, an allowlisted entity
/// on both ends, the same entity on each end, both MBIDs valid. A 301 on a
/// browse path such as `/release-group?artist=` is never persisted, since
/// the only MBID in play there belongs to another entity (v2
/// `_lookup_redirect_pair`).
pub fn lookup_redirect_pair(from_path: &str, hop_path: &str) -> Option<RedirectHop> {
    let from_segments: Vec<&str> = from_path
        .split('?')
        .next()?
        .trim_matches('/')
        .split('/')
        .collect();
    let hop_segments: Vec<&str> = hop_path
        .split('?')
        .next()?
        .trim_matches('/')
        .split('/')
        .collect();
    if from_segments.len() != 2 || hop_segments.len() != 2 {
        return None;
    }
    let (entity, from_mbid) = (from_segments[0], from_segments[1]);
    let (hop_entity, to_mbid) = (hop_segments[0], hop_segments[1]);
    if entity != hop_entity || !REDIRECT_ENTITY_KINDS.contains(&entity) {
        return None;
    }
    if !is_valid_mbid(from_mbid) || !is_valid_mbid(to_mbid) {
        return None;
    }
    Some(RedirectHop {
        entity: entity.to_owned(),
        from_mbid: from_mbid.to_owned(),
        to_mbid: to_mbid.to_owned(),
    })
}

/// Split a URL into (scheme, host, port, path) for origin checks. Small and
/// strict: only http/https URLs with a host parse.
fn split_origin(url: &str) -> Option<(String, String, Option<u16>, String)> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "http" && scheme != "https" {
        return None;
    }
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let (authority, path) = rest.split_at(authority_end);
    if authority.contains('@') || authority.is_empty() {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port_text)) if !port_text.contains(']') => {
            let port: u16 = port_text.parse().ok()?;
            (host, Some(port))
        }
        _ => (authority, None),
    };
    if host.is_empty() {
        return None;
    }
    Some((
        scheme.to_owned(),
        host.to_ascii_lowercase(),
        port,
        path.to_owned(),
    ))
}

/// Resolve a Location against the request URL (absolute form, or a path on
/// the same origin). Anything else is unresolvable and the 3xx is rejected.
fn resolve_location(request_url: &str, location: &str) -> Option<String> {
    let trimmed = location.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return Some(trimmed.to_owned());
    }
    if !trimmed.starts_with('/') {
        return None;
    }
    let (scheme, host, port, _) = split_origin(request_url)?;
    let authority = match port {
        Some(port) => format!("{host}:{port}"),
        None => host,
    };
    Some(format!("{scheme}://{authority}{trimmed}"))
}

/// Best-effort parse of an official-mode 3xx Location into a hop triple:
/// resolve against the request URL, same-origin check against the
/// attempt's source, then entity/UUID validation (v2
/// `_official_redirect_pair`). Unparseable input returns `None` and the
/// 3xx raises as usual.
pub fn official_redirect_hop(
    request_url: &str,
    source_base: &str,
    request_path: &str,
    location: &str,
) -> Option<RedirectHop> {
    let target = resolve_location(request_url, location)?;
    let (target_scheme, target_host, target_port, target_path) = split_origin(&target)?;
    let normalized_base = source_base.trim_end_matches('/');
    let (base_scheme, base_host, base_port, base_path) = split_origin(normalized_base)?;
    if target_scheme != base_scheme || target_host != base_host || target_port != base_port {
        return None;
    }
    let suffix = target_path.strip_prefix(&format!("{base_path}/"))?;
    lookup_redirect_pair(request_path, &format!("/{suffix}"))
}

/// BrainzMash host allowlist: exactly this hostname, nothing else.
pub const BRAINZMASH_HOST: &str = "api.brainzmash.cc";

/// Entity paths the application uses on BrainzMash (v2
/// `_BRAINZMASH_ENTITY_PATHS`).
const BRAINZMASH_ENTITY_PATHS: [&str; 6] = [
    "artist",
    "release-group",
    "release",
    "recording",
    "isrc",
    "url",
];

/// Validate the one server-owned BrainzMash origin and return its base URL
/// (v2 `validate_brainzmash_url`): https, exact host, no port, no
/// credentials, no query or fragment, exactly `/ws/2`.
pub fn validate_brainzmash_url(url: &str) -> Result<&str, MbError> {
    let rejected = || {
        MbError::Misconfigured(
            "brainzmash endpoint must be the approved HTTPS /ws/2 origin".to_owned(),
        )
    };
    let (scheme, host, port, path) = split_origin(url).ok_or_else(rejected)?;
    if scheme != "https" || host != BRAINZMASH_HOST || port.is_some() {
        return Err(rejected());
    }
    if path.contains(['?', '#', '%']) || path.trim_end_matches('/') != "/ws/2" {
        return Err(rejected());
    }
    Ok(BRAINZMASH_API_BASE)
}

/// Allow only the MusicBrainz WS/2 entity paths this application uses (v2
/// `validate_brainzmash_path`): one or two segments, allowlisted entity,
/// and an ASCII-alphanumeric-and-dash second segment (MBIDs qualify).
pub fn validate_brainzmash_path(path: &str) -> Result<String, MbError> {
    let rejected = || MbError::Misconfigured(format!("invalid brainzmash API path: {path}"));
    if !path.starts_with('/') || path.contains(['\\', '%', '?', '#']) || path.contains("//") {
        return Err(rejected());
    }
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    if segments.len() > 2 || !BRAINZMASH_ENTITY_PATHS.contains(&segments[0]) {
        return Err(rejected());
    }
    if segments.len() == 2 {
        let leaf = segments[1];
        if leaf.is_empty()
            || leaf == "."
            || leaf == ".."
            || !leaf
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(rejected());
        }
    }
    Ok(format!("/{}", segments.join("/")))
}

/// Reject request authority/path changes before a BrainzMash wire attempt
/// (v2 `validate_brainzmash_request_url`): the approved origin, no port or
/// credentials, no fragment, and a `/ws/2/` path. The query string (v2
/// validates `urlsplit(...).path`, so `?fmt=json` Locations pass) is
/// ignored for path validation.
pub fn validate_brainzmash_request_url(url: &str) -> Result<(), MbError> {
    let rejected =
        || MbError::Misconfigured("brainzmash request authority is not approved".to_owned());
    let (scheme, host, port, path) = split_origin(url).ok_or_else(rejected)?;
    if scheme != "https" || host != BRAINZMASH_HOST || port.is_some() {
        return Err(rejected());
    }
    if path.contains('#') {
        return Err(rejected());
    }
    let path_only = path.split('?').next().unwrap_or("");
    if !path_only.starts_with("/ws/2/") {
        return Err(rejected());
    }
    validate_brainzmash_path(&path_only["/ws/2".len()..])?;
    Ok(())
}

/// Validated `/ws/2` path for one same-origin BrainzMash redirect hop.
/// Probed live 2026-09-12 against api.brainzmash.cc: fetching merged
/// release `77a698a8-...` answers 301 with a same-origin
/// `/ws/2/release/<survivor>?fmt=json` Location, and that hop answers 200
/// with the surviving release (v2 `_validated_brainzmash_redirect_path`).
/// The Location resolves against the approved origin before validation, so
/// a foreign host, scheme downgrade, port shift, or off-`/ws/2` path still
/// returns `None` and the 3xx falls through to rejection.
pub fn brainzmash_redirect_path(
    request_url: &str,
    status: u16,
    location: Option<&str>,
) -> Option<String> {
    if !(300..400).contains(&status) {
        return None;
    }
    let resolved = resolve_location(request_url, location?)?;
    validate_brainzmash_request_url(&resolved).ok()?;
    let (_, _, _, path) = split_origin(&resolved)?;
    let path_only = path.split('?').next().unwrap_or("");
    validate_brainzmash_path(&path_only["/ws/2".len()..]).ok()
}
