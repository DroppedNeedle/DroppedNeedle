//! The taste job: one kind ([`KIND`], keyed by the user id) that works out
//! a person's taste from their own rows and keeps it whole.
//!
//! A run reads the person's history (their plays in the app and imported
//! listens), weighs every artist their own rows and accounts name as the
//! seeds always were, links the seeds through the kept similar-artist
//! answers only, groups them, and replaces their four tables in one write
//! that completes the job. It reads no public fact from a provider: a seed
//! whose list is not kept is wanted for the kept facts' refresh, and the
//! build is marked incomplete and keeps the groups before it. Any other
//! gap (an input that failed, no room for the top-artists read) is noted
//! on the build row too, and the seeds are grouped again. The
//! person's own top artists are still read in two cases, through the
//! cache: ListenBrainz's while their imported history is not current (and
//! the day's background reads have room), and Last.fm's. The write that
//! completes a run also queues the person's pool build
//! ([`pool`](super::pool)), through [`Due::queue_now`]. A run is safe to
//! repeat.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobError, JobHandler, JobKind, KindSpec, Outcome};
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::AdvancedSettings;
use droppedneedle_providers::RequestPriority;
use rusqlite::Transaction;

use super::history::History;
use super::pool;
use super::taste::{self, Links, TasteSeed};
use super::taste_store::{self, ArtistOut, Build, BuildRow, Kept, KnownGroupOut};
use crate::discover::cache::ForgetCounts;
use crate::discover::due::Due;
use crate::discover::facts::plan;
use crate::discover::listening::{Ask, ListenSource, ListeningData};
use crate::discover::queue::QueueSettings;
use crate::discover::queue::build::BuildContext;
use crate::discover::queue::seeds;
use crate::discover::queue::store::QueueDb;

/// The taste job kind.
pub const KIND: JobKind = JobKind::new("discover.taste");

/// How taste jobs run: a handful of local reads and one write per person,
/// done rows kept a day since a person may be built several times a day.
pub const SPEC: KindSpec = KindSpec::new(KIND)
    .max_attempts(3)
    .run_timeout(Duration::from_secs(5 * 60))
    .keep_done(Duration::from_secs(24 * 3600))
    .keep_failed(Duration::from_secs(7 * 24 * 3600));

/// What the taste job is built from.
pub struct TasteInputs {
    /// The database.
    pub db: Db,
    /// The kernel clock.
    pub clock: Arc<dyn Clock>,
    /// The listening reads: the person's accounts, their own top artists
    /// and the kept similar-artist answers.
    pub listening: ListeningData,
    /// The advanced settings, for the seed settings.
    pub advanced: Live<AdvancedSettings>,
    /// The forget counts every page cache shares.
    pub forgets: ForgetCounts,
    /// Where the pool build a taste build makes due is queued.
    pub due: Arc<Due>,
}

/// Runs [`KIND`] jobs.
#[derive(Clone)]
pub struct TasteJobs {
    db: Db,
    queue_db: QueueDb,
    clock: Arc<dyn Clock>,
    listening: ListeningData,
    advanced: Live<AdvancedSettings>,
    forgets: ForgetCounts,
    due: Arc<Due>,
}

/// Why a run kept nothing.
#[derive(Debug, thiserror::Error)]
pub enum TasteError {
    /// Discover's settings could not be read: the build waits for them.
    #[error("discover's settings cannot be read")]
    Settings,
    /// The database or the job's lease.
    #[error(transparent)]
    Job(#[from] JobError),
}

impl TasteJobs {
    /// The handler over `inputs`.
    pub fn new(inputs: TasteInputs) -> Self {
        Self {
            queue_db: QueueDb::new(inputs.db.clone()),
            db: inputs.db,
            clock: inputs.clock,
            listening: inputs.listening,
            advanced: inputs.advanced,
            forgets: inputs.forgets,
            due: inputs.due,
        }
    }

