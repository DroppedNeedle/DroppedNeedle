//! Internet Archive client: the Free Music download source.
//!
//! A Rust port of v2's Internet Archive repository, with the shapes pinned
//! by v2's repository tests and Archive mock. Only items
//! carrying an explicit Creative Commons or public-domain `licenseurl` are
//! surfaced; that filter is DroppedNeedle's own editorial rule for its own
//! client, not a rule imposed on anyone else.
//!
//! Transport rides the shared [`HttpPort`](super::client::HttpPort) GET port.
//! Rate limiting (2/s), retries, and caching stay with wiring.
//! Note the failure contract v2 keeps: a dead Archive raises rather than
//! degrading, because Free Music failing is a real failure the user is
//! waiting on, not a background enrichment. `Err` below therefore means
//! "tell the user", never "render without".
//!
//! Byte streaming (`stream_file` in v2) has no port here: it needs a
//! streaming body the shared `HttpPort` does not model, so it stays with a
//! future streaming port.

use serde::{Deserialize, Serialize};

use super::client::HttpPort;

/// Provider key used in logs and degradation records.
pub const PROVIDER_NAME: &str = "archive";
/// Live advanced-search endpoint.
pub const SEARCH_PATH: &str = "https://archive.org/advancedsearch.php";
/// Live metadata endpoint pattern (`{identifier}` substituted).
pub const METADATA_PATTERN: &str = "https://archive.org/metadata/{identifier}";
/// Live download endpoint pattern (for wiring's streaming port).
pub const DOWNLOAD_PATTERN: &str = "https://archive.org/download/{identifier}/{filename}";

/// What can go wrong on an Archive fetch.
#[derive(Debug, Clone, PartialEq)]
pub enum FetchError {
    /// The wire failed (retryable).
    Transport,
    /// The upstream answered 429; the caller backs off this long.
    RateLimited {
        /// Seconds to wait before retrying.
        retry_after_secs: f64,
    },
    /// Any other status or an undecodable body. Surfaced to the user.
    Unusable,
}

// ---------------------------------------------------------------------------
// Wire models (default-tolerant; unknown fields are ignored by serde)
// ---------------------------------------------------------------------------

/// One advanced-search hit on the wire. `creator` and `year` keep their raw
/// JSON values because live items vary: a creator can be one string or a
/// list naming the artist twice, and a year can be a string or a number.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireDoc {
    /// Item identifier.
    #[serde(default)]
    pub identifier: Option<String>,
    /// Item title.
    #[serde(default)]
    pub title: Option<String>,
    /// Creator string or list.
    #[serde(default)]
    pub creator: serde_json::Value,
    /// Year string or number.
    #[serde(default)]
    pub year: serde_json::Value,
    /// Licence URL.
    #[serde(default)]
    pub licenseurl: Option<String>,
}

/// The solr-style envelope on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireSearchResponse {
    /// Response envelope.
    #[serde(default)]
    pub response: WireSearchInner,
}

/// The inner `response` object on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireSearchInner {
    /// Found hits.
    #[serde(default)]
    pub docs: Vec<WireDoc>,
}

/// One metadata file entry on the wire.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireFile {
    /// File name.
    #[serde(default)]
    pub name: Option<String>,
    /// Archive format string ("VBR MP3", "Flac", ...).
    #[serde(default)]
    pub format: Option<String>,
    /// File size in bytes (string or number on the wire).
    #[serde(default)]
    pub size: serde_json::Value,
    /// Track number (string or number on the wire).
    #[serde(default)]
    pub track: serde_json::Value,
    /// Track title.
    #[serde(default)]
    pub title: Option<String>,
}

/// An item metadata answer on the wire. A dark or removed item answers `{}`,
/// so every field stays optional (v2 `get_item_files`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct WireMetadata {
    /// Item metadata block.
    #[serde(default)]
    pub metadata: serde_json::Value,
    /// File entries.
    #[serde(default)]
    pub files: Option<Vec<WireFile>>,
}

// ---------------------------------------------------------------------------
// Normalized models
// ---------------------------------------------------------------------------

/// One search hit, already licence-filtered.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveItem {
    /// Item identifier.
    pub identifier: String,
    /// Item title.
    pub title: String,
    /// Joined creator string.
    pub creator: String,
    /// Release year.
    pub year: Option<i64>,
    /// Licence URL.
    pub licence_url: String,
}

