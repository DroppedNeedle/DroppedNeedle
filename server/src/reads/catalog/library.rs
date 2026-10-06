//! Local library reads behind the catalog pages: which MusicBrainz ids the
//! library holds, which albums have open requests, edition evidence, and
//! the local copy a page falls back to when MusicBrainz is down.
//!
//! Every read is an indexed lookup over the reader pool, so these parts of
//! a page stay inside the local read budget whatever the providers do.
//! Ids compare lowercase: MusicBrainz always sends lowercase, tags may not.

use std::collections::HashSet;

use sqlx::{Row, SqlitePool};

/// Most ids bound into one `IN (...)` list.
const MAX_BINDS: usize = 500;

/// Request statuses that count as "requested" on catalog pages (open, not
/// finished): the same set the requests module dedupes against.
const OPEN_REQUEST_STATUSES: &str = "'pending','downloading','queued','awaiting_approval'";

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
    /// Indexed tracks in disc and track order.
    pub tracks: Vec<LocalTrack>,
}

/// One indexed local track.
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
    /// Release MBID a curator pinned.
    pub pinned_release: Option<String>,
    /// Indexed files in the library's copy, when it has one.
    pub file_count: Option<u32>,
}

/// Local catalog reads over one pool. Clone shares the pool.
#[derive(Debug, Clone)]
pub struct LocalCatalog {
    pool: SqlitePool,
}

fn placeholders(count: usize) -> String {
    vec!["?"; count].join(",")
}

fn lowered(ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.iter()
        .map(|id| id.trim().to_ascii_lowercase())
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect()
}

impl LocalCatalog {
    /// Read from this pool.
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Run one `IN (...)` query per chunk and gather the first column.
    async fn collect_ids(&self, sql: &str, ids: &[String]) -> Result<HashSet<String>, sqlx::Error> {
        let mut found = HashSet::new();
        for chunk in lowered(ids).chunks(MAX_BINDS) {
            let query = sql.replace("{ids}", &placeholders(chunk.len()));
            let mut statement = sqlx::query(&query);
            for id in chunk {
                statement = statement.bind(id);
            }
            for row in statement.fetch_all(&self.pool).await? {
                let id: String = row.try_get(0)?;
                found.insert(id.to_ascii_lowercase());
            }
        }
        Ok(found)
    }

    /// The artist MBIDs (lowercase) the library holds: credited on a live
    /// album with at least one indexed track (v2
    /// `target_provider_artist_relationship`, owned half).
    pub async fn owned_artists(&self, mbids: &[String]) -> Result<HashSet<String>, sqlx::Error> {
        self.collect_ids(
            "SELECT DISTINCT lower(e.provider_artist_id) FROM local_artist_external_identities e \
             JOIN local_artists a ON a.id = e.local_artist_id \
             JOIN local_album_artists credit ON credit.local_artist_id = e.local_artist_id \
             JOIN local_albums b ON b.id = credit.local_album_id \
             WHERE e.provider = 'musicbrainz' AND a.retired_into_artist_id IS NULL \
             AND b.retired_into_album_id IS NULL \
             AND lower(e.provider_artist_id) IN ({ids}) \
             AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = b.id \
             AND t.availability = 'indexed')",
            mbids,
        )
        .await
    }

