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

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension as _, params};

use super::matching::{Confidence, EventRow, StoredConcert, SweepArtist, TmBasis};
use super::models::{ConcertStatus, EventCity, EventSource};
use super::sql;
use droppedneedle_kernel::db::{Db, DbError, Lane};

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

fn stored_concert(row: sql::FeedColumns) -> Option<StoredConcert> {
    let source = EventSource::parse(&row.source)?;
    Some(StoredConcert {
        event: EventRow {
            source,
            source_event_id: row.source_event_id,
            artist_mbid_lower: row.artist_mbid_lower,
            artist_name: row.artist_name,
            event_name: row.event_name,
            local_date: row.local_date,
            status: ConcertStatus::parse(&row.status),
            confidence: if row.match_confidence == "mbid" {
                Confidence::Mbid
            } else {
                Confidence::Name
            },
            venue_name: row.venue_name,
            city: row.city,
            region: row.region,
            country_code: row.country_code,
            latitude: row.latitude,
            longitude: row.longitude,
            starts_at: row.starts_at,
            ticket_url: row.ticket_url,
        },
        artist_mbid: row.link_mbid,
    })
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

    async fn read<R, F>(&self, name: &'static str, op: F) -> Result<R, StoreError>
    where
        F: FnOnce(&Connection) -> Result<R, DbError> + Send + 'static,
        R: Send + 'static,
    {
        self.db.read(name, op).await.map_err(StoreError::from)
    }

    // -- sweep set -----------------------------------------------------------

    /// Every distinct followed artist, keyed by lowercase MBID.
    pub async fn followed_artists(&self) -> Result<Vec<SweepArtist>, StoreError> {
        self.read("concerts followed artists", |conn| {
            let artists = conn
                .prepare(sql::FOLLOWED_ARTISTS.text())?
                .query_map([], sql::FollowedArtist::read)?
                .map(|row| {
                    row.map(|row| SweepArtist {
                        mbid: row.artist_mbid,
                        mbid_lower: row.artist_mbid_lower,
                        name: row.artist_name,
                    })
                })
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(artists)
        })
        .await
    }

    /// Every library artist with a MusicBrainz identity that leads or is
    /// credited on a live album with indexed tracks (library sweep scope).
    pub async fn library_artists(&self) -> Result<Vec<SweepArtist>, StoreError> {
        self.read("concerts library artists", |conn| {
            let artists = conn
                .prepare(sql::LIBRARY_ARTISTS.text())?
                .query_map([], sql::LibraryArtist::read)?
                .map(|row| {
                    row.map(|row| SweepArtist {
                        mbid: row.mbid,
                        mbid_lower: row.mbid_lower,
                        name: row.name,
                    })
                })
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(artists)
        })
        .await
    }

    /// Lowercase MBID to last check time, for least-recently-checked-first
    /// rotation. Never-checked artists are absent.
    pub async fn cursor_ages(&self) -> Result<HashMap<String, f64>, StoreError> {
        self.read("concerts cursor ages", |conn| {
            let ages = conn
                .prepare(sql::CURSOR_AGES.text())?
                .query_map([], sql::CursorAge::read)?
                .map(|row| row.map(|row| (row.artist_mbid_lower, row.last_checked_at)))
                .collect::<rusqlite::Result<HashMap<_, _>>>()?;
            Ok(ages)
        })
        .await
    }

    /// Users following an artist, for the `concerts_new` fan-out.
    pub async fn followers(&self, artist_mbid_lower: &str) -> Result<Vec<String>, StoreError> {
        let artist = artist_mbid_lower.to_owned();
        self.read("concerts followers", move |conn| {
            let users = conn
                .prepare(sql::FOLLOWERS.text())?
                .query_map(params![artist], |row| row.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(users)
        })
        .await
    }

    // -- source resolutions --------------------------------------------------

    /// Cached Ticketmaster resolution, if any.
    pub async fn tm_resolution(
        &self,
        artist_mbid_lower: &str,
    ) -> Result<Option<TmResolution>, StoreError> {
        let artist = artist_mbid_lower.to_owned();
        let row = self
            .read("concerts tm resolution", move |conn| {
                let row = conn
                    .query_row(
                        sql::TM_RESOLUTION.text(),
                        params![artist],
                        sql::TmColumns::read,
                    )
                    .optional()?;
                Ok(row)
            })
            .await?;
        Ok(row.map(|row| TmResolution {
            attraction_id: row.attraction_id,
            basis: TmBasis::parse(&row.match_basis),
            resolved_at: row.resolved_at,
        }))
    }

    /// Cached Skiddle resolution, if any.
    pub async fn skiddle_resolution(
        &self,
        artist_mbid_lower: &str,
    ) -> Result<Option<SkiddleResolution>, StoreError> {
        let artist = artist_mbid_lower.to_owned();
        let marker: Option<f64> = self
            .read("concerts skiddle resolution", move |conn| {
                let marker = conn
                    .query_row(sql::SKIDDLE_RESOLVED_AT.text(), params![artist], |row| {
                        row.get(0)
                    })
                    .optional()?;
                Ok(marker)
            })
            .await?;
        let Some(resolved_at) = marker else {
            return Ok(None);
        };
        let artist = artist_mbid_lower.to_owned();
        let artist_ids = self
            .read("concerts skiddle ids", move |conn| {
                let ids = conn
                    .prepare(sql::SKIDDLE_IDS.text())?
                    .query_map(params![artist], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok(ids)
            })
            .await?;
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
        let artist = artist_mbid_lower.to_owned();
        let rows = self
            .read("concerts event keys", move |conn| {
                let rows = conn
                    .prepare(sql::EVENT_KEYS.text())?
                    .query_map(params![artist], sql::EventKey::read)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|row| Some((EventSource::parse(&row.source)?, row.source_event_id)))
            .collect())
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
        let mut binds: Vec<Value> = Vec::with_capacity(3);
        if let Some(user_id) = user_id {
            binds.push(Value::Text(user_id.to_owned()));
        }
        binds.push(Value::Text(min_local_date.to_owned()));
        if let Some(after) = discovered_after {
            binds.push(Value::Real(after));
        }
        let rows = self
            .read("concerts list", move |conn| {
                let rows = conn
                    .prepare(&statement)?
                    .query_map(rusqlite::params_from_iter(binds), sql::FeedColumns::read)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            })
            .await?;
        Ok(rows.into_iter().filter_map(stored_concert).collect())
    }

    // -- per user ------------------------------------------------------------

    /// The user's cities in picker order.
    pub async fn cities(&self, user_id: &str) -> Result<Vec<EventCity>, StoreError> {
        let user_id = user_id.to_owned();
        self.read("concerts cities", move |conn| {
            let cities = conn
                .prepare(sql::CITIES.text())?
                .query_map(params![user_id], sql::CityColumns::read)?
                .map(|row| {
                    row.map(|row| EventCity {
                        city_name: row.city_name,
                        latitude: row.latitude,
                        longitude: row.longitude,
                        radius_km: row.radius_km,
                        country_code: row.country_code,
                    })
                })
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(cities)
        })
        .await
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
        let user_id = user_id.to_owned();
        let seen: Option<f64> = self
            .read("concerts seen at", move |conn| {
                let seen = conn
                    .query_row(sql::SEEN_AT.text(), params![user_id], |row| row.get(0))
                    .optional()?;
                Ok(seen)
            })
            .await?;
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
