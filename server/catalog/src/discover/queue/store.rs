//! Discover's durable state: the per-user ignore ledger
//! (`catalog_ignored_releases`), the Last.fm release to release-group map
//! (`catalog_mbid_resolutions`) and the genres builds learned per artist
//! (`catalog_artist_genres`). The decks are rows of the page cache.
//!
//! Reads use the reader pool; every write goes through the writer lane.

use std::collections::{HashMap, HashSet};

use crate::discover::models::{IgnoredRelease, QueueIgnoreRequest};
use crate::discover::{ownership, read_all, sql};
use droppedneedle_kernel::db::statement::ListLen;
use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};

/// Most ids one resolution read binds.
const MAX_BIND: usize = 500;

/// Discover's tables.
#[derive(Clone, Debug)]
pub struct QueueDb {
    db: Db,
}

impl QueueDb {
    /// Bind the tables over the database.
    pub fn new(db: Db) -> Self {
        Self { db }
    }

    /// Lowercased release-group ids the user ignored.
    pub async fn ignored_mbids(&self, user_id: &str) -> Result<HashSet<String>, String> {
        let rows: Vec<String> = read_all(
            &self.db,
            "discover.queue.ignored_mbids",
            sql::IGNORED_MBIDS.text(),
            (user_id.to_owned(),),
            |row| row.get(0),
        )
        .await
        .map_err(|error| format!("ignored releases read: {error}"))?;
        Ok(rows.into_iter().filter(|mbid| !mbid.is_empty()).collect())
    }

    /// The user's ignore ledger, newest first.
    pub async fn ignored(&self, user_id: &str) -> Result<Vec<IgnoredRelease>, String> {
        let rows = read_all(
            &self.db,
            "discover.queue.ignored",
            sql::IGNORED_RELEASES.text(),
            (user_id.to_owned(),),
            sql::IgnoredColumns::read,
        )
        .await
        .map_err(|error| format!("ignored releases read: {error}"))?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let Millis(ignored_at) = row.ignored_at;
                IgnoredRelease {
                    release_group_mbid: row.release_group_mbid,
                    artist_mbid: row.artist_mbid,
                    release_name: row.release_name,
                    artist_name: row.artist_name,
                    ignored_at: ignored_at / 1000,
                }
            })
            .collect())
    }

    /// Record one ignore at `at` (unix ms). Ignoring the same release
    /// again refreshes its names and time. Nothing is recorded for a user
    /// who no longer exists.
    pub async fn ignore(
        &self,
        user_id: &str,
        release: &QueueIgnoreRequest,
        at: i64,
    ) -> Result<(), String> {
        let user_id = user_id.to_owned();
        let release = release.clone();
        self.db
            .write(Lane::Foreground, "discover.queue.ignore", move |tx| {
                tx.execute(
                    sql::IGNORE_RELEASE.text(),
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
            .map_err(|error: DbError| format!("ignore write: {error}"))
    }

    /// Remove one of the user's ignores. Later decks may hold the album
    /// again. Nothing happens when the user has no such ignore.
    pub async fn unignore(
        &self,
        user_id: &str,
        release_group_mbid_lower: &str,
    ) -> Result<(), String> {
        let user_id = user_id.to_owned();
        let release_group_mbid_lower = release_group_mbid_lower.to_owned();
        self.db
            .write(Lane::Foreground, "discover.queue.unignore", move |tx| {
                tx.execute(
                    sql::UNIGNORE_RELEASE.text(),
                    rusqlite::params![user_id, release_group_mbid_lower],
                )?;
                Ok(())
            })
            .await
            .map_err(|error: DbError| format!("unignore write: {error}"))
    }

    /// Drop ignores older than `before` (unix ms). Returns how many went.
    pub async fn prune_ignored(&self, before: i64) -> Result<usize, String> {
        self.db
            .write(
                Lane::Background,
                "discover.queue.prune_ignored",
                move |tx| Ok(tx.execute(sql::PRUNE_IGNORED.text(), rusqlite::params![before])?),
            )
            .await
            .map_err(|error: DbError| format!("ignore prune: {error}"))
    }

    /// Known release to release-group answers for the given lowercased
    /// release ids. `None` values are remembered misses.
    pub async fn resolutions(
        &self,
        release_mbids: &[String],
    ) -> Result<HashMap<String, Option<String>>, String> {
        let mut found = HashMap::new();
        for chunk in release_mbids.chunks(MAX_BIND) {
            let rows = read_all(
                &self.db,
                "discover.queue.resolutions_read",
                sql::resolutions(ListLen::new(chunk.len())),
                rusqlite::params_from_iter(chunk.to_vec()),
                sql::ResolutionColumns::read,
            )
            .await
            .map_err(|error| format!("resolution map read: {error}"))?;
            for row in rows {
                found.insert(
                    row.source_mbid_lower,
                    row.release_group_mbid.filter(|group| !group.is_empty()),
                );
            }
        }
        Ok(found)
    }

    /// Remember one release to release-group answer (a miss as `None`).
    pub async fn save_resolution(&self, release: String, group: Option<String>) {
        let written: Result<(), DbError> = self
            .db
            .write(Lane::Background, "discover.queue.resolutions", move |tx| {
                tx.execute(
                    sql::SAVE_RESOLUTION.text(),
                    rusqlite::params![release, group],
                )?;
                Ok(())
            })
            .await;
        if let Err(error) = written {
            tracing::warn!(%error, "could not save a release group answer; it is looked up again next time");
        }
    }

    /// The given release groups (lowercased) that some library album is
    /// identified as.
    pub async fn owned(&self, release_group_mbids: &[String]) -> Result<HashSet<String>, String> {
        let wanted: Vec<&str> = release_group_mbids.iter().map(String::as_str).collect();
        ownership::owned_albums(&self.db, &wanted)
            .await
            .map(|owned| owned.into_keys().collect())
            .map_err(|failure| failure.to_string())
    }

    /// Remember genres learned per artist (lowercased MBID, genre name)
    /// at `at` (unix ms), and drop pairs no build has seen since
    /// `forget_before`.
    pub async fn learn_genres(&self, learned: Vec<(String, String)>, at: i64, forget_before: i64) {
        let written: Result<(), DbError> = self
            .db
            .write(
                Lane::Background,
                "discover.artist_genres.learn",
                move |tx| {
                    for (artist, genre) in &learned {
                        let genre = genre.trim();
                        let folded = genre.to_lowercase();
                        if artist.is_empty() || folded.is_empty() {
                            continue;
                        }
                        tx.execute(
                            sql::LEARN_ARTIST_GENRE.text(),
                            rusqlite::params![artist, folded, genre, at],
                        )?;
                    }
                    tx.execute(
                        sql::FORGET_ARTIST_GENRES.text(),
                        rusqlite::params![forget_before],
                    )?;
                    Ok(())
                },
            )
            .await;
        if let Err(error) = written {
            tracing::warn!(%error, "could not save learned artist genres; the next build learns them again");
        }
    }
}
