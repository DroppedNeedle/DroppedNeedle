//! Local library reads behind the catalog pages, on the local catalog:
//! which MusicBrainz ids the library holds, which albums have open
//! requests, edition evidence, and the local copy a page falls back to when
//! MusicBrainz is down.
//!
//! Every read is an indexed lookup over the request pool, so these parts of
//! a page stay inside the local read budget whatever the providers do.
//! Ids compare lowercase: MusicBrainz always sends lowercase, tags may not.

use std::collections::HashSet;

use droppedneedle_kernel::db::statement::ListLen;
use droppedneedle_library::identity::DecisionSource;
use droppedneedle_library::identity::reads::{Pick, copy_for_group};
use rusqlite::types::Value;

use crate::reads::library::sql;
use crate::reads::library::store::{first, rows, texts, track_row};
use crate::reads::library::stores::{StoreError, TrackRecord};

pub use crate::reads::library::store::LocalCatalog;

/// Most ids bound into one `IN (...)` list.
const MAX_BINDS: usize = 500;

/// A local artist row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalArtist {
    /// Display name.
    pub name: String,
}

/// A local album with its indexed tracks.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalAlbum {
    /// Album title.
    pub title: String,
    /// Album artist name.
    pub artist_name: String,
    /// Album artist MBID, when identified.
    pub artist_mbid: Option<String>,
    /// Year.
    pub year: Option<i32>,
    /// Whether the library owns this copy: it has at least one indexed
    /// track.
    pub owned: bool,
    /// Indexed tracks in disc and track order.
    pub tracks: Vec<LocalTrack>,
}

/// One indexed local track, as the catalog pages show it.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalTrack {
    /// Disc number.
    pub disc_number: u32,
    /// Track number.
    pub track_number: u32,
    /// Title.
    pub title: String,
    /// Length in milliseconds.
    pub length_ms: Option<u64>,
    /// Recording MBID, when identified.
    pub recording_mbid: Option<String>,
}

impl From<TrackRecord> for LocalTrack {
    fn from(track: TrackRecord) -> Self {
        Self {
            disc_number: u32::try_from(track.disc_number.max(1)).unwrap_or(1),
            track_number: u32::try_from(track.track_number.max(0)).unwrap_or(0),
            title: track.title,
            length_ms: track
                .duration_seconds
                .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                .map(|seconds| (seconds * 1000.0).round() as u64),
            recording_mbid: track.recording_mbid,
        }
    }
}

/// One local album of an artist, for the discography fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalArtistAlbum {
    /// Release-group MBID.
    pub release_group_mbid: String,
    /// Title.
    pub title: String,
    /// Year.
    pub year: Option<i32>,
}

/// What the library knows about which edition of an album it holds.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditionEvidence {
    /// Release MBID the library's copy is identified as.
    pub owned_release: Option<String>,
    /// Release MBID a person chose for the library's copy.
    pub chosen_release: Option<String>,
    /// Indexed files in the library's copy, when it has one.
    pub file_count: Option<u32>,
}

fn lowered(ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.iter()
        .map(|id| id.trim().to_ascii_lowercase())
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect()
}

/// The `IN (...)` statements the id-set reads run, one per chunk.
#[derive(Clone, Copy)]
enum IdSet {
    /// Owned albums by release group.
    OwnedGroups,
    /// Owned albums by release.
    OwnedReleases,
    /// Owned artists.
    OwnedArtists,
    /// Albums with an open request.
    Requested,
}

impl LocalCatalog {
    /// The lowercase ids among `ids` one id-set read finds, one read per
    /// chunk.
    async fn collect_ids(&self, set: IdSet, ids: &[String]) -> Result<HashSet<String>, StoreError> {
        let mut found = HashSet::new();
        for chunk in lowered(ids).chunks(MAX_BINDS) {
            let values = texts(chunk);
            let chunk_found: Vec<String> = self
                .read("catalog.library.ids", move |conn| {
                    let len = ListLen::new(values.len());
                    // The owners statements answer the album id first and
                    // the matched id second.
                    let (text, column) = match set {
                        IdSet::OwnedGroups => (sql::owners_by_group(len), 1),
                        IdSet::OwnedReleases => (sql::owners_by_release(len), 1),
                        IdSet::OwnedArtists => (sql::owned_artists_in(len), 0),
                        IdSet::Requested => (sql::requested_in(len), 0),
                    };
                    rows(conn, &text, &values, |row| row.get::<_, String>(column))
                })
                .await?;
            found.extend(chunk_found.into_iter().map(|id| id.to_ascii_lowercase()));
        }
        Ok(found)
    }

