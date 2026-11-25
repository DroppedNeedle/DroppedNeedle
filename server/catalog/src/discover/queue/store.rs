//! Discover's durable state: the Last.fm release to release-group map
//! (`catalog_mbid_resolutions`) and the genres builds learned per artist
//! (`catalog_artist_genres`). What people told Discover is
//! [`feedback`](crate::discover::feedback)'s.
//!
//! Reads use the reader pool; every write goes through the writer lane.

use std::collections::{HashMap, HashSet};

use crate::discover::{ownership, read_all, sql};
use droppedneedle_kernel::db::statement::ListLen;
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

    /// The database, for the seed reads and the feedback.
    pub(in crate::discover) fn db(&self) -> &Db {
        &self.db
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
