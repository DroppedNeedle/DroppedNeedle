//! SQLite access for the concerts feed.
//!
//! Tables: `catalog_live_event_feed` is the shared feed, one row per
//! (source, listing, artist) so a festival bill shows up under every
//! followed co-headliner; `catalog_artist_event_checks` is the per-artist
//! sweep cursor; `catalog_artist_tm_attractions`, `catalog_artist_skiddle_ids`
//! and `catalog_artist_skiddle_resolutions` cache source resolutions
//! (negative ones too, so the seven-day TTL applies to "not on Ticketmaster"
//! as well); `catalog_event_cities` and `catalog_event_seen_marks` are per
//! user. Times are unix milliseconds.
//!
//! Reads go through the pool. Writes go through the writer lane: the sweep
//! on the background lane, user actions on the foreground lane. No network
//! call ever happens inside a write.

use std::collections::{HashMap, HashSet};

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension as _, params};

use super::matching::{EventRow, StoredConcert, SweepArtist, TmBasis};
use super::models::{EventCity, EventSource};
use super::sql;
use droppedneedle_kernel::db::time::Millis;
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
    /// Unix milliseconds.
    pub resolved_at: i64,
}

/// Cached Skiddle resolution for one artist. Empty ids mean no presence.
#[derive(Debug, Clone, PartialEq)]
pub struct SkiddleResolution {
    /// Every matching Skiddle artist id.
    pub artist_ids: Vec<String>,
    /// Unix milliseconds.
    pub resolved_at: i64,
}

