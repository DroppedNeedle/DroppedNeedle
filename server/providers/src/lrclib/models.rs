//! LRCLIB wire shapes.
//!
//! Tolerant of unknown fields and of missing lyrics, and strict on identity:
//! an answer without its `id` fails decoding.

use serde::Deserialize;

/// The record the exact lookup answers with: one track and its lyrics.
///
/// Verified against lrclib.net/api/get on 2026-10-20: tests/fixtures/lrclib/get_found.json.
#[derive(Debug, Deserialize)]
pub struct Lyrics {
    /// LRCLIB lyric id, the record's identity. A missing one fails decoding;
    /// a zero or negative one fails the completeness gate.
    pub id: i64,
    /// Track title.
    #[serde(default, rename = "trackName")]
    pub track_name: String,
    /// Track artist.
    #[serde(default, rename = "artistName")]
    pub artist_name: String,
    /// Album title.
    #[serde(default, rename = "albumName")]
    pub album_name: String,
    /// Track duration in seconds.
    #[serde(default)]
    pub duration: f64,
    /// True when the track has no lyrics at all.
    #[serde(default)]
    pub instrumental: bool,
    /// Plain lyrics text.
    #[serde(default, rename = "plainLyrics")]
    pub plain_lyrics: Option<String>,
    /// Synced (LRC) lyrics text.
    #[serde(default, rename = "syncedLyrics")]
    pub synced_lyrics: Option<String>,
}
