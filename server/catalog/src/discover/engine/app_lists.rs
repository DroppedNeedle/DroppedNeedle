//! The `discover.app_lists` job: the playlists Discover keeps for a
//! person's apps (`crate::collections::discover_lists`), worked out from
//! their own stored rows and written wholesale.
//!
//! - "This week's set": the library's tracks of their newest weekly set,
//!   pick by pick. A pick is an album the library did not hold when it was
//!   dealt, so the list holds what has arrived since, and none goes in it
//!   before.
//! - "From your shelves": the tracks of the albums on their shelves
//!   ([`super::sources::shelves`]), played a lot and not lately first.
//! - One per Daily Mix kept for them ([`crate::discover::mixes`]), named
//!   as the mix: its library tracks in its order. Its new tracks are left
//!   out, as apps cannot play them, and so is what they hid from Discover.
//!
//! A list whose Discover section they switched off is not kept, and their
//! one switch (`catalog_discover_prefs.app_playlists`) turns them all off;
//! a list not kept is removed; one is made once it has songs and stays,
//! empty, when they all go. A run keeps nothing once the person forgot
//! their history after it began. It
//! is queued through [`Due`](crate::discover::due::Due): once a day with
//! their daily work and on a visit, when a new set or the day's mixes are
//! kept, when the switch moves and when their section switches are saved.
//! It reads nothing outside the database.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobHandler, JobKind, KindSpec, Outcome};

use super::sources::shelves;
use super::{sql, weekly_store};
use crate::collections::CollectionsState;
use crate::collections::db::StoreError;
use crate::collections::discover_lists::{self, DiscoverList, Wanted, Written};
use crate::discover::cache::ForgetCounts;
use crate::discover::feedback::Feedback;
use crate::discover::mixes;
use crate::discover::read_all;
use crate::discover::sections::{self, Page, SectionKey};

/// The job kind.
pub const KIND: JobKind = JobKind::new("discover.app_lists");

/// How the job runs: local reads and one write, so a few tries a minute
/// apart are enough.
pub const SPEC: KindSpec = KindSpec::new(KIND)
    .max_attempts(5)
    .backoff(Duration::from_secs(60), Duration::from_secs(3600))
    .run_timeout(Duration::from_secs(5 * 60))
    .keep_done(Duration::from_secs(2 * 24 * 3600))
    .keep_failed(Duration::from_secs(7 * 24 * 3600));

/// "This week's set", as the apps show it.
pub const WEEK_SET_NAME: &str = "This week's set";
/// "From your shelves", as the apps show it.
pub const SHELVES_NAME: &str = "From your shelves";

/// Runs the job.
#[derive(Clone)]
pub struct AppListJobs {
    db: Db,
    collections: CollectionsState,
    clock: Arc<dyn Clock>,
    forgets: ForgetCounts,
}

impl AppListJobs {
    /// The job over `db`, writing through `collections`, dated by `clock`;
    /// a forget moves `forgets` on.
    pub fn new(
        db: Db,
        collections: CollectionsState,
        clock: Arc<dyn Clock>,
        forgets: ForgetCounts,
    ) -> Self {
        Self {
            db,
            collections,
            clock,
            forgets,
        }
    }

