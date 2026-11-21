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
    /// True when the record's lyricsfile carries word timings.
    #[serde(default, rename = "hasWordSync")]
    pub has_word_sync: bool,
    /// The record as a Lyricsfile: YAML text, read as [`Lyricsfile`].
    #[serde(default)]
    pub lyricsfile: Option<String>,
}

/// A Lyricsfile document, the fields read from it. Unknown fields
/// (`metadata`, `plain`, anything newer) are tolerated.
///
/// Verified against the Lyricsfile 1.0 draft
/// (github.com/tranxuanthang/lyricsfile, SPECIFICATION.md) and LRCLIB's own
/// reader (github.com/tranxuanthang/lrclib, `core/src/lyricsfile.rs`) on
/// 2026-11-21, and lrclib.net/api/get/36210 on 2026-11-21:
/// tests/fixtures/lrclib/get_found_words.json. `version` is required and
/// read as text; LRCLIB writes `'1.0'`.
#[derive(Debug, Deserialize)]
pub struct Lyricsfile {
    /// Format version; only `"1.0"` is read.
    pub version: String,
    /// Timed lines. Absent or null means none.
    #[serde(default)]
    pub lines: Option<Vec<LyricsfileLine>>,
}

/// One timed line of a Lyricsfile.
#[derive(Debug, Deserialize)]
pub struct LyricsfileLine {
    /// The whole line, for readers without word timing.
    pub text: String,
    /// Start in milliseconds.
    pub start_ms: i64,
    /// End in milliseconds, when given.
    #[serde(default)]
    pub end_ms: Option<i64>,
    /// Word or syllable timings. Joined in order they give the line's text,
    /// spaces included in each word's text. Absent or null means none.
    #[serde(default)]
    pub words: Option<Vec<LyricsfileWord>>,
}

/// One timed word or syllable of a Lyricsfile line.
#[derive(Debug, Deserialize)]
pub struct LyricsfileWord {
    /// The word, with any space that follows it.
    pub text: String,
    /// Start in milliseconds.
    pub start_ms: i64,
    /// End in milliseconds, when given.
    #[serde(default)]
    pub end_ms: Option<i64>,
}
