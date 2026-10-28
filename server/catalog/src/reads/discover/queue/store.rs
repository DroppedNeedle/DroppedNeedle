//! Discover's durable state: the per-user ignore ledger
//! (`catalog_ignored_releases`), the Last.fm release to release-group map
//! (`catalog_mbid_resolutions`) and the genres builds learned per artist
//! (`catalog_artist_genres`). The decks are rows of the page cache.
//!
//! Reads use the reader pool; every write goes through the writer lane.

use std::collections::{HashMap, HashSet};

use crate::reads::discover::models::{IgnoredRelease, QueueIgnoreRequest};
use crate::reads::discover::ownership;
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
        let rows: Vec<(String,)> = sqlx::query_as(
            "SELECT release_group_mbid_lower FROM catalog_ignored_releases WHERE user_id = ?1",
        )
        .bind(user_id)
        .fetch_all(self.db.sqlx())
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
        let rows: Vec<(String, String, String, String, i64)> = sqlx::query_as(
            "SELECT release_group_mbid, artist_mbid, release_name, artist_name, ignored_at \
             FROM catalog_ignored_releases WHERE user_id = ?1 ORDER BY ignored_at DESC",
        )
        .bind(user_id)
        .fetch_all(self.db.sqlx())
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
                        ignored_at: ignored_at / 1000,
                    }
                },
            )
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
                    "INSERT INTO catalog_ignored_releases (user_id, release_group_mbid_lower, \
                         release_group_mbid, artist_mbid, release_name, artist_name, ignored_at) \
                     SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7 \
                     WHERE EXISTS (SELECT 1 FROM auth_users WHERE id = ?1) \
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
            .map_err(|error: DbError| format!("ignore write: {error}"))
    }

    /// Drop ignores older than `before` (unix ms). Returns how many went.
    pub async fn prune_ignored(&self, before: i64) -> Result<usize, String> {
        self.db
            .write(
                Lane::Background,
                "discover.queue.prune_ignored",
                move |tx| {
                    Ok(tx.execute(
                        "DELETE FROM catalog_ignored_releases WHERE ignored_at < ?1",
                        rusqlite::params![before],
                    )?)
                },
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
            let sql = format!(
                "SELECT source_mbid_lower, release_group_mbid FROM catalog_mbid_resolutions \
                 WHERE source_mbid_lower IN ({})",
                vec!["?"; chunk.len()].join(",")
            );
            let mut query = sqlx::query_as::<_, (String, Option<String>)>(&sql);
            for mbid in chunk {
                query = query.bind(mbid);
            }
            let rows = query
                .fetch_all(self.db.sqlx())
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
        let written: Result<(), DbError> = self
            .db
            .write(Lane::Background, "discover.queue.resolutions", move |tx| {
                for (release, group) in &answers {
                    tx.execute(
                        "INSERT INTO catalog_mbid_resolutions \
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
        ownership::owned_albums(self.db.sqlx(), &wanted)
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
                            "INSERT INTO catalog_artist_genres \
                             (artist_mbid, genre_folded, genre, learned_at) \
                         VALUES (?1, ?2, ?3, ?4) \
                         ON CONFLICT(artist_mbid, genre_folded) DO UPDATE SET \
                             genre = excluded.genre, learned_at = excluded.learned_at",
                            rusqlite::params![artist, folded, genre, at],
                        )?;
                    }
                    tx.execute(
                        "DELETE FROM catalog_artist_genres WHERE learned_at < ?1",
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