    /// Whether the artist only appears on the library's tracks (a featured
    /// or guest credit on someone else's album) without an album of their
    /// own (v2 `target_provider_artist_relationship`, appears half).
    pub async fn artist_appears(&self, mbid: &str) -> Result<bool, sqlx::Error> {
        let row = sqlx::query(
            "SELECT 1 FROM local_artist_external_identities e \
             JOIN local_artists a ON a.id = e.local_artist_id \
             JOIN local_track_artists credit ON credit.local_artist_id = e.local_artist_id \
             JOIN local_tracks t ON t.id = credit.local_track_id \
             JOIN local_albums b ON b.id = t.local_album_id \
             WHERE e.provider = 'musicbrainz' AND lower(e.provider_artist_id) = ? \
             AND a.retired_into_artist_id IS NULL AND b.retired_into_album_id IS NULL \
             AND t.availability = 'indexed' AND NOT EXISTS ( \
             SELECT 1 FROM local_album_artists album_credit \
             WHERE album_credit.local_album_id = t.local_album_id \
             AND album_credit.local_artist_id = credit.local_artist_id) LIMIT 1",
        )
        .bind(mbid.to_ascii_lowercase())
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.is_some())
    }

    /// The release-group MBIDs (lowercase) the library holds with at least
    /// one indexed file. A release MBID passed in also matches, returning
    /// the release MBID, so callers holding either id get an answer.
    pub async fn owned_albums(&self, mbids: &[String]) -> Result<HashSet<String>, sqlx::Error> {
        let by_group = self
            .collect_ids(
                "SELECT lower(e.release_group_mbid) FROM local_album_external_identities e \
                 JOIN local_albums b ON b.id = e.local_album_id \
                 WHERE b.retired_into_album_id IS NULL \
                 AND lower(e.release_group_mbid) IN ({ids}) \
                 AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = b.id \
                 AND t.availability = 'indexed')",
                mbids,
            )
            .await?;
        let by_release = self
            .collect_ids(
                "SELECT lower(e.release_mbid) FROM local_album_external_identities e \
                 JOIN local_albums b ON b.id = e.local_album_id \
                 WHERE b.retired_into_album_id IS NULL AND e.release_mbid IS NOT NULL \
                 AND lower(e.release_mbid) IN ({ids}) \
                 AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = b.id \
                 AND t.availability = 'indexed')",
                mbids,
            )
            .await?;
        Ok(by_group.union(&by_release).cloned().collect())
    }

    /// The album MBIDs (lowercase) with an open acquisition request.
    pub async fn requested_albums(&self, mbids: &[String]) -> Result<HashSet<String>, sqlx::Error> {
        let sql = format!(
            "SELECT musicbrainz_id_lower FROM request_history \
             WHERE request_kind = 'album' AND status IN ({OPEN_REQUEST_STATUSES}) \
             AND musicbrainz_id_lower IN ({{ids}})"
        );
        self.collect_ids(&sql, mbids).await
    }

    /// The live local artist behind one MBID.
    pub async fn artist(&self, mbid: &str) -> Result<Option<LocalArtist>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT a.display_name FROM local_artist_external_identities e \
             JOIN local_artists a ON a.id = e.local_artist_id \
             WHERE e.provider = 'musicbrainz' AND a.retired_into_artist_id IS NULL \
             AND lower(e.provider_artist_id) = ? LIMIT 1",
        )
        .bind(mbid.to_ascii_lowercase())
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(LocalArtist {
                name: row.try_get(0)?,
            })
        })
        .transpose()
    }

    /// The identified local albums credited to one artist MBID, newest
    /// first.
    pub async fn artist_albums(&self, mbid: &str) -> Result<Vec<LocalArtistAlbum>, sqlx::Error> {
        let rows = sqlx::query(
            "SELECT e.release_group_mbid, b.title, b.year \
             FROM local_artist_external_identities ae \
             JOIN local_albums b ON b.album_artist_id = ae.local_artist_id \
             JOIN local_album_external_identities e ON e.local_album_id = b.id \
             WHERE ae.provider = 'musicbrainz' AND lower(ae.provider_artist_id) = ? \
             AND b.retired_into_album_id IS NULL \
             ORDER BY b.year IS NULL, b.year DESC, b.title_folded",
        )
        .bind(mbid.to_ascii_lowercase())
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok(LocalArtistAlbum {
                    release_group_mbid: row.try_get(0)?,
                    title: row.try_get(1)?,
                    year: row.try_get(2)?,
                })
            })
            .collect()
    }

    /// The local copy of one release group, with its indexed tracks.
    pub async fn album(&self, release_group_mbid: &str) -> Result<Option<LocalAlbum>, sqlx::Error> {
        let row = sqlx::query(
            "SELECT b.id, b.title, COALESCE(b.album_artist_name, ''), b.year, \
             (SELECT ae.provider_artist_id FROM local_artist_external_identities ae \
              WHERE ae.local_artist_id = b.album_artist_id AND ae.provider = 'musicbrainz') \
             FROM local_album_external_identities e \
             JOIN local_albums b ON b.id = e.local_album_id \
             WHERE b.retired_into_album_id IS NULL AND lower(e.release_group_mbid) = ? \
             ORDER BY b.created_at LIMIT 1",
        )
        .bind(release_group_mbid.to_ascii_lowercase())
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let album_id: String = row.try_get(0)?;
        let tracks = sqlx::query(
            "SELECT t.disc_number, t.track_number, t.title, t.duration_seconds, \
             e.recording_mbid FROM local_tracks t \
             LEFT JOIN local_track_external_identities e \
             ON e.local_track_id = t.id AND e.provider = 'musicbrainz' \
             WHERE t.local_album_id = ? AND t.availability = 'indexed' \
             ORDER BY t.disc_number, t.track_number, t.title_folded",
        )
        .bind(&album_id)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|track| {
            let disc: i64 = track.try_get(0)?;
            let number: i64 = track.try_get(1)?;
            let seconds: Option<f64> = track.try_get(3)?;
            Ok(LocalTrack {
                disc_number: u32::try_from(disc.max(1)).unwrap_or(1),
                track_number: u32::try_from(number.max(0)).unwrap_or(0),
                title: track.try_get(2)?,
                length_ms: seconds
                    .filter(|seconds| seconds.is_finite() && *seconds > 0.0)
                    .map(|seconds| (seconds * 1000.0).round() as u64),
                recording_mbid: track.try_get(4)?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
        Ok(Some(LocalAlbum {
            title: row.try_get(1)?,
            artist_name: row.try_get(2)?,
            year: row.try_get(3)?,
            artist_mbid: row.try_get(4)?,
            tracks,
        }))
    }

    /// The live library albums identified as one release group.
    pub async fn albums_for_group(
        &self,
        release_group_mbid: &str,
    ) -> Result<Vec<String>, sqlx::Error> {
        sqlx::query_scalar(
            "SELECT b.id FROM local_album_external_identities e \
             JOIN local_albums b ON b.id = e.local_album_id \
             WHERE b.retired_into_album_id IS NULL AND lower(e.release_group_mbid) = ? \
             ORDER BY b.created_at",
        )
        .bind(release_group_mbid.to_ascii_lowercase())
        .fetch_all(&self.pool)
        .await
    }

    /// Owned and pinned editions plus the file count for one release
    /// group. Several local copies of one group read as the first one.
    pub async fn edition_evidence(
        &self,
        release_group_mbid: &str,
    ) -> Result<EditionEvidence, sqlx::Error> {
        let row = sqlx::query(
            "SELECT e.release_mbid, p.release_mbid, \
             (SELECT COUNT(*) FROM local_tracks t WHERE t.local_album_id = b.id \
              AND t.availability = 'indexed') \
             FROM local_album_external_identities e \
             JOIN local_albums b ON b.id = e.local_album_id \
             LEFT JOIN library_album_release_pins p ON p.local_album_id = b.id \
             WHERE b.retired_into_album_id IS NULL AND lower(e.release_group_mbid) = ? \
             ORDER BY b.created_at LIMIT 1",
        )
        .bind(release_group_mbid.to_ascii_lowercase())
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Ok(EditionEvidence::default());
        };
        let files: i64 = row.try_get(2)?;
        Ok(EditionEvidence {
            owned_release: row.try_get(0)?,
            pinned_release: row.try_get(1)?,
            file_count: u32::try_from(files).ok().filter(|count| *count > 0),
        })
    }
}
