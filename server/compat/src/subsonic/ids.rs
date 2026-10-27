//! Type-prefixed Subsonic ids, as v2 spells them.
//!
//! Internal ids are local library ids for artists, albums and tracks, the
//! playlist id and the genre slug. Unknown prefix decodes to error 70.
//!
//! An album's cover id is its id with the cover version after an
//! underscore, `al-<id>_<version>`, as Navidrome sends it (see
//! [`cover_id`]). Every other place an album id goes out stays `al-<id>`.
//! Decoding keeps the whole id: an alias v2 handed out is free text and may
//! end the same way. Resolving an album id tries it whole first, then
//! without the version (see [`without_cover_version`]), so a cover id with
//! an old version, or with none, names the album and opens its current
//! picture.

use super::error::{NOT_FOUND, SubsonicError};

/// Id kinds, by prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdKind {
    /// `ar-` + local artist id.
    Artist,
    /// `al-` + local album id.
    Album,
    /// `tr-` + local track id.
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

/// An album's cover id: `al-<id>_<version>`, or `al-<id>` with no version.
pub fn cover_id(album_id: &str, version: Option<&str>) -> String {
    match version {
        Some(version) => format!("{}{album_id}_{version}", IdKind::Album.prefix()),
        None => encode(IdKind::Album, album_id),
    }
}

/// An album id without the cover version [`cover_id`] adds: an underscore
/// and exactly eight lowercase hex digits at the end. `None` when it ends
/// otherwise.
pub fn without_cover_version(album_id: &str) -> Option<&str> {
    let (id, version) = album_id.rsplit_once('_')?;
    let is_version = version.len() == 8
        && version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    (is_version && !id.is_empty()).then_some(id)
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
