//! Identity lookups on [`LocalCatalog`]: identifier resolution for album
//! status and track resolution, membership, and the stats extras.
//!
//! Identifiers here are what a catalog page holds: a local album id, a
//! legacy alias, or a MusicBrainz release-group or release id (compared
//! case-insensitively through the `lower(...)` expression indexes on the
//! album identities). Retired (merged) albums resolve to the album they
//! merged into. Absence is an empty answer, never failure. Ownership and
//! open requests are the catalog pages' own reads, so a card and a
//! membership check never disagree.

use std::collections::{HashMap, HashSet};

use droppedneedle_kernel::db::statement::ListLen;
use rusqlite::types::Value;

use super::sql;
use super::store::{LocalCatalog, first, rows, texts, track_row, unsigned};
use super::stores::{AlbumMbids, StatsExtras, StoreError, TrackRecord};

/// Most ids bound into one `IN (...)` list.
const MAX_BINDS: usize = 500;

/// Lowercase, trimmed, de-duplicated, blanks dropped.
fn lowered(ids: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.iter()
        .map(|id| id.trim().to_lowercase())
        .filter(|id| !id.is_empty() && seen.insert(id.clone()))
        .collect()
}

/// Which owners statement a lookup runs.
#[derive(Clone, Copy)]
enum Owners {
    /// By release group.
    Group,
    /// By release.
    Release,
}

impl LocalCatalog {
    /// Review, local-only and last-scan totals.
    pub async fn stats_extras(&self) -> Result<StatsExtras, StoreError> {
        let (reviews, local_only, last_scan): (i64, i64, Option<f64>) = self
            .read("library.stats.extras", |conn| {
                let mut statement = conn.prepare_cached(sql::STATS_EXTRAS.text())?;
                statement.query_row([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            })
            .await?;
        Ok(StatsExtras {
            review_count: unsigned(reviews),
            local_only_count: unsigned(local_only),
            last_scan_at: last_scan,
        })
    }

    /// Every held release group and every open album request (the
    /// unfiltered membership answer).
    pub async fn all_album_mbids(&self) -> Result<AlbumMbids, StoreError> {
        let owned = self.all_owned_release_groups().await?;
        let requested = self.all_requested_albums().await?;
        Ok(AlbumMbids { owned, requested })
    }

    /// `(album id, matched id)` pairs for one owners statement over
    /// lowercase ids, one read per chunk.
    async fn owners(
        &self,
        owners: Owners,
        lowered: &[String],
    ) -> Result<Vec<(String, String)>, StoreError> {
        let mut found = Vec::new();
        for chunk in lowered.chunks(MAX_BINDS) {
            let values = texts(chunk);
            found.extend(
                self.read("library.lookups.owners", move |conn| {
                    let len = ListLen::new(values.len());
                    let text = match owners {
                        Owners::Group => sql::owners_by_group(len),
                        Owners::Release => sql::owners_by_release(len),
                    };
                    rows(conn, &text, &values, |row| Ok((row.get(0)?, row.get(1)?)))
                })
                .await?,
            );
        }
        Ok(found)
    }

    /// The one live local album each identifier names. Identifiers that
    /// name nothing, or a MusicBrainz id held by more than one album, are
    /// left out rather than guessed.
    pub async fn resolve_albums(
        &self,
        identifiers: &[String],
    ) -> Result<HashMap<String, String>, StoreError> {
        let mut seen = HashSet::new();
        let asked: Vec<String> = identifiers
            .iter()
            .filter(|identifier| seen.insert(identifier.as_str()))
            .cloned()
            .collect();
        let mut resolved = HashMap::new();
        let mut pending = Vec::new();
        // One statement answers a whole chunk of local ids and aliases.
        for chunk in asked.chunks(MAX_BINDS) {
            let values = texts(chunk);
            let answers: Vec<(String, Option<String>)> = self
                .read("library.lookups.direct", move |conn| {
                    rows(
                        conn,
                        &sql::current_albums(ListLen::new(values.len())),
                        &values,
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                })
                .await?;
            let mut answers: HashMap<String, Option<String>> = answers.into_iter().collect();
            for identifier in chunk {
                match answers.remove(identifier).flatten() {
                    Some(album_id) => {
                        resolved.insert(identifier.clone(), album_id);
                    }
                    None => pending.push(identifier.clone()),
                }
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
        for owners in [Owners::Group, Owners::Release] {
            for (album_id, matched) in self.owners(owners, &needles).await? {
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
        if let Some(album_id) = self.current_album(identifier).await? {
            return Ok(vec![album_id]);
        }
        let needle = lowered(&[identifier.to_owned()]);
        for owners in [Owners::Group, Owners::Release] {
            let mut albums: Vec<String> = self
                .owners(owners, &needle)
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
        let values = vec![Value::Text(group.clone())];
        let copy: Option<String> = self
            .read("library.lookups.live_copy", move |conn| {
                first(conn, sql::LIVE_COPY_BY_GROUP.text(), &values, |row| {
                    row.get(0)
                })
            })
            .await?;
        Ok(copy.into_iter().collect())
    }

    /// The live album a local id or alias names now: the album itself, the
    /// album a merge pointed its alias at, or the album it was retired into.
    /// None when the id names nothing live.
    pub async fn current_album(&self, id: &str) -> Result<Option<String>, StoreError> {
        let values = vec![Value::Text(id.to_owned())];
        self.read("library.lookups.direct", move |conn| {
            first(conn, sql::CURRENT_ALBUM.text(), &values, |row| row.get(0))
        })
        .await
    }

    /// The live artist a local id or alias names now, in the same order as
    /// [`LocalCatalog::current_album`].
    pub async fn current_artist(&self, id: &str) -> Result<Option<String>, StoreError> {
        let values = vec![Value::Text(id.to_owned())];
        self.read("library.lookups.direct", move |conn| {
            first(conn, sql::CURRENT_ARTIST.text(), &values, |row| row.get(0))
        })
        .await
    }

    /// The live artist linked to a MusicBrainz artist id, compared without
    /// case. None when no live artist carries it.
    pub async fn artist_for_mbid(&self, mbid: &str) -> Result<Option<String>, StoreError> {
        let values = vec![Value::Text(mbid.to_owned())];
        self.read("library.lookups.artist_mbid", move |conn| {
            first(conn, sql::ARTIST_FOR_MBID.text(), &values, |row| row.get(0))
        })
        .await
    }

    /// Streamable tracks of the albums, ordered by album id, disc, track.
    pub async fn album_tracks_batch(
        &self,
        album_ids: &[String],
    ) -> Result<Vec<TrackRecord>, StoreError> {
        let mut tracks = Vec::new();
        for chunk in album_ids.chunks(MAX_BINDS) {
            let values = texts(chunk);
            tracks.extend(
                self.read("library.lookups.tracks", move |conn| {
                    rows(
                        conn,
                        &sql::album_tracks_in(ListLen::new(values.len())),
                        &values,
                        track_row,
                    )
                })
                .await?,
            );
        }
        Ok(tracks)
    }
}
