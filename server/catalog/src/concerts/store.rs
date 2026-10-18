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
use sqlx::{Row, sqlite::SqliteRow};

use super::matching::{Confidence, EventRow, StoredConcert, SweepArtist, TmBasis};
use super::models::{ConcertStatus, EventCity, EventSource};
use super::sql;
use droppedneedle_kernel::db::{Db, DbError, Lane, map_sqlx_busy};

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

/// The concerts tables in the application database.
#[derive(Clone, Debug)]
pub struct ConcertsStore {
    db: Db,
}

impl ConcertsStore {
    /// Store over the serving runtime's database.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    // -- sweep set -----------------------------------------------------------

    /// Every distinct followed artist, keyed by lowercase MBID.
    pub async fn followed_artists(&self) -> Result<Vec<SweepArtist>, StoreError> {
        let rows = sqlx::query(sql::FOLLOWED_ARTISTS.text())
            .fetch_all(self.db.sqlx())
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
        let rows = sqlx::query(sql::LIBRARY_ARTISTS.text())
            .fetch_all(self.db.sqlx())
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
        let rows = sqlx::query(sql::CURSOR_AGES.text())
            .fetch_all(self.db.sqlx())
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
        sqlx::query_scalar(sql::FOLLOWERS.text())
            .bind(artist_mbid_lower)
            .fetch_all(self.db.sqlx())
            .await
            .map_err(read_error("concerts followers"))
    }

    // -- source resolutions --------------------------------------------------

    /// Cached Ticketmaster resolution, if any.
    pub async fn tm_resolution(
        &self,
        artist_mbid_lower: &str,
    ) -> Result<Option<TmResolution>, StoreError> {
        let row = sqlx::query(sql::TM_RESOLUTION.text())
            .bind(artist_mbid_lower)
            .fetch_optional(self.db.sqlx())
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
        let marker: Option<f64> = sqlx::query_scalar(sql::SKIDDLE_RESOLVED_AT.text())
            .bind(artist_mbid_lower)
            .fetch_optional(self.db.sqlx())
            .await
            .map_err(read_error("concerts skiddle resolution"))?;
        let Some(resolved_at) = marker else {
            return Ok(None);
        };
        let artist_ids = sqlx::query_scalar(sql::SKIDDLE_IDS.text())
            .bind(artist_mbid_lower)
            .fetch_all(self.db.sqlx())
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
        self.db
            .write(Lane::Background, "concerts tm resolution", move |tx| {
                tx.execute(
                    sql::SAVE_TM_RESOLUTION.text(),
                    params![artist, attraction_id, basis.as_str(), now],
                )?;
                Ok::<_, DbError>(())
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
        self.db
            .write(Lane::Background, "concerts skiddle resolution", move |tx| {
                tx.execute(sql::DELETE_SKIDDLE_IDS.text(), params![artist])?;
                for id in &artist_ids {
                    tx.execute(sql::INSERT_SKIDDLE_ID.text(), params![artist, id])?;
                }
                tx.execute(sql::SAVE_SKIDDLE_RESOLUTION.text(), params![artist, now])?;
                Ok::<_, DbError>(())
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
        let rows = sqlx::query(sql::EVENT_KEYS.text())
            .bind(artist_mbid_lower)
            .fetch_all(self.db.sqlx())
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
        self.db
            .write(Lane::Background, "concerts apply sweep", move |tx| {
                for (source, id) in &deletes {
                    tx.execute(
                        sql::DELETE_EVENT.text(),
                        params![source.as_str(), id, artist],
                    )?;
                }
                for row in &upserts {
                    tx.execute(
                        sql::UPSERT_EVENT.text(),
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
                Ok::<_, DbError>(())
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
        self.db
            .write(Lane::Background, "concerts record failure", move |tx| {
                upsert_cursor(tx, &artist, now, "error", Some(&error))?;
                Ok::<_, DbError>(())
            })
            .await
            .map_err(StoreError::from)
    }

    /// Delete rows dated strictly before `cutoff_local_date`.
    pub async fn prune_before(&self, cutoff_local_date: String) -> Result<usize, StoreError> {
        self.db
            .write(Lane::Background, "concerts prune", move |tx| {
                Ok::<_, DbError>(tx.execute(sql::PRUNE_EVENTS.text(), params![cutoff_local_date])?)
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
        let statement = sql::feed(user_id.is_some(), discovered_after.is_some());
        let mut query = sqlx::query(&statement);
        if let Some(user_id) = user_id {
            query = query.bind(user_id);
        }
        query = query.bind(min_local_date);
        if let Some(after) = discovered_after {
            query = query.bind(after);
        }
        let rows = query
            .fetch_all(self.db.sqlx())
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
        let rows = sqlx::query(sql::CITIES.text())
            .bind(user_id)
            .fetch_all(self.db.sqlx())
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
        self.db
            .write(Lane::Foreground, "concerts replace cities", move |tx| {
                tx.execute(sql::DELETE_CITIES.text(), params![user_id])?;
                for (position, city) in cities.iter().enumerate() {
                    tx.execute(
                        sql::INSERT_CITY.text(),
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
                Ok::<_, DbError>(())
            })
            .await
            .map_err(StoreError::from)
    }

    /// When the user last opened the events page; 0 for never.
    pub async fn seen_at(&self, user_id: &str) -> Result<f64, StoreError> {
        let seen: Option<f64> = sqlx::query_scalar(sql::SEEN_AT.text())
            .bind(user_id)
            .fetch_optional(self.db.sqlx())
            .await
            .map_err(read_error("concerts seen at"))?;
        Ok(seen.unwrap_or(0.0))
    }

    /// Stamp the user's seen marker.
    pub async fn mark_seen(&self, user_id: &str, now: f64) -> Result<(), StoreError> {
        let user_id = user_id.to_owned();
        self.db
            .write(Lane::Foreground, "concerts mark seen", move |tx| {
                tx.execute(sql::MARK_SEEN.text(), params![user_id, now])?;
                Ok::<_, DbError>(())
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
        sql::UPSERT_CURSOR.text(),
        params![artist_mbid_lower, now, status, error],
    )?;
    Ok(())
}
