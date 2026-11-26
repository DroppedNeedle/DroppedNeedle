//! A person's taste as their page shows it: the groups the taste job kept,
//! each with its artists by plays and what the person said about each,
//! what the taste was worked out from, and whether their picks are still
//! catching up: a tuning they made is newer than their pool build, or
//! their taste or pool build is queued or running (an undo leaves no row
//! to date it by). Only their own rows are read. Nothing here writes: the
//! groups are the job's, and statements are tunings
//! ([`crate::discover::tunings`]).

use std::collections::HashMap;

use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_kernel::jobs::queue::{JobQueue, State};
use rusqlite::OptionalExtension;

use crate::discover::engine::{jobs, pool, sql};
use crate::discover::models::{
    DiscoverTasteResponse, TasteArtist, TasteArtistTuning, TasteGroup, TasteMadeFrom,
};

/// Reads each person's taste page.
#[derive(Clone)]
pub struct TastePage {
    db: Db,
    queue: JobQueue,
}

impl TastePage {
    /// The page over `db`, reading the builds' jobs from `queue`.
    pub fn new(db: Db, queue: JobQueue) -> Self {
        Self { db, queue }
    }

    /// Whether `user_id`'s taste or pool build is queued or running.
    async fn building(&self, user_id: &str) -> Result<bool, DbError> {
        for kind in [jobs::KIND, pool::KIND] {
            if let Some(job) = self.queue.latest(kind, user_id).await?
                && matches!(job.state, State::Queued | State::Running)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// `user_id`'s taste: empty, and not updating, before their first
    /// taste build.
    ///
    /// # Errors
    ///
    /// The read's failure.
    pub async fn read(&self, user_id: &str) -> Result<DiscoverTasteResponse, DbError> {
        let mut page = self.rows(user_id).await?;
        if page.made_from.is_some() && !page.updating {
            page.updating = self.building(user_id).await?;
        }
        Ok(page)
    }

    /// `user_id`'s taste as their rows hold it.
    async fn rows(&self, user_id: &str) -> Result<DiscoverTasteResponse, DbError> {
        let user = user_id.to_owned();
        self.db
            .read("discover.taste_page", move |conn| {
                // A request-path read whose statements must agree: the build
                // row, its groups, their artists and the tunings shown on
                // them are one taste build.
                let Some(build) = conn
                    .prepare_cached(sql::TASTE_PAGE_BUILD.text())?
                    .query_row([&user], sql::TastePageBuild::read)
                    .optional()?
                else {
                    return Ok(DiscoverTasteResponse {
                        made_from: None,
                        groups: Vec::new(),
                        updating: false,
                    });
                };
                let mut tunings: HashMap<String, TasteArtistTuning> = HashMap::new();
                for row in conn
                    .prepare_cached(sql::TASTE_PAGE_TUNINGS.text())?
                    .query_map([&user], sql::TastePageTuning::read)?
                {
                    let row = row?;
                    tunings.entry(row.target).or_insert(TasteArtistTuning {
                        kind: row.kind,
                        group_anchor_mbid: row.group_anchor_mbid,
                        group_label: row.group_label,
                    });
                }
                let mut artists: HashMap<i64, Vec<TasteArtist>> = HashMap::new();
                for row in conn
                    .prepare_cached(sql::TASTE_PAGE_ARTISTS.text())?
                    .query_map([&user], sql::TastePageArtist::read)?
                {
                    let row = row?;
                    artists.entry(row.group_no).or_default().push(TasteArtist {
                        tuning: tunings.remove(&row.artist_mbid_lower),
                        artist_mbid: row.artist_mbid_lower,
                        name: row.name,
                        plays: row.plays,
                        imported_listens: row.imported_listens,
                        is_seed: row.is_seed,
                    });
                }
                let groups = conn
                    .prepare_cached(sql::TASTE_PAGE_GROUPS.text())?
                    .query_map([&user], sql::TastePageGroup::read)?
                    .map(|row| {
                        row.map(|row| TasteGroup {
                            artists: artists.remove(&row.group_no).unwrap_or_default(),
                            group_no: row.group_no,
                            anchor_artist_mbid: row.anchor_mbid,
                            anchor_name: row.anchor_name,
                            share: row.share,
                            trend: row.trend,
                            last_played_at: row.last_played_at.map(|at| at.0 / 1000),
                        })
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(DiscoverTasteResponse {
                    made_from: Some(TasteMadeFrom {
                        plays_counted: build.plays_counted,
                        imported_listens_counted: build.imported_listens_counted,
                        history_from_at: build.history_from_at.map(|at| at.0 / 1000),
                    }),
                    groups,
                    updating: build.updating,
                })
            })
            .await
    }
}
