//! SQLite access for the concerts feed.
//!
//! Tables (all in the baseline schema): `live_event_feed` is the shared
//! feed, one row per (source, listing, artist) so a festival bill shows up
//! under every followed co-headliner; `artist_event_check` is the per-artist
//! sweep cursor; `artist_tm_attraction`, `artist_skiddle_ids` and
//! `artist_skiddle_resolution` cache source resolutions (negative ones too,
//! so the seven-day TTL applies to "not on Ticketmaster" as well);
//! `user_event_cities` and `user_event_seen` are per user.
//!
//! Reads go through the pool. Writes go through the writer lane: the sweep
//! on the background lane, user actions on the foreground lane. No network
//! call ever happens inside a write.

use std::collections::{HashMap, HashSet};

use rusqlite::params;
use sqlx::{Row, SqlitePool, sqlite::SqliteRow};

use super::matching::{Confidence, EventRow, StoredConcert, SweepArtist, TmBasis};
use super::models::{ConcertStatus, EventCity, EventSource};
use crate::db::{DbError, Lane, WriteLane, map_sqlx_busy};

/// A store failure. The text goes to the log only.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// Lock contention outlived the busy timeout; the caller may retry.
    #[error("database busy during {0}")]
    Busy(String),
    /// Anything else.
    #[error("concerts store failure: {0}")]
    Internal(String),
}

impl From<DbError> for StoreError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::Busy { operation } => Self::Busy(operation),
            other => Self::Internal(other.to_string()),
        }
    }
}

fn read_error(operation: &str) -> impl FnOnce(sqlx::Error) -> StoreError + '_ {
    move |error| StoreError::from(map_sqlx_busy(operation, error))
}

/// Cached Ticketmaster resolution for one artist.
#[derive(Debug, Clone, PartialEq)]
pub struct TmResolution {
    /// Attraction id; `None` when the artist has no Ticketmaster presence.
    pub attraction_id: Option<String>,
    /// How it was resolved.
    pub basis: TmBasis,
    /// Unix seconds.
    pub resolved_at: f64,
}

/// Cached Skiddle resolution for one artist. Empty ids mean no presence.
#[derive(Debug, Clone, PartialEq)]
pub struct SkiddleResolution {
    /// Every matching Skiddle artist id.
    pub artist_ids: Vec<String>,
    /// Unix seconds.
    pub resolved_at: f64,
}

const FEED_COLUMNS: &str = "f.source, f.source_event_id, f.artist_mbid_lower, f.artist_name, \
    f.event_name, f.venue_name, f.city, f.region, f.country_code, f.latitude, f.longitude, \
    f.starts_at, f.local_date, f.status, f.ticket_url, f.match_confidence";

fn event_from_row(row: &SqliteRow) -> Result<Option<EventRow>, sqlx::Error> {
    let source: String = row.try_get("source")?;
    let Some(source) = EventSource::parse(&source) else {
        return Ok(None);
    };
    let confidence: String = row.try_get("match_confidence")?;
    let status: String = row.try_get("status")?;
    Ok(Some(EventRow {
        source,
        source_event_id: row.try_get("source_event_id")?,
        artist_mbid_lower: row.try_get("artist_mbid_lower")?,
        artist_name: row.try_get("artist_name")?,
        event_name: row.try_get("event_name")?,
        local_date: row.try_get("local_date")?,
        status: ConcertStatus::parse(&status),
        confidence: if confidence == "mbid" {
            Confidence::Mbid
        } else {
            Confidence::Name
        },
        venue_name: row.try_get("venue_name")?,
        city: row.try_get("city")?,
        region: row.try_get("region")?,
        country_code: row.try_get("country_code")?,
        latitude: row.try_get("latitude")?,
        longitude: row.try_get("longitude")?,
        starts_at: row.try_get("starts_at")?,
        ticket_url: row.try_get("ticket_url")?,
    }))
}

/// Pool plus writer lane over the application database.
#[derive(Clone, Debug)]
pub struct ConcertsStore {
    pool: SqlitePool,
    lane: WriteLane,
}

impl ConcertsStore {
    /// Store over the serving runtime's pool and lane.
    pub fn new(pool: SqlitePool, lane: WriteLane) -> Self {
        Self { pool, lane }
    }

    // -- sweep set -----------------------------------------------------------

