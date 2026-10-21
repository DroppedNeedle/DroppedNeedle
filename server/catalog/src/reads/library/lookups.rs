//! [`LibraryLookups`]: membership, identifier
//! resolution for album status and track resolution, and the stats extras.
//!
//! Reads only, over the reader pool. MusicBrainz ids compare lowercase
//! through the `lower(...)` expression indexes on the album identities.
//! Ownership and open requests reuse the catalog pages' definitions
//! ([`LocalCatalog`]) so a card and a membership check never disagree.

use std::collections::{HashMap, HashSet};

use sqlx::SqlitePool;

use super::definitions::{copy_order, live_album, musicbrainz, owned_album};
use super::sqlite::{TRACK_COLUMNS, TRACK_JOINS, in_placeholders, internal, map_track};
use super::stores::{AlbumMbids, StatsExtras, StoreError, TrackRecord};
use crate::reads::catalog::library::LocalCatalog;
use droppedneedle_library::corrections::continuation::CURRENT_TRACK;

/// Most ids bound into one `IN (...)` list.
const MAX_BINDS: usize = 500;

/// Review, local-only and last-scan totals in one round trip. Local-only
/// albums are owned albums with no identity row (v2 `local_only_count`);
/// the last scan is the newest completed run.
const STATS_EXTRAS: &str = concat!(
    "SELECT \
     (SELECT COUNT(*) FROM library_identify_reviews WHERE state = 'pending') AS reviews, \
     (SELECT COUNT(*) FROM local_albums a WHERE ",
    owned_album!("a"),
    " AND NOT EXISTS (SELECT 1 FROM local_album_external_identities i \
     WHERE i.local_album_id = a.id AND ",
    musicbrainz!("i"),
    ")) AS local_only, \
     (SELECT MAX(terminal_at) FROM library_scan_runs WHERE state = 'completed') AS last_scan"
);

/// The live album a local id or alias names now: the album itself while it
/// is live, else the album its alias points at (every merge points older
/// aliases at the survivor), else the album it was retired into.
const DIRECT_ALBUM: &str = "SELECT id FROM ( \
    SELECT id, 0 AS rank FROM local_albums WHERE id = ?1 AND retired_into_album_id IS NULL \
    UNION ALL SELECT b.id, 1 FROM local_album_aliases x \
     JOIN local_albums b ON b.id = x.local_album_id \
     WHERE x.alias = lower(?1) AND b.retired_into_album_id IS NULL \
    UNION ALL SELECT b.id, 2 FROM local_albums a \
     JOIN local_albums b ON b.id = a.retired_into_album_id \
     WHERE a.id = ?1 AND b.retired_into_album_id IS NULL) \
    ORDER BY rank LIMIT 1";

/// The live artist a local id or alias names now, in the same order as
/// [`DIRECT_ALBUM`]. An artist merge never repoints `retired_into_artist_id`
/// of artists merged earlier, so the alias comes before it.
const DIRECT_ARTIST: &str = "SELECT id FROM ( \
    SELECT id, 0 AS rank FROM local_artists WHERE id = ?1 AND retired_into_artist_id IS NULL \
    UNION ALL SELECT r.id, 1 FROM local_artist_aliases x \
     JOIN local_artists r ON r.id = x.local_artist_id \
     WHERE x.alias = lower(?1) AND r.retired_into_artist_id IS NULL \
    UNION ALL SELECT r.id, 2 FROM local_artists a \
     JOIN local_artists r ON r.id = a.retired_into_artist_id \
     WHERE a.id = ?1 AND r.retired_into_artist_id IS NULL) \
    ORDER BY rank LIMIT 1";

/// The live artist linked to one MusicBrainz artist id, by the lowercase
/// expression index.
const ARTIST_FOR_MBID: &str = "SELECT r.id FROM local_artist_external_identities e \
    JOIN local_artists r ON r.id = e.local_artist_id \
    WHERE e.provider = 'musicbrainz' AND lower(e.provider_artist_id) = lower(?1) \
    AND r.retired_into_artist_id IS NULL ORDER BY r.id LIMIT 1";

/// Owned albums holding one of the release groups.
const OWNERS_BY_GROUP: &str = concat!(
    "SELECT e.local_album_id, lower(e.release_group_mbid) \
     FROM local_album_external_identities e JOIN local_albums b ON b.id = e.local_album_id \
     WHERE lower(e.release_group_mbid) IN ({ids}) AND ",
    musicbrainz!("e"),
    " AND ",
    owned_album!("b")
);