fn stored_concert(row: sql::FeedColumns) -> StoredConcert {
    StoredConcert {
        event: EventRow {
            source: row.source,
            source_event_id: row.source_event_id,
            artist_mbid_lower: row.artist_mbid_lower,
            artist_name: row.artist_name,
            event_name: row.event_name,
            local_date: row.local_date,
            status: row.status,
            confidence: row.match_confidence,
            venue_name: row.venue_name,
            city: row.city,
            region: row.region,
            country_code: row.country_code,
            latitude: row.latitude,
            longitude: row.longitude,
            start_text: row.start_text,
            ticket_url: row.ticket_url,
        },
        artist_mbid: row.link_mbid,
    }
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

    /// Lowercase MBID to last check time in unix milliseconds, for
    /// least-recently-checked-first rotation. Never-checked artists are absent.
    pub async fn cursor_ages(&self) -> Result<HashMap<String, i64>, StoreError> {
        self.read("concerts cursor ages", |conn| {
            let ages = conn
                .prepare(sql::CURSOR_AGES.text())?
                .query_map([], sql::CursorAge::read)?
                .map(|row| row.map(|row| (row.artist_mbid_lower, row.last_checked_at.0)))
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
            basis: row.match_basis,
            resolved_at: row.resolved_at.0,
        }))
    }

    /// Cached Skiddle resolution, if any.
    pub async fn skiddle_resolution(
        &self,
        artist_mbid_lower: &str,
    ) -> Result<Option<SkiddleResolution>, StoreError> {
        let artist = artist_mbid_lower.to_owned();
        let marker: Option<Millis> = self
            .read("concerts skiddle resolution", move |conn| {
                let marker = conn
                    .query_row(sql::SKIDDLE_RESOLVED_AT.text(), params![artist], |row| {
                        row.get(0)
                    })
                    .optional()?;
                Ok(marker)
            })
            .await?;
        let Some(Millis(resolved_at)) = marker else {
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

    /// Cache a Ticketmaster resolution (negative results included), stamped
    /// `now` in unix milliseconds.
    pub async fn set_tm_resolution(
        &self,
        artist_mbid_lower: &str,
        attraction_id: Option<String>,
        basis: TmBasis,
        now: i64,
    ) -> Result<(), StoreError> {
        let artist = artist_mbid_lower.to_owned();
        self.db
            .write(Lane::Background, "concerts tm resolution", move |tx| {
                tx.execute(
                    sql::SAVE_TM_RESOLUTION.text(),
                    params![artist, attraction_id, basis.as_str(), Millis(now)],
                )?;
                Ok::<_, DbError>(())
            })
            .await
            .map_err(StoreError::from)
    }

    /// Replace the Skiddle id set and stamp the TTL marker (`now`, unix
    /// milliseconds) together. An empty set is a valid negative result.
    pub async fn set_skiddle_resolution(
        &self,
        artist_mbid_lower: &str,
        artist_ids: Vec<String>,
        now: i64,
    ) -> Result<(), StoreError> {
        let artist = artist_mbid_lower.to_owned();
        self.db
            .write(Lane::Background, "concerts skiddle resolution", move |tx| {
                tx.execute(sql::DELETE_SKIDDLE_IDS.text(), params![artist])?;
                for id in &artist_ids {
                    tx.execute(sql::INSERT_SKIDDLE_ID.text(), params![artist, id])?;
                }
                tx.execute(
                    sql::SAVE_SKIDDLE_RESOLUTION.text(),
                    params![artist, Millis(now)],
                )?;
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
            .map(|row| (row.source, row.source_event_id))
            .collect())
    }

    /// Apply one artist's sweep in one transaction: delete the vanished
    /// keys, upsert the current rows (keeping `discovered_at` on conflict),
    /// and stamp the artist's cursor ok, at `now` in unix milliseconds.
    pub async fn apply_sweep(
        &self,
        artist_mbid_lower: &str,
        upserts: Vec<EventRow>,
        deletes: Vec<(EventSource, String)>,
        now: i64,
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
                            row.start_text,
                            row.local_date,
                            row.status.as_str(),
                            row.ticket_url,
                            row.confidence.as_str(),
                            Millis(now),
                        ],
                    )?;
                }
                upsert_cursor(tx, &artist, now, CheckOutcome::Ok, None)?;
                Ok::<_, DbError>(())
            })
            .await
            .map_err(StoreError::from)
    }

    /// Record a failed check on the artist's cursor at `now` (unix
    /// milliseconds). The artist waits for the next sweep; there is no
    /// in-sweep retry.
    pub async fn record_failure(
        &self,
        artist_mbid_lower: &str,
        error: String,
        now: i64,
    ) -> Result<(), StoreError> {
        let artist = artist_mbid_lower.to_owned();
        self.db
            .write(Lane::Background, "concerts record failure", move |tx| {
                upsert_cursor(tx, &artist, now, CheckOutcome::Error, Some(&error))?;
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
    /// `discovered_after` keeps only rows first seen after that time (unix
    /// milliseconds).
    pub async fn concerts(
        &self,
        user_id: Option<&str>,
        min_local_date: &str,
        discovered_after: Option<i64>,
    ) -> Result<Vec<StoredConcert>, StoreError> {
        let statement = sql::feed(user_id.is_some(), discovered_after.is_some());
        let mut binds: Vec<Value> = Vec::with_capacity(3);
        if let Some(user_id) = user_id {
            binds.push(Value::Text(user_id.to_owned()));
        }
        binds.push(Value::Text(min_local_date.to_owned()));
        if let Some(after) = discovered_after {
            binds.push(Value::Integer(after));
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
        Ok(rows.into_iter().map(stored_concert).collect())
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

    /// When the user last opened the events page, in unix milliseconds; 0
    /// for never.
    pub async fn seen_at(&self, user_id: &str) -> Result<i64, StoreError> {
        let user_id = user_id.to_owned();
        let seen: Option<Millis> = self
            .read("concerts seen at", move |conn| {
                let seen = conn
                    .query_row(sql::SEEN_AT.text(), params![user_id], |row| row.get(0))
                    .optional()?;
                Ok(seen)
            })
            .await?;
        Ok(seen.map_or(0, |Millis(at)| at))
    }

    /// Stamp the user's seen marker at `now`, in unix milliseconds.
    pub async fn mark_seen(&self, user_id: &str, now: i64) -> Result<(), StoreError> {
        let user_id = user_id.to_owned();
        self.db
            .write(Lane::Foreground, "concerts mark seen", move |tx| {
                tx.execute(sql::MARK_SEEN.text(), params![user_id, Millis(now)])?;
                Ok::<_, DbError>(())
            })
            .await
            .map_err(StoreError::from)
    }
}

/// Add one city inside the offline importer's transaction, at the position
/// its plan chose. Writes the row and nothing else.
pub fn insert_city_in(
    tx: &rusqlite::Transaction<'_>,
    user_id: &str,
    city: &EventCity,
    position: i64,
) -> rusqlite::Result<()> {
    tx.execute(
        sql::INSERT_CITY.text(),
        params![
            user_id,
            city.city_name,
            city.country_code,
            city.latitude,
            city.longitude,
            city.radius_km,
            position,
        ],
    )?;
    Ok(())
}

/// Set when a user last looked at their concerts (unix milliseconds),
/// inside the offline importer's transaction. Writes the row and nothing
/// else.
pub fn set_seen_in(
    tx: &rusqlite::Transaction<'_>,
    user_id: &str,
    seen_at: i64,
) -> rusqlite::Result<()> {
    tx.execute(sql::MARK_SEEN.text(), params![user_id, Millis(seen_at)])?;
    Ok(())
}

/// How an artist's last check ended, as its cursor row stores it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    /// The sources answered.
    Ok,
    /// The check failed; its error text sits beside it.
    Error,
}

droppedneedle_kernel::spelled!(CheckOutcome {
    Ok => "ok",
    Error => "error",
});

fn upsert_cursor(
    tx: &rusqlite::Transaction<'_>,
    artist_mbid_lower: &str,
    now: i64,
    outcome: CheckOutcome,
    error: Option<&str>,
) -> rusqlite::Result<()> {
    tx.execute(
        sql::UPSERT_CURSOR.text(),
        params![artist_mbid_lower, Millis(now), outcome.as_str(), error],
    )?;
    Ok(())
}