/// One importable audio file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchiveFile {
    /// File name.
    pub name: String,
    /// Lowercased Archive format string.
    pub format: String,
    /// File size in bytes.
    pub size_bytes: i64,
    /// Track number.
    pub track: Option<i64>,
    /// Track title.
    pub title: String,
}

// ---------------------------------------------------------------------------
// Pure normalization (ports of the v2 module-level helpers)
// ---------------------------------------------------------------------------

/// Licence prefixes this client accepts (v2 `_ALLOWED_LICENCE_PREFIXES`).
pub const ALLOWED_LICENCE_PREFIXES: &[&str] = &[
    "http://creativecommons.org/licenses/",
    "https://creativecommons.org/licenses/",
    "http://creativecommons.org/publicdomain/",
    "https://creativecommons.org/publicdomain/",
];

/// True only for an explicit Creative Commons or public-domain licence (v2
/// `is_open_licence`). The check is a lowercase prefix match, which is also
/// what rejects lookalike hosts such as
/// `https://evil.example/creativecommons.org/licenses/by/4.0/` (a v2 test case).
pub fn is_open_licence(licence_url: Option<&str>) -> bool {
    let value = licence_url.unwrap_or("").trim().to_lowercase();
    !value.is_empty()
        && ALLOWED_LICENCE_PREFIXES
            .iter()
            .any(|prefix| value.starts_with(prefix))
}

/// Escape a value for a Lucene phrase query (v2 `_escape`): backslashes and
/// double quotes are stripped.
pub fn escape_lucene(value: &str) -> String {
    value.replace(['\\', '"'], "")
}

/// Archive `format` strings for audio this client can import, mapped to a
/// file extension. Matching is case-insensitive (v2 `_AUDIO_FORMATS`).
pub fn extension_for(archive_format: &str) -> &'static str {
    match archive_format.to_lowercase().as_str() {
        "flac" | "24bit flac" => "flac",
        "vbr mp3" | "mp3" => "mp3",
        "ogg vorbis" => "ogg",
        _ => "",
    }
}