/// The live copy of one release group (`?1`, lowercase) when no copy is
/// owned: the oldest copy with files, else the oldest copy. Its album page
/// stays reachable while its files are gone.
const LIVE_COPY_BY_GROUP: &str = concat!(
    "SELECT b.id FROM local_album_external_identities e \
     JOIN local_albums b ON b.id = e.local_album_id \
     WHERE lower(e.release_group_mbid) = ?1 AND ",
    musicbrainz!("e"),
    " AND ",
    live_album!("b"),
    " ORDER BY ",
    copy_order!("b"),
    " LIMIT 1"
);

/// Owned albums identified as one of the releases.
const OWNERS_BY_RELEASE: &str = concat!(
    "SELECT e.local_album_id, lower(e.release_mbid) \
     FROM local_album_external_identities e JOIN local_albums b ON b.id = e.local_album_id \
     WHERE e.release_mbid IS NOT NULL AND lower(e.release_mbid) IN ({ids}) AND ",
    musicbrainz!("e"),
    " AND ",
    owned_album!("b")
);

/// Identity lookups behind membership, album status, track resolution and
/// the stats extras.
///
/// Identifiers here are what a catalog page holds: a local album id, a
/// legacy alias, or a MusicBrainz release-group or release id (compared
/// case-insensitively). Retired (merged) albums resolve to the album they
/// merged into. Absence is an empty answer, never failure.
#[derive(Clone, Debug)]
pub struct LibraryLookups {
    pool: SqlitePool,
}

/// The live album or artist (`kind`) a local id or alias names now, if
/// any. A track id always answers: the track it names now, or itself.
pub(super) async fn current_id(
    pool: &SqlitePool,
    kind: &str,
    identifier: &str,
) -> Result<Option<String>, StoreError> {
    let sql = match kind {
        "album" => DIRECT_ALBUM,
        "artist" => DIRECT_ARTIST,
        "track" => CURRENT_TRACK,
        _ => return Ok(None),
    };
    sqlx::query_scalar(sql)
        .bind(identifier)
        .fetch_optional(pool)
        .await
        .map_err(|error| internal("library.lookups.direct", error))
}

/// `(album id, matched id)` pairs for one owners query over lowercase ids.
async fn owners(
    pool: &SqlitePool,
    sql: &str,
    lowered: &[String],
) -> Result<Vec<(String, String)>, StoreError> {
    let mut found = Vec::new();
    for chunk in lowered.chunks(MAX_BINDS) {
        let statement = sql.replace("{ids}", &in_placeholders(chunk.len()));
        let mut query = sqlx::query_as::<_, (String, String)>(&statement);
        for id in chunk {
            query = query.bind(id);
        }
        found.extend(
            query
                .fetch_all(pool)
                .await
                .map_err(|error| internal("library.lookups.owners", error))?,
        );
    }
    Ok(found)
}

/// Lowercase, trimmed, de-duplicated, blanks dropped.
fn lowered(ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.iter()
        .map(|id| id.trim().to_lowercase())
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect()
}

impl LibraryLookups {
    /// Lookups over the reader pool.
    pub fn new(pool: &SqlitePool) -> Self {
        Self { pool: pool.clone() }
    }

    /// Review, local-only and last-scan totals.
    pub async fn stats_extras(&self) -> Result<StatsExtras, StoreError> {
        let (reviews, local_only, last_scan): (i64, i64, Option<f64>) =
            sqlx::query_as(STATS_EXTRAS)
                .fetch_one(&self.pool)
                .await
                .map_err(|error| internal("library.stats.extras", error))?;
        Ok(StatsExtras {
            review_count: reviews.max(0) as u64,
            local_only_count: local_only.max(0) as u64,
            last_scan_at: last_scan,
        })
    }

    /// The MusicBrainz album ids (lowercase) the library holds with at
    /// least one streamable track.
    pub async fn owned_albums(&self, mbids: &[String]) -> Result<HashSet<String>, StoreError> {
        LocalCatalog::new(self.pool.clone())
            .owned_albums(mbids)
            .await
            .map_err(|error| internal("library.membership.owned", error))
    }

    /// The album ids (lowercase) with an open acquisition request.
    pub async fn requested_albums(&self, mbids: &[String]) -> Result<HashSet<String>, StoreError> {
        LocalCatalog::new(self.pool.clone())
            .requested_albums(mbids)
            .await
            .map_err(|error| internal("library.membership.requested", error))
    }

    /// Every held release group and every open album request (the
    /// unfiltered membership answer).
    pub async fn all_album_mbids(&self) -> Result<AlbumMbids, StoreError> {
        let catalog = LocalCatalog::new(self.pool.clone());
        let owned = catalog
            .all_owned_release_groups()
            .await
            .map_err(|error| internal("library.mbids.owned", error))?;
        let requested = catalog
            .all_requested_albums()
            .await
            .map_err(|error| internal("library.mbids.requested", error))?;
        Ok(AlbumMbids { owned, requested })
    }

