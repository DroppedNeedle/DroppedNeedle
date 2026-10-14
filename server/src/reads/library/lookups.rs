//! SQLite adapter for [`LibraryLookups`]: membership, identifier
//! resolution for album status and track resolution, and the stats extras.
//!
//! Reads only, over the reader pool. MusicBrainz ids compare lowercase
//! through the `lower(...)` expression indexes on the album identities.
//! Ownership and open requests reuse the catalog pages' definitions
//! ([`LocalCatalog`]) so a card and a membership check never disagree.

use std::collections::{HashMap, HashSet};

use sqlx::SqlitePool;

use super::sqlite::{
    LibraryDb, TRACK_COLUMNS, TRACK_JOINS, in_placeholders, internal, map_track, unwired_store,
};
use super::stores::{AlbumMbids, BoxFuture, LibraryLookups, StatsExtras, StoreError, TrackRecord};
use crate::reads::catalog::library::LocalCatalog;

/// Most ids bound into one `IN (...)` list.
const MAX_BINDS: usize = 500;

/// Review, local-only and last-scan totals in one round trip. Local-only
/// albums are live albums with a streamable track and no identity row
/// (v2 `local_only_count`); the last scan is the newest completed run.
const STATS_EXTRAS: &str = "SELECT \
    (SELECT COUNT(*) FROM library_identify_reviews WHERE state = 'pending') AS reviews, \
    (SELECT COUNT(*) FROM local_albums a WHERE a.retired_into_album_id IS NULL \
     AND NOT EXISTS (SELECT 1 FROM local_album_external_identities i \
      WHERE i.local_album_id = a.id) \
     AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
      AND t.availability = 'indexed')) AS local_only, \
    (SELECT MAX(terminal_at) FROM library_scan_runs WHERE state = 'completed') AS last_scan";

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

/// The track a local id names now: the track itself while its file is
/// indexed, else the track its alias names (a missing track that
/// continued into another), else the id itself, so a read's own
/// availability test answers for it as for any id.
pub(crate) const CURRENT_TRACK: &str = "SELECT id FROM ( \
    SELECT id, 0 AS rank FROM local_tracks WHERE id = ?1 AND availability = 'indexed' \
    UNION ALL SELECT local_track_id, 1 FROM local_track_aliases WHERE alias = ?1 \
    UNION ALL SELECT ?1, 2) \
    ORDER BY rank LIMIT 1";

/// Live albums with a streamable track holding one of the release groups.
const OWNERS_BY_GROUP: &str = "SELECT e.local_album_id, lower(e.release_group_mbid) \
    FROM local_album_external_identities e JOIN local_albums b ON b.id = e.local_album_id \
    WHERE b.retired_into_album_id IS NULL AND lower(e.release_group_mbid) IN ({ids}) \
    AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = b.id \
    AND t.availability = 'indexed')";

/// Live albums with a streamable track identified as one of the releases.
const OWNERS_BY_RELEASE: &str = "SELECT e.local_album_id, lower(e.release_mbid) \
    FROM local_album_external_identities e JOIN local_albums b ON b.id = e.local_album_id \
    WHERE b.retired_into_album_id IS NULL AND e.release_mbid IS NOT NULL \
    AND lower(e.release_mbid) IN ({ids}) \
    AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = b.id \
    AND t.availability = 'indexed')";

/// Lookups over the reader pool.
#[derive(Clone, Debug)]
pub struct SqliteLookups {
    db: LibraryDb,
}

impl SqliteLookups {
    /// Adapter over one handle.
    pub fn new(db: &LibraryDb) -> Self {
        Self { db: db.clone() }
    }

    fn pool(&self) -> Result<&SqlitePool, StoreError> {
        self.db.live().ok_or_else(unwired_store)
    }
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

impl LibraryLookups for SqliteLookups {
    fn stats_extras<'a>(&'a self) -> BoxFuture<'a, Result<StatsExtras, StoreError>> {
        Box::pin(async move {
            let (reviews, local_only, last_scan): (i64, i64, Option<f64>) =
                sqlx::query_as(STATS_EXTRAS)
                    .fetch_one(self.pool()?)
                    .await
                    .map_err(|error| internal("library.stats.extras", error))?;
            Ok(StatsExtras {
                review_count: reviews.max(0) as u64,
                local_only_count: local_only.max(0) as u64,
                last_scan_at: last_scan,
            })
        })
    }

    fn owned_albums<'a>(
        &'a self,
        mbids: &'a [String],
    ) -> BoxFuture<'a, Result<HashSet<String>, StoreError>> {
        Box::pin(async move {
            LocalCatalog::new(self.pool()?.clone())
                .owned_albums(mbids)
                .await
                .map_err(|error| internal("library.membership.owned", error))
        })
    }

    fn requested_albums<'a>(
        &'a self,
        mbids: &'a [String],
    ) -> BoxFuture<'a, Result<HashSet<String>, StoreError>> {
        Box::pin(async move {
            LocalCatalog::new(self.pool()?.clone())
                .requested_albums(mbids)
                .await
                .map_err(|error| internal("library.membership.requested", error))
        })
    }

    fn all_album_mbids<'a>(&'a self) -> BoxFuture<'a, Result<AlbumMbids, StoreError>> {
        Box::pin(async move {
            let catalog = LocalCatalog::new(self.pool()?.clone());
            let owned = catalog
                .all_owned_release_groups()
                .await
                .map_err(|error| internal("library.mbids.owned", error))?;
            let requested = catalog
                .all_requested_albums()
                .await
                .map_err(|error| internal("library.mbids.requested", error))?;
            Ok(AlbumMbids { owned, requested })
        })
    }

    fn resolve_albums<'a>(
        &'a self,
        identifiers: &'a [String],
    ) -> BoxFuture<'a, Result<HashMap<String, String>, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
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
        })
    }

    fn status_albums<'a>(
        &'a self,
        identifier: &'a str,
    ) -> BoxFuture<'a, Result<Vec<String>, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
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
            Ok(Vec::new())
        })
    }

    fn current_album<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, StoreError>> {
        Box::pin(async move { current_id(self.pool()?, "album", id).await })
    }

    fn current_artist<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, StoreError>> {
        Box::pin(async move { current_id(self.pool()?, "artist", id).await })
    }

    fn album_tracks_batch<'a>(
        &'a self,
        album_ids: &'a [String],
    ) -> BoxFuture<'a, Result<Vec<TrackRecord>, StoreError>> {
        Box::pin(async move {
            let pool = self.pool()?;
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
        })
    }
}
