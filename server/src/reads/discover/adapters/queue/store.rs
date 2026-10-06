//! The queue's durable state, in tables the baseline schema already has:
//! the per-user ignore ledger (`ignored_releases`), the last built deck
//! per user (`discovery_snapshots`, key `discover_queue:<user>`), and the
//! Last.fm release to release-group map (`mbid_resolution_map`).
//!
//! Reads use the reader pool; every write goes through the writer lane.
//! A saved deck is tied to the library catalog revision it was built
//! against, so a library change makes it read as missing (v2
//! `get_with_stale`).

use std::collections::{HashMap, HashSet};

use sqlx::SqlitePool;

use crate::db::{WriteLane, writer::Lane};
use crate::reads::discover::adapters::ownership;
use crate::reads::discover::models::{IgnoredRelease, QueueIgnoreRequest};

/// Snapshot key prefix the decks share with v2.
const DECK_KEY_PREFIX: &str = "discover_queue:";
/// Most ids one resolution read binds.
const MAX_BIND: usize = 500;

/// The queue's tables.
#[derive(Clone, Debug)]
pub struct QueueDb {
    pool: SqlitePool,
    lane: WriteLane,
}

/// A saved deck as stored, plus whether something marked it stale.
#[derive(Debug, Clone)]
pub struct SavedDeck {
    /// The stored JSON.
    pub payload: Vec<u8>,
    /// True when an invalidation marked the deck stale.
    pub stale: bool,
}

fn deck_key(user_id: &str) -> String {
    format!("{DECK_KEY_PREFIX}{user_id}")
}

impl QueueDb {
    /// Bind the tables over the reader pool and the writer lane.
    pub fn new(pool: SqlitePool, lane: WriteLane) -> Self {
        Self { pool, lane }
    }

    /// Lowercased release-group ids the user ignored.
    pub async fn ignored_mbids(&self, user_id: &str) -> Result<HashSet<String>, String> {
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT release_group_mbid_lower FROM ignored_releases WHERE user_id = ?1",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| format!("ignored releases read: {error}"))?;
        Ok(rows
            .into_iter()
            .map(|(mbid,)| mbid)
            .filter(|mbid| !mbid.is_empty())
            .collect())
    }

    /// The user's ignore ledger, newest first.
    pub async fn ignored(&self, user_id: &str) -> Result<Vec<IgnoredRelease>, String> {
        let rows: Vec<(String, String, String, String, f64)> = sqlx::query_as(
            "SELECT release_group_mbid, artist_mbid, release_name, artist_name, ignored_at \
             FROM ignored_releases WHERE user_id = ?1 ORDER BY ignored_at DESC",
        )
        .bind(user_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|error| format!("ignored releases read: {error}"))?;
        Ok(rows
            .into_iter()
            .map(
                |(release_group_mbid, artist_mbid, release_name, artist_name, ignored_at)| {
                    IgnoredRelease {
                        release_group_mbid,
                        artist_mbid,
                        release_name,
                        artist_name,
                        ignored_at: ignored_at as i64,
                    }
                },
            )
            .collect())
    }

