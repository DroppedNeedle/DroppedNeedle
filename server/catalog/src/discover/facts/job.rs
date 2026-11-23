//! The kept facts' refresh: one job kind ([`KIND`], one key for the
//! install) that makes the background reads, queued by the demand tick
//! and a person's visit through [`Due`](crate::discover::due::Due) while
//! [`FactsDue::waiting`] says there is work and room in the day.
//!
//! A run reads at most [`RUN_READS`] times, and no more than the day has
//! left under the ceiling, then finishes; the next tick queues it again.
//! It reads wanted facts first (a build's seeds and what ranks them, then
//! the albums of the artists it reads, then the library's), then the
//! oldest kept facts past their age that someone read in the last seven
//! days. Listener counts go in one read of up to 500 artists, album counts
//! in one of up to 347 groups across artists. Each read that goes out is
//! counted, and a failed one marks its fact failed, so it backs off; one
//! the client turns down unsent is not counted. While labs rests after a
//! failure no list is asked for: the job plans no list, and the
//! tick queues nothing for lists alone. Unread facts are dropped once a UTC
//! day. With nobody seen in seven days it reads nothing; with
//! ListenBrainz reads off it reads nothing from ListenBrainz; with the
//! MusicBrainz settings unreadable it reads no album list. A run is safe
//! to repeat.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::jobs::BoxFuture as JobFuture;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobError, JobHandler, JobKind, KindSpec, Outcome};
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::listenbrainz::{
    ARTIST_POPULARITY_BATCH, ArtistPopularity, RELEASE_GROUP_POPULARITY_BATCH, ReleaseGroupCounts,
    ScoredSimilarArtist,
};

use super::plan::{self, RUN_READS};
use super::{DAY_MS, FactKind, FactRef, KeptFacts, MeteredSource, ReadError};
use crate::discover::listening::albums::{self, AlbumReads};
use crate::discover::listening::similar::{self, SimilarReads};
use crate::discover::listening::{ListeningError, ListeningNetwork, SourceContext};
use crate::discover::ports::BoxFuture;
use crate::metadata::artist::ReleaseGroupItem;

/// The refresh job kind.
pub const KIND: JobKind = JobKind::new("discover.facts");
/// Its one key: the facts are the install's.
pub const KEY: &str = "install";

/// How refresh jobs run: about a hundred paced reads, done rows kept an
/// hour since a run is queued again on most ticks while work waits.
pub const SPEC: KindSpec = KindSpec::new(KIND)
    .max_attempts(3)
    .run_timeout(Duration::from_secs(10 * 60))
    .keep_done(Duration::from_secs(3600))
    .keep_failed(Duration::from_secs(7 * 24 * 3600));

/// Every read the job makes goes out in the background.
const PRIORITY: RequestPriority = RequestPriority::BackgroundSync;
/// How many facts a run plans from.
const PLANNED: usize = 1_000;
/// How many times a run plans: lists, then their counts, then the rest.
const ROUNDS: usize = 3;

/// The job's reads through the listening network, each one that went out
/// counted. A discography read is counted by the catalog, page by page, as
/// it sends them: one the catalog answers from what it holds sends none.
struct CountedReads {
    network: Arc<dyn ListeningNetwork>,
    facts: KeptFacts,
    /// Whether discography reads go to the official MusicBrainz service.
    musicbrainz_metered: bool,
}

impl CountedReads {
    async fn counted<T>(
        &self,
        source: MeteredSource,
        result: Result<T, ListeningError>,
    ) -> Result<T, ListeningError> {
        if !result.as_ref().is_err_and(ReadError::unsent) {
            self.facts.count_read(source, 1).await;
        }
        result
    }
}

impl SimilarReads for CountedReads {
    type Error = ListeningError;

    fn labs<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<ScoredSimilarArtist>, ListeningError>> {
        Box::pin(async move {
            let read = self.network.scored_similar(artist_mbid, priority).await;
            self.counted(MeteredSource::Labs, read).await
        })
    }