/// Read a v2-style digits-only year: numbers and digit strings convert,
/// anything else is absence (v2 `search_audio`).
pub fn year_from_json(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(number) => number.as_i64().filter(|year| *year >= 0),
        serde_json::Value::String(text) => {
            if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) {
                text.parse::<i64>().ok()
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Read a digits-only track number, same rules as the year (v2
/// `get_item_files`).
pub fn track_from_json(value: &serde_json::Value) -> Option<i64> {
    year_from_json(value)
}

/// Read a file size, defaulting to 0 when the wire value is missing or odd
/// (v2 `int(entry.get("size") or 0)`).
pub fn size_from_json(value: &serde_json::Value) -> i64 {
    match value {
        serde_json::Value::Number(number) => number.as_i64().unwrap_or(0).max(0),
        serde_json::Value::String(text) => text.trim().parse::<i64>().unwrap_or(0).max(0),
        _ => 0,
    }
}

/// Join a creator that may be one string or a list (v2 `search_audio` keeps
/// repeats: `["Brad Sucks", "Brad Sucks"]` joins with both halves).
pub fn creator_from_json(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

/// Normalize one search hit, dropping items without an identifier or an
/// open licence (v2 `search_audio`).
pub fn normalize_item(doc: &WireDoc) -> Option<ArchiveItem> {
    let identifier = doc.identifier.as_deref().unwrap_or("");
    if identifier.is_empty() || !is_open_licence(doc.licenseurl.as_deref()) {
        return None;
    }
    let title = doc
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
        .unwrap_or(identifier);
    Some(ArchiveItem {
        identifier: identifier.to_owned(),
        title: title.to_owned(),
        creator: creator_from_json(&doc.creator),
        year: year_from_json(&doc.year),
        licence_url: doc.licenseurl.clone().unwrap_or_default(),
    })
}

/// Normalize one file entry, keeping only importable audio (v2
/// `get_item_files`).
pub fn normalize_file(entry: &WireFile) -> Option<ArchiveFile> {
    let name = entry.name.as_deref().unwrap_or("");
    let format = entry.format.as_deref().unwrap_or("").to_lowercase();
    if name.is_empty() || extension_for(&format).is_empty() {
        return None;
    }
    Some(ArchiveFile {
        name: name.to_owned(),
        format,
        size_bytes: size_from_json(&entry.size),
        track: track_from_json(&entry.track),
        title: entry.title.clone().unwrap_or_default(),
    })
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Internet Archive client.
pub struct ArchiveClient<'h, H: HttpPort> {
    /// Transport port (scripted fake in tests, real HTTP in wiring).
    pub http: &'h H,
    /// Advanced-search endpoint; defaults to the live URL.
    pub search_url: String,
    /// Metadata endpoint pattern with `{identifier}`; defaults to live.
    pub metadata_pattern: String,
}

impl<'h, H: HttpPort> ArchiveClient<'h, H> {
    /// Build a client against the live endpoints.
    pub fn new(http: &'h H) -> Self {
        Self {
            http,
            search_url: SEARCH_PATH.to_owned(),
            metadata_pattern: METADATA_PATTERN.to_owned(),
        }
    }

    /// Build a client against scripted or mirrored endpoints.
    pub fn with_endpoints(http: &'h H, search_url: &str, metadata_pattern: &str) -> Self {
        Self {
            http,
            search_url: search_url.to_owned(),
            metadata_pattern: metadata_pattern.to_owned(),
        }
    }

    /// Licensed audio items matching artist + title (v2 `search_audio`).
    /// With neither artist nor title the query would be meaningless, so no
    /// request is made and the answer is empty.
    pub async fn search_audio(
        &self,
        artist: &str,
        title: &str,
        limit: u32,
    ) -> Result<Vec<ArchiveItem>, FetchError> {
        let mut clauses = vec![
            "mediatype:audio".to_owned(),
            "licenseurl:[* TO *]".to_owned(),
        ];
        if !artist.trim().is_empty() {
            clauses.push(format!(r#"creator:"{}""#, escape_lucene(artist)));
        }
        if !title.trim().is_empty() {
            clauses.push(format!(r#"title:"{}""#, escape_lucene(title)));
        }
        if clauses.len() == 2 {
            return Ok(Vec::new());
        }
        let query = clauses.join(" AND ");
        let rows = limit.clamp(1, 50).to_string();
        let reply = self
            .http
            .get(
                &self.search_url,
                &[
                    ("q", query.as_str()),
                    ("fl[]", "identifier"),
                    ("fl[]", "title"),
                    ("fl[]", "creator"),
                    ("fl[]", "licenseurl"),
                    ("fl[]", "year"),
                    ("rows", rows.as_str()),
                    ("output", "json"),
                ],
            )
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 429 {
            return Err(FetchError::RateLimited {
                retry_after_secs: retry_after_secs(reply.retry_after.as_deref()).unwrap_or(60.0),
            });
        }
        if reply.status != 200 {
            return Err(FetchError::Unusable);
        }
        let page: WireSearchResponse =
            serde_json::from_slice(&reply.body).map_err(|_| FetchError::Unusable)?;
        Ok(page
            .response
            .docs
            .iter()
            .filter_map(normalize_item)
            .collect())
    }

    /// `(licence_url, audio_files)` for an item (v2 `get_item_files`). A
    /// dark, removed, or closed-licence item answers `("", [])`: the Archive
    /// replies `{}` for those instead of a 404.
    pub async fn get_item_files(
        &self,
        identifier: &str,
    ) -> Result<(String, Vec<ArchiveFile>), FetchError> {
        let url = self.metadata_pattern.replace("{identifier}", identifier);
        let reply = self
            .http
            .get(&url, &[])
            .await
            .map_err(|_| FetchError::Transport)?;
        if reply.status == 429 {
            return Err(FetchError::RateLimited {
                retry_after_secs: retry_after_secs(reply.retry_after.as_deref()).unwrap_or(60.0),
            });
        }
        if reply.status != 200 {
            return Err(FetchError::Unusable);
        }
        let item: WireMetadata =
            serde_json::from_slice(&reply.body).map_err(|_| FetchError::Unusable)?;
        let licence = item
            .metadata
            .get("licenseurl")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        if !is_open_licence(Some(licence)) {
            return Ok((String::new(), Vec::new()));
        }
        let files = item
            .files
            .as_deref()
            .unwrap_or(&[])
            .iter()
            .filter_map(normalize_file)
            .collect();
        Ok((licence.to_owned(), files))
    }
}

/// Honor a positive `Retry-After` delay; missing, unparsable, or
/// non-positive values yield `None` so the caller falls back.
fn retry_after_secs(value: Option<&str>) -> Option<f64> {
    value
        .and_then(|raw| raw.trim().parse::<f64>().ok())
        .filter(|secs| secs.is_finite() && *secs > 0.0)
}