    /// Every distinct followed artist, keyed by lowercase MBID.
    pub async fn followed_artists(&self) -> Result<Vec<SweepArtist>, StoreError> {
        let rows = sqlx::query(
            "SELECT artist_mbid_lower, MIN(artist_mbid) AS artist_mbid, \
             MIN(artist_name) AS artist_name FROM user_followed_artists \
             GROUP BY artist_mbid_lower ORDER BY artist_mbid_lower",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(read_error("concerts followed artists"))?;
        rows.iter()
            .map(|row| {
                Ok(SweepArtist {
                    mbid: row.try_get("artist_mbid")?,
                    mbid_lower: row.try_get("artist_mbid_lower")?,
                    name: row.try_get("artist_name")?,
                })
            })
            .collect::<Result<_, sqlx::Error>>()
            .map_err(read_error("concerts followed artists"))
    }

    /// Every library artist with a MusicBrainz identity that leads or is
    /// credited on a live album with indexed tracks (library sweep scope).
    pub async fn library_artists(&self) -> Result<Vec<SweepArtist>, StoreError> {
        let rows = sqlx::query(
            "SELECT lower(e.provider_artist_id) AS mbid_lower, \
             MIN(e.provider_artist_id) AS mbid, MIN(a.display_name) AS name \
             FROM local_artist_external_identities e \
             JOIN local_artists a ON a.id = e.local_artist_id \
             JOIN local_album_artists credit ON credit.local_artist_id = e.local_artist_id \
             JOIN local_albums b ON b.id = credit.local_album_id \
             WHERE e.provider = 'musicbrainz' AND e.provider_artist_id != '' \
             AND a.retired_into_artist_id IS NULL AND b.retired_into_album_id IS NULL \
             AND EXISTS (SELECT 1 FROM local_tracks t WHERE t.local_album_id = b.id \
             AND t.availability = 'indexed') \
             GROUP BY lower(e.provider_artist_id) ORDER BY 1",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(read_error("concerts library artists"))?;
        rows.iter()
            .map(|row| {
                Ok(SweepArtist {
                    mbid: row.try_get("mbid")?,
                    mbid_lower: row.try_get("mbid_lower")?,
                    name: row.try_get("name")?,
                })
            })
            .collect::<Result<_, sqlx::Error>>()
            .map_err(read_error("concerts library artists"))
    }

    /// Lowercase MBID to last check time, for least-recently-checked-first
    /// rotation. Never-checked artists are absent.
    pub async fn cursor_ages(&self) -> Result<HashMap<String, f64>, StoreError> {
        let rows = sqlx::query(
            "SELECT artist_mbid_lower, last_checked_at FROM artist_event_check \
             WHERE last_checked_at IS NOT NULL",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(read_error("concerts cursor ages"))?;
        rows.iter()
            .map(|row| {
                Ok((
                    row.try_get("artist_mbid_lower")?,
                    row.try_get("last_checked_at")?,
                ))
            })
            .collect::<Result<_, sqlx::Error>>()
            .map_err(read_error("concerts cursor ages"))
    }

    /// Users following an artist, for the `concerts_new` fan-out.
    pub async fn followers(&self, artist_mbid_lower: &str) -> Result<Vec<String>, StoreError> {
        sqlx::query_scalar(
            "SELECT user_id FROM user_followed_artists WHERE artist_mbid_lower = ? \
             ORDER BY user_id",
        )
        .bind(artist_mbid_lower)
        .fetch_all(&self.pool)
        .await
        .map_err(read_error("concerts followers"))
    }

    // -- source resolutions --------------------------------------------------

    /// Cached Ticketmaster resolution, if any.
    pub async fn tm_resolution(
        &self,
        artist_mbid_lower: &str,
    ) -> Result<Option<TmResolution>, StoreError> {
        let row = sqlx::query(
            "SELECT attraction_id, match_basis, resolved_at FROM artist_tm_attraction \
             WHERE artist_mbid_lower = ?",
        )
        .bind(artist_mbid_lower)
        .fetch_optional(&self.pool)
        .await
        .map_err(read_error("concerts tm resolution"))?;
        row.map(|row| {
            let basis: String = row.try_get("match_basis")?;
            Ok(TmResolution {
                attraction_id: row.try_get("attraction_id")?,
                basis: TmBasis::parse(&basis),
                resolved_at: row.try_get("resolved_at")?,
            })
        })
        .transpose()
        .map_err(read_error("concerts tm resolution"))
    }

    /// Cached Skiddle resolution, if any.
    pub async fn skiddle_resolution(
        &self,
        artist_mbid_lower: &str,
    ) -> Result<Option<SkiddleResolution>, StoreError> {
        let marker: Option<f64> = sqlx::query_scalar(
            "SELECT resolved_at FROM artist_skiddle_resolution WHERE artist_mbid_lower = ?",
        )
        .bind(artist_mbid_lower)
        .fetch_optional(&self.pool)
        .await
        .map_err(read_error("concerts skiddle resolution"))?;
        let Some(resolved_at) = marker else {
            return Ok(None);
        };
        let artist_ids = sqlx::query_scalar(
            "SELECT skiddle_artistid FROM artist_skiddle_ids WHERE artist_mbid_lower = ? \
             ORDER BY skiddle_artistid",
        )
        .bind(artist_mbid_lower)
        .fetch_all(&self.pool)
        .await
        .map_err(read_error("concerts skiddle ids"))?;
        Ok(Some(SkiddleResolution {
            artist_ids,
            resolved_at,
        }))
    }

    /// Cache a Ticketmaster resolution (negative results included).
    pub async fn set_tm_resolution(
        &self,
        artist_mbid_lower: &str,
        attraction_id: Option<String>,
        basis: TmBasis,
        now: f64,
    ) -> Result<(), StoreError> {
        let artist = artist_mbid_lower.to_owned();
        self.lane
            .write(Lane::Background, "concerts tm resolution", move |tx| {
                tx.execute(
                    "INSERT INTO artist_tm_attraction \
                     (artist_mbid_lower, attraction_id, match_basis, resolved_at) \
                     VALUES (?1, ?2, ?3, ?4) ON CONFLICT(artist_mbid_lower) DO UPDATE SET \
                     attraction_id = excluded.attraction_id, \
                     match_basis = excluded.match_basis, resolved_at = excluded.resolved_at",
                    params![artist, attraction_id, basis.as_str(), now],
                )?;
                Ok(())
            })
            .await
            .map_err(StoreError::from)
    }

    /// Replace the Skiddle id set and stamp the TTL marker together. An
    /// empty set is a valid negative result.
    pub async fn set_skiddle_resolution(
        &self,
        artist_mbid_lower: &str,
        artist_ids: Vec<String>,
        now: f64,
    ) -> Result<(), StoreError> {
        let artist = artist_mbid_lower.to_owned();
        self.lane
            .write(Lane::Background, "concerts skiddle resolution", move |tx| {
                tx.execute(
                    "DELETE FROM artist_skiddle_ids WHERE artist_mbid_lower = ?1",
                    params![artist],
                )?;
                for id in &artist_ids {
                    tx.execute(
                        "INSERT OR IGNORE INTO artist_skiddle_ids \
                         (artist_mbid_lower, skiddle_artistid) VALUES (?1, ?2)",
                        params![artist, id],
                    )?;
                }
                tx.execute(
                    "INSERT INTO artist_skiddle_resolution (artist_mbid_lower, resolved_at) \
                     VALUES (?1, ?2) ON CONFLICT(artist_mbid_lower) DO UPDATE SET \
                     resolved_at = excluded.resolved_at",
                    params![artist, now],
                )?;
                Ok(())
            })
            .await
            .map_err(StoreError::from)
    }

    // -- feed ----------------------------------------------------------------

    /// The (source, listing) keys stored for one artist.
    pub async fn event_keys(
        &self,
        artist_mbid_lower: &str,
    ) -> Result<HashSet<(EventSource, String)>, StoreError> {
        let rows = sqlx::query(
            "SELECT source, source_event_id FROM live_event_feed WHERE artist_mbid_lower = ?",
        )
        .bind(artist_mbid_lower)
        .fetch_all(&self.pool)
        .await
        .map_err(read_error("concerts event keys"))?;
        let mut keys = HashSet::with_capacity(rows.len());
        for row in &rows {
            let source: String = row
                .try_get("source")
                .map_err(read_error("concerts event keys"))?;
            let id: String = row
                .try_get("source_event_id")
                .map_err(read_error("concerts event keys"))?;
            if let Some(source) = EventSource::parse(&source) {
                keys.insert((source, id));
            }
        }
        Ok(keys)
    }

    /// Apply one artist's sweep in one transaction: delete the vanished
    /// keys, upsert the current rows (keeping `discovered_at` on conflict),
    /// and stamp the artist's cursor ok.
    pub async fn apply_sweep(
        &self,
        artist_mbid_lower: &str,
        upserts: Vec<EventRow>,
        deletes: Vec<(EventSource, String)>,
        now: f64,
    ) -> Result<(), StoreError> {
        let artist = artist_mbid_lower.to_owned();
        self.lane
            .write(Lane::Background, "concerts apply sweep", move |tx| {
                for (source, id) in &deletes {
                    tx.execute(
                        "DELETE FROM live_event_feed WHERE source = ?1 AND source_event_id = ?2 \
                         AND artist_mbid_lower = ?3",
                        params![source.as_str(), id, artist],
                    )?;
                }
                for row in &upserts {
                    tx.execute(
                        "INSERT INTO live_event_feed (source, source_event_id, \
                         artist_mbid_lower, artist_name, event_name, venue_name, city, region, \
                         country_code, latitude, longitude, starts_at, local_date, status, \
                         ticket_url, match_confidence, discovered_at, updated_at) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, \
                         ?15, ?16, ?17, ?17) \
                         ON CONFLICT(source, source_event_id, artist_mbid_lower) DO UPDATE SET \
                         artist_name = excluded.artist_name, event_name = excluded.event_name, \
                         venue_name = excluded.venue_name, city = excluded.city, \
                         region = excluded.region, country_code = excluded.country_code, \
                         latitude = excluded.latitude, longitude = excluded.longitude, \
                         starts_at = excluded.starts_at, local_date = excluded.local_date, \
                         status = excluded.status, ticket_url = excluded.ticket_url, \
                         match_confidence = excluded.match_confidence, \
                         updated_at = excluded.updated_at",
                        params![
                            row.source.as_str(),
                            row.source_event_id,
                            row.artist_mbid_lower,
                            row.artist_name,
                            row.event_name,
                            row.venue_name,
                            row.city,
                            row.region,
                            row.country_code,
                            row.latitude,
                            row.longitude,
                            row.starts_at,
                            row.local_date,
                            row.status.as_str(),
                            row.ticket_url,
                            row.confidence.as_str(),
                            now,
                        ],
                    )?;
                }
                upsert_cursor(tx, &artist, now, "ok", None)?;
                Ok(())
            })
            .await
            .map_err(StoreError::from)
    }

    /// Record a failed check on the artist's cursor. The artist waits for
    /// the next sweep; there is no in-sweep retry.
    pub async fn record_failure(
        &self,
        artist_mbid_lower: &str,
        error: String,
        now: f64,
    ) -> Result<(), StoreError> {
        let artist = artist_mbid_lower.to_owned();
        self.lane
            .write(Lane::Background, "concerts record failure", move |tx| {
                upsert_cursor(tx, &artist, now, "error", Some(&error))?;
                Ok(())
            })
            .await
            .map_err(StoreError::from)
    }

    /// Delete rows dated strictly before `cutoff_local_date`.
    pub async fn prune_before(&self, cutoff_local_date: String) -> Result<usize, StoreError> {
        self.lane
            .write(Lane::Background, "concerts prune", move |tx| {
                Ok(tx.execute(
                    "DELETE FROM live_event_feed WHERE local_date < ?1",
                    params![cutoff_local_date],
                )?)
            })
            .await
            .map_err(StoreError::from)
    }

    /// Feed rows on or after `min_local_date`, oldest first. `user_id`
    /// narrows to that user's follows (carrying the follow's MBID);
    /// `None` reads the whole feed (library scope) with lowercase MBIDs.
    /// `discovered_after` keeps only rows first seen after that time.
    pub async fn concerts(
        &self,
        user_id: Option<&str>,
        min_local_date: &str,
        discovered_after: Option<f64>,
    ) -> Result<Vec<StoredConcert>, StoreError> {
        let mut sql = match user_id {
            Some(_) => format!(
                "SELECT {FEED_COLUMNS}, u.artist_mbid AS link_mbid FROM live_event_feed f \
                 JOIN user_followed_artists u ON u.artist_mbid_lower = f.artist_mbid_lower \
                 AND u.user_id = ? WHERE f.local_date >= ?"
            ),
            None => format!(
                "SELECT {FEED_COLUMNS}, f.artist_mbid_lower AS link_mbid FROM live_event_feed f \
                 WHERE f.local_date >= ?"
            ),
        };
        if discovered_after.is_some() {
            sql.push_str(" AND f.discovered_at > ?");
        }
        sql.push_str(" ORDER BY f.local_date ASC, f.event_name ASC");
        let mut query = sqlx::query(&sql);
        if let Some(user_id) = user_id {
            query = query.bind(user_id);
        }
        query = query.bind(min_local_date);
        if let Some(after) = discovered_after {
            query = query.bind(after);
        }
        let rows = query
            .fetch_all(&self.pool)
            .await
            .map_err(read_error("concerts list"))?;
        let mut concerts = Vec::with_capacity(rows.len());
        for row in &rows {
            let Some(event) = event_from_row(row).map_err(read_error("concerts list"))? else {
                continue;
            };
            let artist_mbid: String = row
                .try_get("link_mbid")
                .map_err(read_error("concerts list"))?;
            concerts.push(StoredConcert { event, artist_mbid });
        }
        Ok(concerts)
    }

    // -- per user ------------------------------------------------------------

    /// The user's cities in picker order.
    pub async fn cities(&self, user_id: &str) -> Result<Vec<EventCity>, StoreError> {
        let rows = sqlx::query(
            "SELECT city_name, country_code, latitude, longitude, radius_km \
             FROM user_event_cities WHERE user_id = ? ORDER BY position",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(read_error("concerts cities"))?;
        rows.iter()
            .map(|row| {
                Ok(EventCity {
                    city_name: row.try_get("city_name")?,
                    latitude: row.try_get("latitude")?,
                    longitude: row.try_get("longitude")?,
                    radius_km: row.try_get("radius_km")?,
                    country_code: row.try_get("country_code")?,
                })
            })
            .collect::<Result<_, sqlx::Error>>()
            .map_err(read_error("concerts cities"))
    }

    /// Replace the user's cities. A repeated coordinate keeps its first entry.
    pub async fn replace_cities(
        &self,
        user_id: &str,
        cities: Vec<EventCity>,
    ) -> Result<(), StoreError> {
        let user_id = user_id.to_owned();
        self.lane
            .write(Lane::Foreground, "concerts replace cities", move |tx| {
                tx.execute(
                    "DELETE FROM user_event_cities WHERE user_id = ?1",
                    params![user_id],
                )?;
                for (position, city) in cities.iter().enumerate() {
                    tx.execute(
                        "INSERT INTO user_event_cities (user_id, city_name, country_code, \
                         latitude, longitude, radius_km, position) \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                         ON CONFLICT(user_id, latitude, longitude) DO NOTHING",
                        params![
                            user_id,
                            city.city_name,
                            city.country_code,
                            city.latitude,
                            city.longitude,
                            city.radius_km,
                            position as i64,
                        ],
                    )?;
                }
                Ok(())
            })
            .await
            .map_err(StoreError::from)
    }

    /// When the user last opened the events page; 0 for never.
    pub async fn seen_at(&self, user_id: &str) -> Result<f64, StoreError> {
        let seen: Option<f64> =
            sqlx::query_scalar("SELECT seen_at FROM user_event_seen WHERE user_id = ?")
                .bind(user_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(read_error("concerts seen at"))?;
        Ok(seen.unwrap_or(0.0))
    }

    /// Stamp the user's seen marker.
    pub async fn mark_seen(&self, user_id: &str, now: f64) -> Result<(), StoreError> {
        let user_id = user_id.to_owned();
        self.lane
            .write(Lane::Foreground, "concerts mark seen", move |tx| {
                tx.execute(
                    "INSERT INTO user_event_seen (user_id, seen_at) VALUES (?1, ?2) \
                     ON CONFLICT(user_id) DO UPDATE SET seen_at = excluded.seen_at",
                    params![user_id, now],
                )?;
                Ok(())
            })
            .await
            .map_err(StoreError::from)
    }
}

fn upsert_cursor(
    tx: &rusqlite::Transaction<'_>,
    artist_mbid_lower: &str,
    now: f64,
    status: &str,
    error: Option<&str>,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO artist_event_check (artist_mbid_lower, last_checked_at, last_status, \
         last_error) VALUES (?1, ?2, ?3, ?4) ON CONFLICT(artist_mbid_lower) DO UPDATE SET \
         last_checked_at = excluded.last_checked_at, last_status = excluded.last_status, \
         last_error = excluded.last_error",
        params![artist_mbid_lower, now, status, error],
    )?;
    Ok(())
}