    fn listeners<'a>(
        &'a self,
        artist_mbids: &'a [String],
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<HashMap<String, ArtistPopularity>, ListeningError>> {
        Box::pin(async move {
            let read = self.network.artist_listeners(artist_mbids, priority).await;
            self.counted(MeteredSource::ListenBrainz, read).await
        })
    }

    fn labs_rest(&self) -> Option<Duration> {
        self.network.labs_rest()
    }
}

impl AlbumReads for CountedReads {
    type Error = ListeningError;

    fn discography<'a>(
        &'a self,
        artist_mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<Vec<ReleaseGroupItem>>, ListeningError>> {
        Box::pin(self.network.discography(artist_mbid, priority))
    }

    fn counts<'a>(
        &'a self,
        release_group_mbids: &'a [String],
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<HashMap<String, ReleaseGroupCounts>, ListeningError>> {
        Box::pin(async move {
            let read = self
                .network
                .release_group_counts(release_group_mbids, priority)
                .await;
            self.counted(MeteredSource::ListenBrainz, read).await
        })
    }
}

/// When labs reads stop resting, unix ms: set from the client's own
/// answer while it rests after a failure. Shared by the job and the tick.
type LabsRest = Arc<Mutex<i64>>;

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The kinds the job may read now: none of ListenBrainz's while its reads
/// are off, no list while labs rests, no album list while the MusicBrainz
/// settings do not read.
fn kinds(
    network: &dyn ListeningNetwork,
    facts: &KeptFacts,
    labs_rest: &LabsRest,
) -> (Vec<FactKind>, SourceContext) {
    let source = network.source();
    let musicbrainz_on = source.mode != SourceContext::unreadable().mode;
    let resting = facts.now() < *lock(labs_rest);
    let mut kinds = plan::kinds(network.listenbrainz_ready(), musicbrainz_on);
    if resting {
        kinds.retain(|kind| *kind != FactKind::Neighbours);
    }
    (kinds, source)
}

/// What the demand tick asks before it queues the refresh job.
#[derive(Clone)]
pub struct FactsDue {
    facts: KeptFacts,
    network: Arc<dyn ListeningNetwork>,
    labs_rest: LabsRest,
}

impl FactsDue {
    /// Whether a fact the job may read waits and the day has room. A
    /// failed read is logged and answers no; the next tick asks again.
    pub(crate) async fn waiting(&self) -> bool {
        let (kinds, _) = kinds(self.network.as_ref(), &self.facts, &self.labs_rest);
        plan::work_waiting(&self.facts, &kinds)
            .await
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "kept facts unreadable; their refresh not queued");
                false
            })
    }
}

/// What a run may still read: the run's own reads and the day's room.
struct Budget {
    run: u64,
    day: u64,
}

impl Budget {
    /// Whether one more read fits; `metered` reads also spend the day's.
    fn take(&mut self, metered: bool) -> bool {
        if self.run == 0 || (metered && self.day == 0) {
            return false;
        }
        self.run -= 1;
        if metered {
            self.day -= 1;
        }
        true
    }
}

/// Runs [`KIND`] jobs over the listening network.
#[derive(Clone)]
pub struct FactJobs {
    facts: KeptFacts,
    network: Arc<dyn ListeningNetwork>,
    /// The UTC day the ceiling was last met and logged.
    ceiling_logged: Arc<Mutex<i64>>,
    /// The UTC day unread facts were last dropped.
    swept: Arc<Mutex<i64>>,
    labs_rest: LabsRest,
}

impl FactJobs {
    /// The handler, keeping facts in `facts` read through `network`.
    pub fn new(facts: KeptFacts, network: Arc<dyn ListeningNetwork>) -> Self {
        Self {
            facts,
            network,
            ceiling_logged: Arc::new(Mutex::new(i64::MIN)),
            swept: Arc::new(Mutex::new(i64::MIN)),
            labs_rest: Arc::new(Mutex::new(i64::MIN)),
        }
    }

