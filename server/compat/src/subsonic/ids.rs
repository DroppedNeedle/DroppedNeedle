//! Type-prefixed Subsonic ids, as v2 spells them.
//!
//! Internal ids: artist = artist_mbid, album = rg_mbid, track = file_id,
//! playlist = playlist_id, genre = slug. Unknown prefix decodes to error 70.

use super::error::{NOT_FOUND, SubsonicError};

/// Id kinds, by prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdKind {
    /// `ar-` + artist_mbid.
    Artist,
    /// `al-` + rg_mbid.
    Album,
    /// `tr-` + file_id.
    Track,
    /// `pl-` + playlist_id.
    Playlist,
    /// `ge-` + slug.
    Genre,
}

impl IdKind {
    /// The wire prefix, with dash.
    pub fn prefix(self) -> &'static str {
        match self {
            Self::Artist => "ar-",
            Self::Album => "al-",
            Self::Track => "tr-",
            Self::Playlist => "pl-",
            Self::Genre => "ge-",
        }
    }

    /// The lowercase kind name used in "Expected a {kind} id" errors.
    pub fn name(self) -> &'static str {
        match self {
            Self::Artist => "artist",
            Self::Album => "album",
            Self::Track => "track",
            Self::Playlist => "playlist",
            Self::Genre => "genre",
        }
    }
}

/// Encode an internal id with its kind prefix.
pub fn encode(kind: IdKind, internal_id: &str) -> String {
    format!("{}{internal_id}", kind.prefix())
}

/// Decode a prefixed id into (kind, internal id); unknown prefix -> 70.
pub fn decode(sid: &str) -> Result<(IdKind, String), SubsonicError> {
    for kind in [
        IdKind::Artist,
        IdKind::Album,
        IdKind::Track,
        IdKind::Playlist,
        IdKind::Genre,
    ] {
        if let Some(rest) = sid.strip_prefix(kind.prefix()) {
            return Ok((kind, rest.to_owned()));
        }
    }
    Err(SubsonicError::new(NOT_FOUND, format!("Invalid id: {sid}")))
}

/// Decode and require a specific kind, else 70 ("Expected a {kind} id").
pub fn decode_expect(sid: &str, kind: IdKind) -> Result<String, SubsonicError> {
    let (found, internal) = decode(sid)?;
    if found != kind {
        return Err(SubsonicError::new(
            NOT_FOUND,
            format!("Expected a {} id", kind.name()),
        ));
    }
    Ok(internal)
}