    /// The one live local album each identifier names. Identifiers that
    /// name nothing, or a MusicBrainz id held by more than one album, are
    /// left out rather than guessed.
    pub async fn resolve_albums(
        &self,
        identifiers: &[String],
    ) -> Result<HashMap<String, String>, StoreError> {
        let pool = &self.pool;
        let mut resolved = HashMap::new();
        let mut pending = Vec::new();
        let mut seen = HashSet::new();
        for identifier in identifiers {
            if !seen.insert(identifier.as_str()) {
                continue;
            }
            match current_id(pool, "album", identifier).await? {
                Some(album_id) => {
                    resolved.insert(identifier.clone(), album_id);
                }
                None => pending.push(identifier.clone()),
            }
        }
        if pending.is_empty() {
            return Ok(resolved);
        }
        // A MusicBrainz id resolves only when exactly one live album
        // holds it: several owners are separate editions, and picking
        // one would play the wrong copy.
        let needles = lowered(&pending);
        let mut candidates: HashMap<String, HashSet<String>> = HashMap::new();
        for sql in [OWNERS_BY_GROUP, OWNERS_BY_RELEASE] {
            for (album_id, matched) in owners(pool, sql, &needles).await? {
                candidates.entry(matched).or_default().insert(album_id);
            }
        }
        for identifier in pending {
            if let Some(albums) = candidates.get(&identifier.trim().to_lowercase())
                && albums.len() == 1
                && let Some(album_id) = albums.iter().next()
            {
                resolved.insert(identifier, album_id.clone());
            }
        }
        Ok(resolved)
    }

    /// Every live local album an identifier covers for the album status
    /// view: the album a local id or alias names, else every owned album
    /// holding the MusicBrainz release group (or release), ordered by id,
    /// else the group's live copy without files.
    pub async fn status_albums(&self, identifier: &str) -> Result<Vec<String>, StoreError> {
        let pool = &self.pool;
        if let Some(album_id) = current_id(pool, "album", identifier).await? {
            return Ok(vec![album_id]);
        }
        let needle = lowered(&[identifier.to_owned()]);
        for sql in [OWNERS_BY_GROUP, OWNERS_BY_RELEASE] {
            let mut albums: Vec<String> = owners(pool, sql, &needle)
                .await?
                .into_iter()
                .map(|(album_id, _)| album_id)
                .collect();
            if !albums.is_empty() {
                albums.sort();
                albums.dedup();
                return Ok(albums);
            }
        }
        let Some(group) = needle.first() else {
            return Ok(Vec::new());
        };
        let copy: Option<String> = sqlx::query_scalar(LIVE_COPY_BY_GROUP)
            .bind(group)
            .fetch_optional(pool)
            .await
            .map_err(|error| internal("library.lookups.live_copy", error))?;
        Ok(copy.into_iter().collect())
    }

    /// The live album a local id or alias names now: the album itself, the
    /// album a merge pointed its alias at, or the album it was retired into.
    /// None when the id names nothing live.
    pub async fn current_album(&self, id: &str) -> Result<Option<String>, StoreError> {
        current_id(&self.pool, "album", id).await
    }

    /// The live artist a local id or alias names now, in the same order as
    /// [`LibraryLookups::current_album`].
    pub async fn current_artist(&self, id: &str) -> Result<Option<String>, StoreError> {
        current_id(&self.pool, "artist", id).await
    }

    /// The live artist linked to a MusicBrainz artist id, compared without
    /// case. None when no live artist carries it.
    pub async fn artist_for_mbid(&self, mbid: &str) -> Result<Option<String>, StoreError> {
        sqlx::query_scalar(ARTIST_FOR_MBID)
            .bind(mbid)
            .fetch_optional(&self.pool)
            .await
            .map_err(|error| internal("library.lookups.artist_mbid", error))
    }

    /// Streamable tracks of the albums, ordered by album id, disc, track.
    pub async fn album_tracks_batch(
        &self,
        album_ids: &[String],
    ) -> Result<Vec<TrackRecord>, StoreError> {
        let pool = &self.pool;
        let mut tracks = Vec::new();
        for chunk in album_ids.chunks(MAX_BINDS) {
            let sql = format!(
                "SELECT {TRACK_COLUMNS} {TRACK_JOINS} \
                 WHERE t.local_album_id IN ({}) AND t.availability = 'indexed' \
                 ORDER BY t.local_album_id, t.disc_number, t.track_number, t.id",
                in_placeholders(chunk.len())
            );
            let mut query = sqlx::query(&sql);
            for id in chunk {
                query = query.bind(id);
            }
            let rows = query
                .fetch_all(pool)
                .await
                .map_err(|error| internal("library.lookups.tracks", error))?;
            tracks.extend(rows.iter().map(map_track));
        }
        Ok(tracks)
    }
}
