//! YouTube Data API wire shapes, read by the client in `youtube.rs`.
//!
//! Tolerant of unknown fields. Each answer's list (`items`) is required: a
//! body without it is not that answer. A video id decodes only when it is
//! made of letters, digits, `-` and `_`, because it goes into an embed URL.

use serde::{Deserialize, Deserializer};

/// The answer to a search with `part=id`, `type=video` and `maxResults=1`.
///
/// Verified against www.googleapis.com/youtube/v3/search on 2026-10-29: tests/fixtures/youtube/search_found.json.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SearchList {
    /// The results. Required: a body without the list is not a search answer.
    pub(super) items: Vec<SearchResult>,
}

/// One search result.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SearchResult {
    /// The id block. Required: the search asks for videos only, so a result
    /// with no video id is not a search answer.
    pub(super) id: SearchResultId,
}

/// A search result's id block.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SearchResultId {
    /// The video id. Required: identity fields fail decoding when missing.
    #[serde(rename = "videoId")]
    pub(super) video_id: VideoId,
}

/// The answer to `videos.list` with `part=id`.
///
/// Verified against www.googleapis.com/youtube/v3/videos on 2026-10-29: tests/fixtures/youtube/videos_found.json.
/// An id that names no video answers an empty list (videos_unknown_id.json, 2026-10-29).
#[derive(Debug, Clone, Deserialize)]
pub(super) struct VideoList {
    /// The videos the request named that exist.
    pub(super) items: Vec<VideoItem>,
}

/// One video. Its id is required.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct VideoItem {
    /// The video id.
    pub(super) id: VideoId,
}

/// The error body of a refused request.
///
/// Verified against www.googleapis.com/youtube/v3/search on 2026-10-29: tests/fixtures/youtube/search_invalid_key.json (400, reason `badRequest`).
/// Verified against www.googleapis.com/youtube/v3/videos on 2026-10-29: tests/fixtures/youtube/videos_forbidden_part.json (403, reason `forbidden`).
#[derive(Debug, Clone, Deserialize)]
pub(super) struct GoogleErrorAnswer {
    /// The error object.
    pub(super) error: GoogleErrorDetail,
}

/// The error object.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct GoogleErrorDetail {
    /// One entry per reason. Empty when the body names none.
    #[serde(default)]
    pub(super) errors: Vec<ErrorReason>,
}

/// One reason an error names.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct ErrorReason {
    /// The reason string, such as `forbidden` or `quotaExceeded`.
    #[serde(default)]
    pub(super) reason: Option<String>,
}

/// A video id as an answer names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VideoId(String);

impl VideoId {
    /// The id text.
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }

    /// The id text, owned.
    pub(super) fn into_string(self) -> String {
        self.0
    }
}

impl<'de> Deserialize<'de> for VideoId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        let url_safe = text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if text.is_empty() || !url_safe {
            return Err(serde::de::Error::custom("not a video id"));
        }
        Ok(Self(text))
    }
}

#[cfg(test)]
mod tests {
    use super::VideoId;

    #[test]
    fn only_url_safe_video_ids_decode() {
        assert!(serde_json::from_str::<VideoId>(r#""hd9OYCqBK88""#).is_ok());
        for bad in [r#""""#, r#""a/b""#, r#""a?v=b""#, r#""a b""#, "12"] {
            assert!(serde_json::from_str::<VideoId>(bad).is_err(), "{bad}");
        }
    }
}