    /// Every identified artist credited on an owned album: (lowercase MBID,
    /// display name), by name.
    pub async fn identified_artists(&self) -> Result<Vec<(String, String)>, StoreError> {
        self.read("catalog.library.identified_artists", |conn| {
            rows(conn, sql::IDENTIFIED_ARTISTS.text(), &[], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
        })
        .await
    }

    /// Every identified owned album: (lowercase release-group MBID, title,
    /// album artist name).
    pub async fn identified_albums(&self) -> Result<Vec<(String, String, String)>, StoreError> {
        self.read("catalog.library.identified_albums", |conn| {
            rows(conn, sql::IDENTIFIED_ALBUMS.text(), &[], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
        })
        .await
    }

    /// The artist MBIDs (lowercase) the library holds: owned artists, live
    /// and credited on an owned album (v2
    /// `target_provider_artist_relationship`, owned half).
    pub async fn owned_artists(&self, mbids: &[String]) -> Result<HashSet<String>, StoreError> {
        self.collect_ids(IdSet::OwnedArtists, mbids).await
    }

    /// Whether the artist only appears on the library's tracks (a featured
    /// or guest credit on someone else's album) without an album of their
    /// own (v2 `target_provider_artist_relationship`, appears half).
    pub async fn artist_appears(&self, mbid: &str) -> Result<bool, StoreError> {
        let values = vec![Value::Text(mbid.to_ascii_lowercase())];
        let found: Option<i64> = self
            .read("catalog.library.artist_appears", move |conn| {
                first(conn, sql::ARTIST_APPEARS.text(), &values, |row| row.get(0))
            })
            .await?;
        Ok(found.is_some())
    }

    /// The release-group MBIDs (lowercase) of owned albums. A release MBID
    /// passed in also matches, returning the release MBID, so callers
    /// holding either id get an answer.
    pub async fn owned_albums(&self, mbids: &[String]) -> Result<HashSet<String>, StoreError> {
        let by_group = self.collect_ids(IdSet::OwnedGroups, mbids).await?;
        let by_release = self.collect_ids(IdSet::OwnedReleases, mbids).await?;
        Ok(by_group.union(&by_release).cloned().collect())
    }

    /// Every owned release-group MBID (lowercase, sorted): the unfiltered
    /// form of [`Self::owned_albums`].
    pub async fn all_owned_release_groups(&self) -> Result<Vec<String>, StoreError> {
        self.read("catalog.library.owned_groups", |conn| {
            rows(conn, sql::OWNED_GROUPS.text(), &[], |row| row.get(0))
        })
        .await
    }

    /// Every album MBID (lowercase, sorted) with an open acquisition
    /// request: the unfiltered form of [`Self::requested_albums`].
    pub async fn all_requested_albums(&self) -> Result<Vec<String>, StoreError> {
        self.read("catalog.library.requested_groups", |conn| {
            rows(conn, sql::REQUESTED_GROUPS.text(), &[], |row| row.get(0))
        })
        .await
    }

    /// The album MBIDs (lowercase) with an open acquisition request.
    pub async fn requested_albums(&self, mbids: &[String]) -> Result<HashSet<String>, StoreError> {
        self.collect_ids(IdSet::Requested, mbids).await
    }

    /// The live local artist behind one MBID.
    pub async fn artist(&self, mbid: &str) -> Result<Option<LocalArtist>, StoreError> {
        let values = vec![Value::Text(mbid.to_ascii_lowercase())];
        self.read("catalog.library.artist", move |conn| {
            first(conn, sql::LOCAL_ARTIST.text(), &values, |row| {
                Ok(LocalArtist { name: row.get(0)? })
            })
        })
        .await
    }

    /// The identified owned albums led by one artist MBID, newest first.
    pub async fn artist_albums(&self, mbid: &str) -> Result<Vec<LocalArtistAlbum>, StoreError> {
        let values = vec![Value::Text(mbid.to_ascii_lowercase())];
        self.read("catalog.library.artist_albums", move |conn| {
            rows(conn, sql::LOCAL_ARTIST_ALBUMS.text(), &values, |row| {
                Ok(LocalArtistAlbum {
                    release_group_mbid: row.get(0)?,
                    title: row.get(1)?,
                    year: row.get(2)?,
                })
            })
        })
        .await
    }

    /// The local copy of one release group, live with or without files
    /// (the oldest copy with files, else the oldest copy), with its
    /// indexed tracks.
    pub async fn album(&self, release_group_mbid: &str) -> Result<Option<LocalAlbum>, StoreError> {
        let values = vec![Value::Text(release_group_mbid.to_ascii_lowercase())];
        let found = self
            .read("catalog.library.album", move |conn| {
                first(conn, sql::LOCAL_ALBUM.text(), &values, |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        LocalAlbum {
                            title: row.get(1)?,
                            artist_name: row.get(2)?,
                            year: row.get(3)?,
                            artist_mbid: row.get(4)?,
                            owned: row.get(5)?,
                            tracks: Vec::new(),
                        },
                    ))
                })
            })
            .await?;
        let Some((album_id, mut album)) = found else {
            return Ok(None);
        };
        let values = vec![Value::Text(album_id)];
        let tracks = self
            .read("catalog.library.album.tracks", move |conn| {
                rows(conn, sql::LOCAL_ALBUM_TRACKS.text(), &values, track_row)
            })
            .await?;
        album.tracks = tracks.into_iter().map(LocalTrack::from).collect();
        Ok(Some(album))
    }

    /// The release group a library album (or the album it was merged
    /// into) is identified as.
    pub async fn group_for_album(&self, album_id: &str) -> Result<Option<String>, StoreError> {
        let values = vec![Value::Text(album_id.to_owned())];
        self.read("catalog.library.group_for_album", move |conn| {
            first(conn, sql::GROUP_FOR_ALBUM.text(), &values, |row| row.get(0))
        })
        .await
    }

    /// The library copy's edition (who chose it) and file count for one
    /// release group. Several copies read as a chosen one first, then the
    /// oldest, as acquisition does.
    pub async fn edition_evidence(
        &self,
        release_group_mbid: &str,
    ) -> Result<EditionEvidence, StoreError> {
        let release_group_mbid = release_group_mbid.to_owned();
        let copy = self
            .read("catalog.library.edition_evidence", move |conn| {
                copy_for_group(conn, &release_group_mbid, Pick::Shown)
            })
            .await?;
        let Some(copy) = copy else {
            return Ok(EditionEvidence::default());
        };
        Ok(EditionEvidence {
            chosen_release: copy
                .release_mbid
                .clone()
                .filter(|_| copy.source == DecisionSource::Manual),
            owned_release: copy.release_mbid,
            file_count: u32::try_from(copy.file_count)
                .ok()
                .filter(|count| *count > 0),
        })
    }
}
