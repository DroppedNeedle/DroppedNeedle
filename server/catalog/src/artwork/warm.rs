//! Background warming of the covers and artist images a person is about
//! to see.
//!
//! When a user's Discover or Home page is built, or an artist page, its
//! similar artists or a genre page is served, the page queues one [`KIND`]
//! job naming its release groups and artists. The job's handler fetches each one that is not
//! cached yet, at background priority, so the next visit draws them from
//! disk: covers at 500 px (the 250 is made from it), artists at the stored
//! original. The archive's limiter serves every page request first.
//!
//! The same kind fills TheAudioDB's artist answers, which a page reads from
//! the cache only: an artist image resolve that finds none queues one job
//! naming that artist. The handler asks TheAudioDB at background priority
//! and caches its answer; when it names a picture and the artist has no
//! stored image yet, the image is resolved again at once.
//!
//! A job is keyed by its page and a digest of its targets, so the same
//! list queued twice is one row. Queueing never waits: the write runs on
//! its own task, and one that fails is logged and dropped (the next build
//! queues again). A run is safe to repeat: what is cached is skipped, and
//! what cannot be fetched is remembered by the cache's own "no art"
//! markers, so the job always ends done. When TheAudioDB does not answer,
//! nothing is cached, so the next page to miss it queues the fill again.
//! The handler writes nothing to the database; a busy one only meets the
//! queue's own writes, which put the job back without spending an attempt.

use std::collections::HashSet;
use std::time::Duration;

use droppedneedle_kernel::db::{Db, Lane};
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::queue::{
    JobCtx, JobHandler, JobKind, JobQueue, KindSpec, Outcome, PAYLOAD_INVALID,
};
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::musicbrainz::is_valid_mbid;
use serde::{Deserialize, Serialize};

use super::ArtworkService;
use super::cache::sha256_hex;

/// The warm job kind.
pub const KIND: JobKind = JobKind::new("artwork.warm");

/// How warm jobs run: one long run fetching a page's images in turn, done
/// rows kept a day since every page build adds one.
pub const SPEC: KindSpec = KindSpec::new(KIND)
    .max_attempts(3)
    .run_timeout(Duration::from_secs(30 * 60))
    .keep_done(Duration::from_secs(24 * 3600))
    .keep_failed(Duration::from_secs(7 * 24 * 3600));

/// Longest a warm job waits for its page to finish fetching images.
const PAGE_WAIT: Duration = Duration::from_secs(10);

/// Release-group covers one job warms at most.
pub const MAX_COVERS: usize = 200;
/// Artist images one job warms at most.
pub const MAX_ARTISTS: usize = 100;

/// The images one page shows, in page order, deduplicated and capped.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WarmTargets {
    #[serde(default)]
    covers: Vec<String>,
    #[serde(default)]
    artists: Vec<String>,
    #[serde(default)]
    audiodb_artists: Vec<String>,
    #[serde(skip)]
    seen: HashSet<String>,
}

impl WarmTargets {
    /// Add a release group's cover. Ids that are not MBIDs are skipped.
    pub fn cover(&mut self, mbid: Option<&str>) {
        if self.covers.len() < MAX_COVERS
            && let Some(mbid) = self.fresh("rg", mbid)
        {
            self.covers.push(mbid);
        }
    }

    /// Add an artist's image. Ids that are not MBIDs are skipped.
    pub fn artist(&mut self, mbid: Option<&str>) {
        if self.artists.len() < MAX_ARTISTS
            && let Some(mbid) = self.fresh("artist", mbid)
        {
            self.artists.push(mbid);
        }
    }

    /// Add an artist whose TheAudioDB answer is to be fetched. Ids that
    /// are not MBIDs are skipped.
    pub fn audiodb_artist(&mut self, mbid: Option<&str>) {
        if self.audiodb_artists.len() < MAX_ARTISTS
            && let Some(mbid) = self.fresh("audiodb", mbid)
        {
            self.audiodb_artists.push(mbid);
        }
    }

    /// True when there is nothing to warm.
    pub fn is_empty(&self) -> bool {
        self.covers.is_empty() && self.artists.is_empty() && self.audiodb_artists.is_empty()
    }

    fn fresh(&mut self, kind: &str, mbid: Option<&str>) -> Option<String> {
        let mbid = mbid?.trim().to_ascii_lowercase();
        (is_valid_mbid(&mbid) && self.seen.insert(format!("{kind}:{mbid}"))).then_some(mbid)
    }
}

/// Queues warm jobs on the jobs table.
#[derive(Clone, Debug)]
pub struct CoverWarm {
    db: Db,
    queue: JobQueue,
}

impl CoverWarm {
    /// Warm jobs on `queue`, written through `db`.
    pub fn new(db: Db, queue: JobQueue) -> Self {
        Self { db, queue }
    }

    /// Queue a warm of `targets` for `page` (`discover`, `home`, `artist`,
    /// `genre`) without waiting for the write.
    pub fn queue(&self, page: &'static str, targets: WarmTargets) {
        if targets.is_empty() {
            return;
        }
        let Ok(encoded) = serde_json::to_vec(&targets) else {
            tracing::warn!(page, "cover warm targets did not encode; not queued");
            return;
        };
        let key = format!("{page}:{}", &sha256_hex(&encoded)[..32]);
        let (db, queue) = (self.db.clone(), self.queue.clone());
        tokio::spawn(async move {
            let queued = db
                .write(Lane::Background, "artwork-warm-enqueue", move |tx| {
                    queue.enqueue(tx, KIND, &key, &targets).map(|_| ())
                })
                .await;
            if let Err(error) = queued {
                tracing::warn!(%error, page, "cover warm not queued");
            }
        });
    }
}

/// Runs [`KIND`] jobs through the artwork service.
pub struct CoverWarmJobs {
    artwork: ArtworkService,
}

impl CoverWarmJobs {
    /// The handler, fetching through `artwork`.
    pub fn new(artwork: ArtworkService) -> Self {
        Self { artwork }
    }
}

impl JobHandler for CoverWarmJobs {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let artwork = self.artwork.clone();
        Box::pin(async move {
            let Ok(targets) = job.payload::<WarmTargets>() else {
                return Outcome::Fail {
                    code: PAYLOAD_INVALID,
                };
            };
            // Let the page fetch its own images first: the two together
            // make the bursts the providers refuse.
            artwork.after_page_fetches(PAGE_WAIT).await;
            let mut fetched = 0_usize;
            // One at a time: a fetch a page started for the same image
            // has usually landed by the time the next check runs.
            for mbid in &targets.covers {
                if !artwork.release_group_cover_cached(mbid).await {
                    artwork
                        .warm_release_group_cover(mbid, RequestPriority::BackgroundSync)
                        .await;
                    fetched += 1;
                }
            }
            for mbid in &targets.artists {
                if !artwork.artist_image_cached(mbid).await {
                    artwork.warm_artist_image(mbid).await;
                    fetched += 1;
                }
            }
            for mbid in &targets.audiodb_artists {
                artwork.fill_artist_image(mbid).await;
            }
            tracing::debug!(
                covers = targets.covers.len(),
                artists = targets.artists.len(),
                audiodb_artists = targets.audiodb_artists.len(),
                fetched,
                "page images warmed"
            );
            Outcome::Done
        })
    }
}