    /// The lists `user_id` should have now; a list left out is not kept.
    async fn wanted(&self, user_id: &str) -> Result<Vec<Wanted>, DbError> {
        let prefs = weekly_store::prefs(&self.db, user_id)
            .await?
            .unwrap_or(weekly_store::Prefs::DEFAULT);
        if !prefs.app_playlists {
            return Ok(Vec::new());
        }
        let off = sections::read_switched_off(&self.db, user_id, Page::Discover).await?;
        let mut wanted = Vec::new();
        if !off.contains(&SectionKey::WeeklySet) {
            let tracks = read_all(
                &self.db,
                "discover.app_lists.set",
                sql::APP_LIST_SET_TRACKS.text(),
                [user_id.to_owned()],
                |row| row.get::<_, String>(0),
            )
            .await?;
            wanted.push(Wanted {
                list: DiscoverList::WeekSet,
                name: WEEK_SET_NAME.to_owned(),
                tracks: unique(tracks),
            });
        }
        if !off.contains(&SectionKey::FromYourShelves) {
            let albums = shelves::albums(&self.db, user_id, self.clock.now_ms()).await?;
            let albums = serde_json::to_string(&albums).unwrap_or_else(|_| "[]".to_owned());
            let tracks = read_all(
                &self.db,
                "discover.app_lists.shelves",
                sql::APP_LIST_ALBUM_TRACKS.text(),
                [albums],
                |row| row.get::<_, String>(0),
            )
            .await?;
            wanted.push(Wanted {
                list: DiscoverList::Shelves,
                name: SHELVES_NAME.to_owned(),
                tracks: unique(tracks),
            });
        }
        if !off.contains(&SectionKey::DailyMixes) {
            let hidden = Feedback::load(&self.db, user_id, self.clock.now_ms()).await?;
            for mix in mixes::store::kept(&self.db, user_id).await? {
                let Some(place) = mix
                    .id
                    .rsplit(':')
                    .next()
                    .and_then(|place| place.parse::<usize>().ok())
                else {
                    tracing::warn!("a kept mix's id names no place; its playlist is left out");
                    continue;
                };
                let tracks = mix
                    .tracks
                    .iter()
                    .filter(|track| {
                        !hidden.hides_album(
                            track.album.musicbrainz_id.as_deref(),
                            track.artist_mbid.as_deref(),
                        )
                    })
                    .filter_map(|track| track.local_track_id.clone())
                    .collect();
                wanted.push(Wanted {
                    list: DiscoverList::Mix(place),
                    name: mix.name,
                    tracks: unique(tracks),
                });
            }
        }
        Ok(wanted)
    }

    /// `user_id`'s forget count now, which a refresh begins from.
    pub fn forgets_now(&self, user_id: &str) -> u64 {
        self.forgets.of(user_id)
    }

    /// Write `user_id`'s lists as they should be now; `done` finishes the
    /// job in the same write. Safe to repeat: lists already right are left
    /// as they are.
    ///
    /// # Errors
    ///
    /// A read's or the write's failure.
    pub async fn refresh(
        &self,
        user_id: &str,
        done: impl FnOnce(&rusqlite::Transaction) -> Result<(), StoreError> + Send + 'static,
    ) -> Result<Written, StoreError> {
        self.refresh_from(user_id, self.forgets_now(user_id), done)
            .await
    }

    /// [`Self::refresh`] for a refresh that began when `user_id`'s forget
    /// count was `forgets_at`: it keeps nothing once the count moved.
    ///
    /// # Errors
    ///
    /// A read's or the write's failure.
    pub async fn refresh_from(
        &self,
        user_id: &str,
        forgets_at: u64,
        done: impl FnOnce(&rusqlite::Transaction) -> Result<(), StoreError> + Send + 'static,
    ) -> Result<Written, StoreError> {
        let wanted = self.wanted(user_id).await?;
        let (forgets, user) = (self.forgets.clone(), user_id.to_owned());
        discover_lists::write(
            &self.collections,
            user_id,
            self.clock.now_ms(),
            wanted,
            move || forgets.of(&user) == forgets_at,
            done,
        )
        .await
    }
}

/// `ids` with each id once, where it first comes.
fn unique(ids: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    ids.into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

impl JobHandler for AppListJobs {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let runner = self.clone();
        Box::pin(async move {
            let user_id = job.key().to_owned();
            match runner
                .refresh(&user_id, move |tx| Ok(job.complete(tx)?))
                .await
            {
                Ok(written) => {
                    if written != Written::default() {
                        tracing::info!(
                            created = written.created,
                            replaced = written.replaced,
                            removed = written.removed,
                            "discover's app playlists refreshed"
                        );
                    }
                    Outcome::Done
                }
                Err(StoreError::Busy(_)) => Outcome::Busy,
                Err(error) => {
                    tracing::warn!(%error, "discover's app playlists not refreshed; trying again");
                    Outcome::Retry {
                        code: "DISCOVER_APP_LISTS_FAILED",
                        at: None,
                    }
                }
            }
        })
    }
}