    /// Work out `user_id`'s taste and keep it; `complete` runs inside the
    /// write, which also queues their pool build.
    ///
    /// # Errors
    ///
    /// Unreadable settings, or the database's failure.
    pub async fn run_once(
        &self,
        user_id: &str,
        complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
    ) -> Result<(), TasteError> {
        let settings = QueueSettings::read(&self.advanced).ok_or(TasteError::Settings)?;
        let forgets_at = self.forgets.of(user_id);
        let now_ms = self.clock.now_ms();
        let history = History::read(&self.db, user_id, now_ms)
            .await
            .map_err(JobError::from)?;
        let policy = self.listening.policy(user_id).await;
        let user = policy.user().clone();
        let source = policy.listening();

        // ListenBrainz's top artists, only while the imported history is
        // not current, and only while the day's background reads have room.
        let mut incomplete = false;
        let follows_listenbrainz =
            policy.blends(settings.blend_services) || source == ListenSource::Listenbrainz;
        let wants_listenbrainz =
            user.listenbrainz.is_some() && follows_listenbrainz && !history.import_current;
        let read_listenbrainz = wants_listenbrainz
            && match plan::room(self.listening.facts()).await {
                Ok(room) => room > 0,
                Err(error) => {
                    tracing::warn!(%error, "the day's reads unreadable; top artists not read");
                    false
                }
            };
        incomplete |= wants_listenbrainz && !read_listenbrainz;

        let ctx = BuildContext {
            listening: &self.listening,
            db: &self.queue_db,
            settings: &settings,
            ask: Ask::build(RequestPriority::BackgroundSync),
            now_ms,
        };
        let tallies = seeds::weigh(&ctx, user_id, &user, source, &history, read_listenbrainz).await;
        incomplete |= ctx.ask.met_failure();

        let seed_ids = tallies.seed_ids();
        let links_ask = Ask::build(RequestPriority::BackgroundSync);
        let lists = self
            .listening
            .similar_artists_each(user_id, &seed_ids, usize::MAX, &links_ask)
            .await;
        // Only missing or failed similar-artist answers keep the groups
        // before: any other gap is noted on the build row and regrouped.
        let links_missing = links_ask.met_failure();
        incomplete |= links_missing;
        let seeded: HashSet<&str> = seed_ids.iter().map(String::as_str).collect();
        let mut links = Links::default();
        for (seed, list) in seed_ids.iter().zip(&lists) {
            for neighbour in list.iter().flatten() {
                if let Some(mbid) = neighbour.artist.mbid.as_deref()
                    && seeded.contains(mbid)
                {
                    links.add(seed, mbid, neighbour.score);
                }
            }
        }

        let gaps = Gaps {
            incomplete,
            links_missing,
        };
        let build = assemble(&history, &tallies, &seed_ids, &links, now_ms, gaps);
        let (due, user) = (Arc::clone(&self.due), user_id.to_owned());
        let complete = move |tx: &Transaction| {
            complete(tx)?;
            due.queue_now(tx, pool::KIND, &user)
        };
        let kept = taste_store::rebuild(
            &self.db,
            &self.forgets,
            user_id,
            forgets_at,
            build,
            complete,
        )
        .await?;
        if kept == Kept::Dropped {
            tracing::debug!("a taste build outlived a forget or its account; nothing kept");
        }
        Ok(())
    }
}

/// What a build could not read.
struct Gaps {
    /// Anything it needed: facts, an input or the day's room.
    incomplete: bool,
    /// A similar-artist answer between its seeds.
    links_missing: bool,
}

/// The rows of one build from what was read.
fn assemble(
    history: &History,
    tallies: &seeds::Tallies,
    seed_ids: &[String],
    links: &Links,
    now_ms: i64,
    gaps: Gaps,
) -> Build {
    let played: std::collections::HashMap<&str, &super::history::Played> = history
        .artists
        .iter()
        .map(|artist| (artist.mbid.as_str(), artist))
        .collect();
    let taste_seeds: Vec<TasteSeed> = seed_ids
        .iter()
        .filter_map(|mbid| {
            let tally = tallies.artists().get(mbid)?;
            // A seed the person said was a phase is one by what they chose:
            // its plays say nothing about the trend.
            let played = played
                .get(mbid.as_str())
                .filter(|_| !tallies.phases().contains(mbid));
            Some(TasteSeed {
                mbid: mbid.clone(),
                name: tally.name.clone(),
                weight: tally.weight,
                plays: played.map_or(0, |artist| artist.plays + artist.imported_listens),
                recent: played.map_or(0, |artist| artist.recent),
            })
        })
        .collect();
    let groups = taste::groups(&taste_seeds, links);
    let group_of = |mbid: &str| {
        groups
            .iter()
            .position(|group| group.members.iter().any(|member| member == mbid))
            .and_then(|no| i64::try_from(no).ok())
    };
    let seeded: HashSet<&str> = seed_ids.iter().map(String::as_str).collect();
    let artists: Vec<ArtistOut> = tallies
        .artists()
        .iter()
        .map(|(mbid, tally)| {
            let history = played.get(mbid.as_str());
            let is_seed = seeded.contains(mbid.as_str());
            ArtistOut {
                mbid: mbid.clone(),
                name: tally.name.clone(),
                plays: history.map_or(0, |artist| artist.plays),
                imported_listens: history.map_or(0, |artist| artist.imported_listens),
                months_active: history.map_or(0, |artist| artist.months),
                last_played_at: history.map(|artist| artist.last_at),
                weight: tally.weight,
                is_seed,
                group_no: is_seed.then(|| group_of(mbid)).flatten(),
            }
        })
        .collect();
    let (plays, listens, from, to) = history.span();
    Build {
        row: BuildRow {
            built_at: now_ms,
            plays_counted: plays,
            imported_listens_counted: listens,
            artists_played: i64::try_from(history.artists.len()).unwrap_or(i64::MAX),
            seeds: i64::try_from(seed_ids.len()).unwrap_or(i64::MAX),
            history_from_at: from,
            history_to_at: to,
            incomplete: gaps.incomplete,
        },
        links_missing: gaps.links_missing,
        groups,
        artists,
        known: history
            .groups
            .iter()
            .map(|group| KnownGroupOut {
                mbid: group.mbid.clone(),
                plays: group.plays,
                last_played_at: group.last_at,
            })
            .collect(),
    }
}

impl JobHandler for TasteJobs {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let runner = self.clone();
        Box::pin(async move {
            let user_id = job.key().to_owned();
            match runner.run_once(&user_id, move |tx| job.complete(tx)).await {
                Ok(()) => Outcome::Done,
                Err(TasteError::Job(error)) if error.is_busy() => Outcome::Busy,
                // Another worker holds the job: its run keeps the rows.
                Err(TasteError::Job(JobError::LeaseLost)) => Outcome::Done,
                Err(TasteError::Settings) => Outcome::Retry {
                    code: "DISCOVER_SETTINGS_UNREADABLE",
                    at: None,
                },
                Err(error) => {
                    tracing::warn!(%error, "a taste build not kept; trying again");
                    Outcome::Retry {
                        code: "DISCOVER_TASTE_FAILED",
                        at: None,
                    }
                }
            }
        })
    }
}
