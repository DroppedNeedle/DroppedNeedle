//! One line of a ListenBrainz export's `listens/<year>/<month>.jsonl`.
//!
//! An export is a ZIP a person downloads from ListenBrainz (or the server
//! fetches for them later): one JSON object a line, one file a month. The
//! archive itself is read in media; this is only the line's shape.
//!
//! Verified against an export made at listenbrainz.org on 2026-11-22 (read
//! on its owner's machine and not kept): `listened_at` is a float of whole
//! seconds, `inserted_at` an
//! integer; `track_name`, `artist_name` and `release_name` are on every
//! line; `mbid_mapping` is null on some lines and holds `recording_mbid`,
//! `release_mbid` and `artist_mbids` where present; `additional_info`
//! carries the submitter's `recording_mbid`, `release_mbid` and
//! `artist_mbids` on some lines, once a `release_group_mbid`. The
//! repository's `tests/fixtures/listenbrainz/history_export.zip` holds
//! invented listens in that shape.

use serde::de::IgnoredAny;
use serde::{Deserialize, Deserializer};

use crate::musicbrainz::is_valid_mbid;

/// The longest track, artist or release name a listen keeps, in characters.
const MAX_NAME_CHARS: usize = 1024;

/// One export line as written. Unknown fields are skipped; the time and the
/// two names are required, so a line without them fails decoding.
#[derive(Debug, Clone, Deserialize)]
pub struct ExportLine {
    /// Listen time, unix seconds, as an integer or a float.
    #[serde(deserialize_with = "seconds")]
    pub listened_at: i64,
    /// What was played.
    pub track_metadata: ExportTrack,
}

/// What a line says was played.
#[derive(Debug, Clone, Deserialize)]
pub struct ExportTrack {
    /// Track title, as the client submitted it.
    pub track_name: String,
    /// Artist name, as the client submitted it.
    pub artist_name: String,
    /// Release title.
    #[serde(default, deserialize_with = "loose_text")]
    pub release_name: Option<String>,
    /// The submitter's extras, free-form upstream.
    #[serde(default, deserialize_with = "loose_ids")]
    pub additional_info: Option<ExportIds>,
    /// ListenBrainz's own MusicBrainz mapping, when it has one.
    #[serde(default, deserialize_with = "loose_ids")]
    pub mbid_mapping: Option<ExportIds>,
}

/// The MusicBrainz ids a mapping or the submitter's extras carry. A value
/// that is not text, or not a list of text, reads as absent.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct ExportIds {
    /// Recording MBID.
    #[serde(default, deserialize_with = "loose_text")]
    pub recording_mbid: Option<String>,
    /// Release MBID.
    #[serde(default, deserialize_with = "loose_text")]
    pub release_mbid: Option<String>,
    /// Artist MBIDs, in credit order.
    #[serde(default, deserialize_with = "loose_list")]
    pub artist_mbids: Vec<String>,
}

/// One listen from an export, checked: names non-blank and of a sane
/// length, ids well formed and lowercased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryListen {
    /// Listen time, unix seconds.
    pub listened_at: i64,
    /// Track title.
    pub track_name: String,
    /// Artist name.
    pub artist_name: String,
    /// Release title.
    pub release_name: Option<String>,
    /// Recording MBID: the mapping's, else the submitter's.
    pub recording_mbid: Option<String>,
    /// Release MBID: the mapping's, else the submitter's.
    pub release_mbid: Option<String>,
    /// The first artist MBID: the mapping's, else the submitter's.
    pub artist_mbid: Option<String>,
}

/// Why a line was not kept.
#[derive(Debug, thiserror::Error)]
pub enum LineError {
    /// The line is not one listen: bad JSON or a missing time or name.
    #[error("the line is not a listen: {0}")]
    Shape(#[from] serde_json::Error),
    /// A name is blank or over the length a listen keeps.
    #[error("the line names no track or artist, or a name is too long")]
    Names,
}

impl ExportLine {
    /// The checked listen, or why it is not one.
    pub fn listen(self) -> Result<HistoryListen, LineError> {
        let track = self.track_metadata;
        let track_name = name(track.track_name).ok_or(LineError::Names)?;
        let artist_name = name(track.artist_name).ok_or(LineError::Names)?;
        let release_name = match track.release_name {
            Some(release) => Some(name(release).ok_or(LineError::Names)?),
            None => None,
        };
        let mapping = track.mbid_mapping.unwrap_or_default();
        let extra = track.additional_info.unwrap_or_default();
        Ok(HistoryListen {
            listened_at: self.listened_at,
            track_name,
            artist_name,
            release_name,
            recording_mbid: mbid(mapping.recording_mbid).or_else(|| mbid(extra.recording_mbid)),
            release_mbid: mbid(mapping.release_mbid).or_else(|| mbid(extra.release_mbid)),
            artist_mbid: mbid(mapping.artist_mbids.into_iter().next())
                .or_else(|| mbid(extra.artist_mbids.into_iter().next())),
        })
    }
}

/// Decode and check one export line.
pub fn parse_line(line: &str) -> Result<HistoryListen, LineError> {
    serde_json::from_str::<ExportLine>(line)?.listen()
}

/// A trimmed name, or `None` when blank or too long.
fn name(text: String) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty() && trimmed.chars().count() <= MAX_NAME_CHARS).then(|| trimmed.to_owned())
}

/// A well-formed MBID, lowercased; anything else reads as absent.
fn mbid(text: Option<String>) -> Option<String> {
    text.map(|text| text.trim().to_ascii_lowercase())
        .filter(|text| is_valid_mbid(text))
}

/// Whole seconds from an integer or a finite float. A fraction is dropped:
/// every export seen held whole seconds.
fn seconds<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Time {
        Int(i64),
        Float(f64),
    }
    match Time::deserialize(deserializer)? {
        Time::Int(seconds) => Ok(seconds),
        Time::Float(seconds) if seconds.is_finite() && seconds.abs() < 1e15 => {
            Ok(seconds.floor() as i64)
        }
        Time::Float(_) => Err(serde::de::Error::custom("listened_at is out of range")),
    }
}

/// Text where anything but a string reads as absent.
fn loose_text<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Loose {
        Text(String),
        Other(IgnoredAny),
    }
    Ok(match Loose::deserialize(deserializer)? {
        Loose::Text(text) => Some(text),
        Loose::Other(_) => None,
    })
}

/// A list of strings where anything else reads as empty.
fn loose_list<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<String>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Loose {
        List(Vec<String>),
        Other(IgnoredAny),
    }
    Ok(match Loose::deserialize(deserializer)? {
        Loose::List(list) => list,
        Loose::Other(_) => Vec::new(),
    })
}

/// An id object where anything but an object reads as absent.
fn loose_ids<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<ExportIds>, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Loose {
        Ids(ExportIds),
        Other(IgnoredAny),
    }
    Ok(match Loose::deserialize(deserializer)? {
        Loose::Ids(ids) => Some(ids),
        Loose::Other(_) => None,
    })
}