    /// Record one ignore. Ignoring the same release again refreshes its
    /// names and time.
    pub async fn ignore(
        &self,
        user_id: &str,
        release: &QueueIgnoreRequest,
        at: f64,
    ) -> Result<(), String> {
        let user_id = user_id.to_owned();
        let release = release.clone();
        self.lane
            .write(Lane::Foreground, "discover.queue.ignore", move |tx| {
                tx.execute(
                    "INSERT INTO ignored_releases (user_id, release_group_mbid_lower, \
                         release_group_mbid, artist_mbid, release_name, artist_name, ignored_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                     ON CONFLICT(user_id, release_group_mbid_lower) DO UPDATE SET \
                         release_group_mbid = excluded.release_group_mbid, \
                         artist_mbid = excluded.artist_mbid, \
                         release_name = excluded.release_name, \
                         artist_name = excluded.artist_name, \
                         ignored_at = excluded.ignored_at",
                    rusqlite::params![
                        user_id,
                        release.release_group_mbid.trim().to_ascii_lowercase(),
                        release.release_group_mbid,
                        release.artist_mbid,
                        release.release_name,
                        release.artist_name,
                        at,
                    ],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| format!("ignore write: {error}"))
    }

    /// Drop ignores older than `before` (unix seconds). Returns how many
    /// went.
    pub async fn prune_ignored(&self, before: f64) -> Result<usize, String> {
        self.lane
            .write(
                Lane::Background,
                "discover.queue.prune_ignored",
                move |tx| {
                    Ok(tx.execute(
                        "DELETE FROM ignored_releases WHERE ignored_at < ?1",
                        rusqlite::params![before],
                    )?)
                },
            )
            .await
            .map_err(|error| format!("ignore prune: {error}"))
    }

    /// The user's saved deck, when it was built against the current
    /// library catalog revision.
    pub async fn load_deck(&self, user_id: &str) -> Result<Option<SavedDeck>, String> {
        let row: Option<(Vec<u8>, i64)> = sqlx::query_as(
            "SELECT snapshot.payload, snapshot.stale FROM discovery_snapshots snapshot \
             JOIN library_catalog_revision revision ON revision.singleton = 1 \
             WHERE snapshot.snapshot_key = ?1 AND snapshot.catalog_revision = revision.value",
        )
        .bind(deck_key(user_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(|error| format!("queue snapshot read: {error}"))?;
        Ok(row.map(|(payload, stale)| SavedDeck {
            payload,
            stale: stale != 0,
        }))
    }

    /// Save the user's deck against the current catalog revision.
    pub async fn save_deck(
        &self,
        user_id: &str,
        payload: Vec<u8>,
        saved_at: f64,
    ) -> Result<(), String> {
        let user_id = user_id.to_owned();
        self.lane
            .write(Lane::Background, "discover.queue.save_deck", move |tx| {
                let revision: i64 = tx.query_row(
                    "SELECT COALESCE((SELECT value FROM library_catalog_revision \
                     WHERE singleton = 1), 0)",
                    [],
                    |row| row.get(0),
                )?;
                tx.execute(
                    "INSERT INTO discovery_snapshots \
                         (snapshot_key, user_id, payload, saved_at, stale, catalog_revision) \
                     VALUES (?1, ?2, ?3, ?4, 0, ?5) \
                     ON CONFLICT(snapshot_key) DO UPDATE SET \
                         user_id = excluded.user_id, payload = excluded.payload, \
                         saved_at = excluded.saved_at, stale = 0, \
                         catalog_revision = excluded.catalog_revision",
                    rusqlite::params![deck_key(&user_id), user_id, payload, saved_at, revision],
                )?;
                Ok(())
            })
            .await
            .map_err(|error| format!("queue snapshot write: {error}"))
    }

    /// Known release to release-group answers for the given lowercased
    /// release ids. `None` values are remembered misses.
    pub async fn resolutions(
        &self,
        release_mbids: &[String],
    ) -> Result<HashMap<String, Option<String>>, String> {
        let mut found = HashMap::new();
        for chunk in release_mbids.chunks(MAX_BIND) {
            let sql = format!(
                "SELECT source_mbid_lower, release_group_mbid FROM mbid_resolution_map \
                 WHERE source_mbid_lower IN ({})",
                vec!["?"; chunk.len()].join(",")
            );
            let mut query = sqlx::query_as::<_, (String, Option<String>)>(&sql);
            for mbid in chunk {
                query = query.bind(mbid);
            }
            let rows = query
                .fetch_all(&self.pool)
                .await
                .map_err(|error| format!("resolution map read: {error}"))?;
            for (release, group) in rows {
                found.insert(release, group.filter(|group| !group.is_empty()));
            }
        }
        Ok(found)
    }

    /// Remember release to release-group answers (misses as `None`).
    pub async fn save_resolutions(&self, answers: Vec<(String, Option<String>)>) {
        if answers.is_empty() {
            return;
        }
        let written = self
            .lane
            .write(Lane::Background, "discover.queue.resolutions", move |tx| {
                for (release, group) in &answers {
                    tx.execute(
                        "INSERT INTO mbid_resolution_map \
                             (source_mbid_lower, source_mbid, release_group_mbid) \
                         VALUES (?1, ?1, ?2) \
                         ON CONFLICT(source_mbid_lower) DO UPDATE SET \
                             release_group_mbid = excluded.release_group_mbid",
                        rusqlite::params![release, group],
                    )?;
                }
                Ok(())
            })
            .await;
        if let Err(error) = written {
            tracing::warn!(%error, "could not save release group answers; they are looked up again next time");
        }
    }

    /// The given release groups (lowercased) that some library album is
    /// identified as.
    pub async fn owned(&self, release_group_mbids: &[String]) -> Result<HashSet<String>, String> {
        let wanted: Vec<&str> = release_group_mbids.iter().map(String::as_str).collect();
        ownership::owned_albums(&self.pool, &wanted)
            .await
            .map(|owned| owned.into_keys().collect())
            .map_err(|failure| failure.to_string())
    }
}