    /// The tick's check, sharing what this handler learns of labs' rest.
    pub fn due(&self) -> FactsDue {
        FactsDue {
            facts: self.facts.clone(),
            network: Arc::clone(&self.network),
            labs_rest: Arc::clone(&self.labs_rest),
        }
    }

    fn today(&self) -> i64 {
        self.facts.now().div_euclid(DAY_MS)
    }

    /// Drop the facts nothing has read for 180 days, once a UTC day.
    async fn sweep(&self) {
        let today = self.today();
        if *lock(&self.swept) == today {
            return;
        }
        match self.facts.drop_unread().await {
            Ok(()) => *lock(&self.swept) = today,
            Err(error) => tracing::warn!(%error, "unread kept facts not dropped; trying next run"),
        }
    }

    /// Log once a day that the day's reads are spent.
    fn log_ceiling(&self) {
        let today = self.today();
        let mut logged = lock(&self.ceiling_logged);
        if *logged != today {
            *logged = today;
            tracing::info!("discover's background reads met today's ceiling; resuming tomorrow");
        }
    }

    /// One run: plan, read under the run's and the day's limits, keep what
    /// answered. Answers how many reads it made. `stopped` says when to end
    /// early (a shutdown or a lost lease).
    ///
    /// # Errors
    ///
    /// The database's failure.
    pub async fn run_once(
        &self,
        stopped: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<u64, DbError> {
        let (mut kinds, source) = kinds(self.network.as_ref(), &self.facts, &self.labs_rest);
        if kinds.is_empty() {
            return Ok(0);
        }
        self.sweep().await;
        let room = plan::room(&self.facts).await?;
        if room == 0 {
            if self.facts.seen().await? > 0 {
                self.log_ceiling();
            }
            return Ok(0);
        }
        let reads = CountedReads {
            network: Arc::clone(&self.network),
            facts: self.facts.clone(),
            musicbrainz_metered: source.mode == "official",
        };
        let mut budget = Budget {
            run: RUN_READS,
            day: room,
        };
        let mut made = 0;
        // Labs rests after a failed read, so the run asks it nothing more.
        let mut labs_failed = false;
        // A list read wants its neighbours' counts and an album list its
        // groups' counts, so a run plans again while it has reads left.
        for _ in 0..ROUNDS {
            if labs_failed {
                kinds.retain(|kind| *kind != FactKind::Neighbours);
            }
            let work = self.facts.to_read(&kinds, PLANNED).await?;
            let this_round = self
                .round(&reads, &work, (&mut budget, &mut labs_failed), stopped)
                .await?;
            made += this_round;
            if this_round == 0 || budget.run == 0 || stopped() {
                break;
            }
        }
        Ok(made)
    }

    /// One pass over `work` in plan order. Answers how many reads it made.
    async fn round(
        &self,
        reads: &CountedReads,
        work: &[FactRef],
        (budget, labs_failed): (&mut Budget, &mut bool),
        stopped: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<u64, DbError> {
        let mut done: HashSet<usize> = HashSet::new();
        let mut made = 0;
        for at in 0..work.len() {
            if stopped() || budget.run == 0 {
                break;
            }
            if done.contains(&at) {
                continue;
            }
            let fact = &work[at];
            match fact.kind {
                FactKind::Neighbours => {
                    done.insert(at);
                    if *labs_failed {
                        continue;
                    }
                    // While labs rests nothing is asked: the run and the
                    // tick plan no list until it is over.
                    if let Some(wait) = reads.labs_rest() {
                        *labs_failed = true;
                        let wait = i64::try_from(wait.as_millis()).unwrap_or(i64::MAX);
                        *lock(&self.labs_rest) = self.facts.now().saturating_add(wait);
                        continue;
                    }
                    if !budget.take(true) {
                        continue;
                    }
                    made += 1;
                    if let Err(error) =
                        similar::fetch_list(&self.facts, reads, &fact.artist, PRIORITY).await
                    {
                        tracing::debug!(%error, "a similar-artists list unread; labs rests");
                        *labs_failed = true;
                    }
                }
                FactKind::Listeners => {
                    let batch = take_batch(
                        work,
                        &mut done,
                        FactKind::Listeners,
                        ARTIST_POPULARITY_BATCH,
                    );
                    if !budget.take(true) {
                        continue;
                    }
                    made += 1;
                    if let Err(error) =
                        similar::fetch_counts(&self.facts, reads, &batch, PRIORITY).await
                    {
                        tracing::debug!(%error, "listener counts unread; asked again later");
                    }
                }
                FactKind::Albums => {
                    done.insert(at);
                    if !budget.take(reads.musicbrainz_metered) {
                        continue;
                    }
                    made += 1;
                    if let Err(error) =
                        albums::fetch_list(&self.facts, reads, &fact.artist, PRIORITY).await
                    {
                        tracing::debug!(%error, "an album list unread; asked again later");
                    }
                }
                FactKind::AlbumCounts => {
                    let batch = self.album_count_batch(work, &mut done).await?;
                    if batch.is_empty() || !budget.take(true) {
                        continue;
                    }
                    made += 1;
                    if let Err(error) =
                        albums::fetch_counts(&self.facts, reads, &batch, PRIORITY).await
                    {
                        tracing::debug!(%error, "album counts unread; asked again later");
                    }
                }
            }
        }
        Ok(made)
    }

    /// The artists whose album counts go in one read, in plan order: as
    /// many as fit [`RELEASE_GROUP_POPULARITY_BATCH`] groups, at least one.
    async fn album_count_batch(
        &self,
        work: &[FactRef],
        done: &mut HashSet<usize>,
    ) -> Result<Vec<String>, DbError> {
        let mut batch = Vec::new();
        let mut groups = 0;
        for (at, fact) in work.iter().enumerate() {
            if fact.kind != FactKind::AlbumCounts || done.contains(&at) {
                continue;
            }
            let more = self
                .facts
                .album_groups(std::slice::from_ref(&fact.artist))
                .await?
                .len();
            if !batch.is_empty() && groups + more > RELEASE_GROUP_POPULARITY_BATCH {
                break;
            }
            done.insert(at);
            groups += more;
            batch.push(fact.artist.clone());
        }
        Ok(batch)
    }
}

/// The artists of up to `size` facts of `kind`, in plan order, marked done.
fn take_batch(
    work: &[FactRef],
    done: &mut HashSet<usize>,
    kind: FactKind,
    size: usize,
) -> Vec<String> {
    let mut batch = Vec::new();
    for (at, fact) in work.iter().enumerate() {
        if batch.len() == size {
            break;
        }
        if fact.kind == kind && done.insert(at) {
            batch.push(fact.artist.clone());
        }
    }
    batch
}

impl JobHandler for FactJobs {
    fn run(&self, job: JobCtx) -> JobFuture<'static, Outcome> {
        let runner = self.clone();
        Box::pin(async move {
            let facts = runner.facts.clone();
            let token = job.token().clone();
            let read = runner.run_once(&move || token.is_cancelled()).await;
            let finished = match read {
                Ok(made) => {
                    tracing::debug!(made, "kept facts refreshed");
                    facts.finish(move |tx| job.complete(tx)).await
                }
                Err(error) => Err(JobError::from(error)),
            };
            match finished {
                Ok(()) => Outcome::Done,
                Err(error) if error.is_busy() => Outcome::Busy,
                // Another worker holds the job: its run reads the facts.
                Err(JobError::LeaseLost) => Outcome::Done,
                Err(error) => {
                    tracing::warn!(%error, "kept facts not refreshed; trying again");
                    Outcome::Retry {
                        code: "DISCOVER_FACTS_FAILED",
                        at: None,
                    }
                }
            }
        })
    }
}
