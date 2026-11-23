//! The pool job: one kind ([`KIND`], keyed by the user id) that builds a
//! person's candidate pool whole from their taste rows, the library, what
//! they told Discover and the kept facts, and keeps it.
//!
//! The taste job queues it in the write that completes a taste build, and
//! [`schedule`](super::schedule) queues it again once album facts a build
//! waited on have arrived. A run reads no provider fact: a list not kept is
//! wanted for the kept facts' refresh. The one outside read is the
//! person's own weekly ListenBrainz playlist, the cached read the Discover
//! page makes ([`listenbrainz_picks`]), and only while no pick from a
//! playlist under a week old is kept. While a seed's list is missing the
//! albums of the build before that still pass stay beside the new ones, so
//! a bad night never empties a pool that was good. A run is safe to repeat.
//!
//! Nothing reads the pool yet: Discover's surfaces move over to it in
//! later work, through [`pool_store::ranked`] and [`select`](super::select).

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::Db;
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobError, JobHandler, JobKind, KindSpec, Outcome};
use rusqlite::Transaction;

use super::candidate::{Candidate, CandidateKind, PathKind};
use super::pool_store::{self, Counts, PoolBuild};
use super::rank::{self, AlbumGates, AlbumList, ArtistGates, Ranking, Seed, Walk, proposed};
use super::select::Exposure;
use super::sources::listenbrainz_picks::{self, Picked};
use super::sources::neighbours;
use super::taste_store::Kept as Written;
use crate::discover::cache::ForgetCounts;
use crate::discover::facts::{FactKind, FactSource, Kept, Notes, WantOrder};
use crate::discover::feedback::Feedback;
use crate::discover::listening::ListeningData;

/// The pool job kind.
pub const KIND: JobKind = JobKind::new("discover.pool");

/// How pool jobs run: local reads and one write per person, done rows kept
/// a day since a person's pool may be built several times a day.
pub const SPEC: KindSpec = KindSpec::new(KIND)
    .max_attempts(3)
    .run_timeout(Duration::from_secs(5 * 60))
    .keep_done(Duration::from_secs(24 * 3600))
    .keep_failed(Duration::from_secs(7 * 24 * 3600));

/// What the pool job is built from.
pub struct PoolInputs {
    /// The database.
    pub db: Db,
    /// The kernel clock.
    pub clock: Arc<dyn Clock>,
    /// The listening reads: the person's accounts and the kept facts.
    pub listening: ListeningData,
    /// The forget counts every page cache shares.
    pub forgets: ForgetCounts,
}

/// Runs [`KIND`] jobs.
#[derive(Clone)]
pub struct PoolJobs {
    db: Db,
    clock: Arc<dyn Clock>,
    listening: ListeningData,
    forgets: ForgetCounts,
}

/// What the walk left.
struct Walked {
    walk: Walk,
    /// Artists whose albums' counts are not read yet.
    uncounted: Vec<String>,
}

impl PoolJobs {
    /// The handler over `inputs`.
    pub fn new(inputs: PoolInputs) -> Self {
        Self {
            db: inputs.db,
            clock: inputs.clock,
            listening: inputs.listening,
            forgets: inputs.forgets,
        }
    }

    /// Walk `ranking` spread over `shares`, reading each artist's kept album
    /// list, until the walk is done. Every list and album count it reached
    /// and did not find is wanted.
    async fn walk(
        &self,
        ranking: &Ranking,
        shares: &[(i64, f64)],
        gates: &AlbumGates<'_>,
    ) -> Result<Walked, JobError> {
        let facts = self.listening.facts();
        let counted = self.listening.listenbrainz_on();
        let now = facts.now();
        let mut walk = Walk::new(proposed::WALK_ARTISTS);
        let mut notes = Notes::default();
        let mut uncounted = Vec::new();
        for at in rank::walk_order(&ranking.artists, shares) {
            if walk.done() {
                break;
            }
            let artist = &ranking.artists[at];
            if gates.exposure.rests_artist(&artist.mbid) {
                walk.step(artist, AlbumList::Nothing, gates);
                continue;
            }
            let (list, counts) = facts.albums(&artist.mbid).await?;
            notes.saw(
                now,
                (FactKind::Albums, FactSource::MusicBrainz, WantOrder::Pool),
                &artist.mbid,
                &list,
            );
            if counted && matches!(list.kept, Kept::Found(_)) {
                notes.saw(
                    now,
                    (
                        FactKind::AlbumCounts,
                        FactSource::ListenBrainz,
                        WantOrder::Pool,
                    ),
                    &artist.mbid,
                    &counts,
                );
                if matches!(counts.kept, Kept::Missing) {
                    uncounted.push(artist.mbid.clone());
                }
            }
            let counts_found = counted && matches!(counts.kept, Kept::Found(()));
            match list.kept {
                Kept::Missing => walk.step(artist, AlbumList::Missing, gates),
                Kept::Nothing => walk.step(artist, AlbumList::Nothing, gates),
                Kept::Found(mut rows) => {
                    // The artist's albums were counted: one the answer left
                    // out has no listeners.
                    if counts_found {
                        for row in &mut rows {
                            row.album.facts.listeners.get_or_insert(0);
                        }
                    }
                    walk.step(artist, AlbumList::Found(&rows), gates)
                }
            };
        }
        if let Err(cause) = facts.note(notes).await {
            tracing::warn!(%cause, "the pool's album wants went unnoted");
        }
        Ok(Walked { walk, uncounted })
    }

    /// What `user_id`'s last build kept that this one may keep: its
    /// new-artist rows, and its picks from a playlist under a week old at
    /// `now_ms`, each with its playlist paths alone (a pick that joined a
    /// new-artist row included).
    async fn previous(
        &self,
        user_id: &str,
        now_ms: i64,
    ) -> Result<(Vec<Candidate>, Vec<Candidate>), JobError> {
        let since = now_ms - listenbrainz_picks::PLAYLIST_MAX_AGE_MS;
        let mut kept_new = Vec::new();
        let mut kept_picks = Vec::new();
        for candidate in pool_store::previous(&self.db, user_id).await? {
            if picked_since(&candidate, since) {
                let mut pick = candidate.clone();
                pick.paths.retain(|path| path.kind == PathKind::Source);
                pick.kind = CandidateKind::SourcePick;
                (pick.support, pick.group, pick.group_support) = (0.0, None, 0.0);
                kept_picks.push(pick);
            }
            if candidate.kind == CandidateKind::NewArtist {
                kept_new.push(candidate);
            }
        }
        Ok((kept_new, kept_picks))
    }

    /// Build `user_id`'s pool and keep it; `complete` runs inside the
    /// write.
    ///
    /// # Errors
    ///
    /// The database's failure, or what they told Discover not reading.
    pub async fn run_once(
        &self,
        user_id: &str,
        complete: impl FnOnce(&Transaction) -> Result<(), JobError> + Send + 'static,
    ) -> Result<(), JobError> {
        let started = tokio::time::Instant::now();
        let forgets_at = self.forgets.of(user_id);
        let now_ms = self.clock.now_ms();
        let Some(taste) = pool_store::taste(&self.db, user_id).await? else {
            pool_store::rebuild(&self.db, &self.forgets, user_id, forgets_at, None, complete)
                .await?;
            return Ok(());
        };
        let seed_ids: Vec<String> = taste.seeds.iter().map(|seed| seed.mbid.clone()).collect();
        let library = pool_store::library(&self.db, &seed_ids).await?;
        let feedback = Feedback::load(&self.db, user_id, now_ms).await?;
        let shown = pool_store::shown(&self.db, user_id, now_ms - proposed::REST_MS).await?;
        let exposure = Exposure::from_shown(&shown, now_ms);
        // The person's switches and keys, not the services' health: the
        // build reads kept rows, so a service resting changes nothing here.
        let accounts = self.listening.accounts(user_id).await;

        let seeds: Vec<Seed> = taste
            .seeds
            .iter()
            .map(|seed| Seed {
                mbid: seed.mbid.clone(),
                name: seed.name.clone(),
                plays: seed.plays,
                weight: seed.weight,
                group: seed.group_no,
                albums_owned: library.albums_owned.get(&seed.mbid).copied().unwrap_or(0),
            })
            .collect();
        let found =
            neighbours::read(&self.db, &self.listening, user_id, &seeds, accounts.lastfm).await?;
        let artist_gates = ArtistGates {
            known: &taste.known_artists,
            library: &library.library_artists,
            feedback: &feedback,
        };
        let ranking = rank::rank(
            &seeds,
            &found.links,
            &found.listeners,
            taste.groups.len(),
            &artist_gates,
        );
        let (kept_new, kept_picks) = self.previous(user_id, now_ms).await?;
        // This week's playlist, once: while the picks kept from it are under
        // a week old it is not read again.
        let picked = if accounts.listenbrainz.is_none() {
            Some(Picked::Rows(Vec::new()))
        } else if kept_picks.is_empty() {
            Some(listenbrainz_picks::read(&self.listening, user_id, &accounts, now_ms).await)
        } else {
            None
        };
        // Ownership is read for the albums this build can reach: the kept
        // lists of the ranked artists, the playlist's albums and the rows
        // the build may keep.
        let ranked_ids: Vec<String> = ranking
            .artists
            .iter()
            .map(|artist| artist.mbid.clone())
            .collect();
        let mut reachable = self.listening.facts().album_groups(&ranked_ids).await?;
        if let Some(Picked::Rows(rows)) = &picked {
            reachable.extend(rows.iter().map(|row| row.release_group.clone()));
        }
        reachable.extend(
            kept_new
                .iter()
                .chain(&kept_picks)
                .map(|candidate| candidate.release_group_mbid.clone()),
        );
        let owned = pool_store::owned_among(&self.db, &reachable).await?;
        let gates = AlbumGates {
            owned: &owned,
            fetching: &library.fetching,
            played: &taste.played,
            feedback: &feedback,
            exposure: &exposure,
            artists: &artist_gates,
        };
        let Walked { walk, uncounted } = self.walk(&ranking, &taste.groups, &gates).await?;

        let mut incomplete = found.incomplete || !walk.pending.is_empty() || !uncounted.is_empty();
        let mut candidates = walk.candidates;
        if found.lists_missing {
            // A seed's list is missing: the albums the last build had that
            // this one could not reach stay, if they still pass.
            for mut before in kept_new {
                let present = candidates
                    .iter()
                    .any(|candidate| candidate.release_group_mbid == before.release_group_mbid);
                if !present && still_admitted(&before, &gates) {
                    before.paths.retain(|path| path.kind != PathKind::Source);
                    candidates.push(before);
                }
            }
        }
        let picks = match picked {
            None => kept_picks
                .into_iter()
                .filter(|candidate| still_admitted(candidate, &gates))
                .collect(),
            Some(Picked::Rows(rows)) => {
                let groups: Vec<String> =
                    rows.iter().map(|row| row.release_group.clone()).collect();
                let facts = pool_store::group_facts(&self.db, &groups).await?;
                let artists: Vec<String> = rows.iter().map(|row| row.artist_mbid.clone()).collect();
                let counting = self.listening.listenbrainz_on();
                let listeners = self
                    .listening
                    .facts()
                    .listeners(&artists)
                    .await?
                    .into_iter()
                    .filter_map(|(id, looked)| match looked.kept {
                        // Counted with no listeners is none; uncounted is
                        // not known.
                        Kept::Found(counts) if counting => {
                            Some((id, counts.listeners.unwrap_or(0)))
                        }
                        Kept::Found(counts) => Some((id, counts.listeners?)),
                        Kept::Nothing if counting => Some((id, 0)),
                        _ => None,
                    })
                    .collect();
                listenbrainz_picks::candidates(&rows, &facts, &listeners, &gates)
            }
            Some(Picked::Missing) => {
                incomplete = true;
                Vec::new()
            }
        };
        merge(&mut candidates, picks);
        let candidates = rank::order(candidates);

        let mut waiting_for = walk.pending.clone();
        waiting_for.extend(uncounted);
        let mut seen = HashSet::new();
        waiting_for.retain(|artist| seen.insert(artist.clone()));
        let counts = Counts {
            seeds_used: seeds.len(),
            neighbours_found: ranking.neighbours_found,
            known_left_out: ranking.known_left_out,
            hubs_left_out: ranking.hubs_left_out,
            artists_ranked: ranking.artists.len(),
            artists_without_album: walk.without_album,
        };
        let kinds = |kind: CandidateKind| candidates.iter().filter(|c| c.kind == kind).count();
        let (new_artist, source_pick) = (
            kinds(CandidateKind::NewArtist),
            kinds(CandidateKind::SourcePick),
        );
        let wanted = waiting_for.len();
        let build = PoolBuild {
            built_at: now_ms,
            incomplete,
            waiting_for,
            counts: counts.clone(),
            candidates,
        };
        let written = pool_store::rebuild(
            &self.db,
            &self.forgets,
            user_id,
            forgets_at,
            Some(build),
            complete,
        )
        .await?;
        if written == Written::Dropped {
            tracing::debug!("a pool build outlived a forget or its account; nothing kept");
            return Ok(());
        }
        tracing::info!(
            user = %user_id,
            seconds = started.elapsed().as_secs_f64(),
            seeds_used = counts.seeds_used,
            neighbours_found = counts.neighbours_found,
            known_left_out = counts.known_left_out,
            hubs_left_out = counts.hubs_left_out,
            artists_ranked = counts.artists_ranked,
            artists_without_album = counts.artists_without_album,
            new_artist,
            source_pick,
            wanted,
            incomplete,
            "discover pool built"
        );
        Ok(())
    }
}

/// `picks` into `candidates`: one already there for the same release group
/// takes the pick's paths of a kind and source it lacks, the rest go in as
/// they are.
fn merge(candidates: &mut Vec<Candidate>, picks: Vec<Candidate>) {
    for pick in picks {
        match candidates
            .iter_mut()
            .find(|candidate| candidate.release_group_mbid == pick.release_group_mbid)
        {
            Some(candidate) => {
                for path in pick.paths {
                    let held = candidate
                        .paths
                        .iter()
                        .any(|kept| kept.kind == path.kind && kept.source == path.source);
                    if !held {
                        candidate.paths.push(path);
                    }
                }
            }
            None => candidates.push(pick),
        }
    }
}

/// Whether a row the last build kept still passes this build's gates.
fn still_admitted(candidate: &Candidate, gates: &AlbumGates<'_>) -> bool {
    let artist = candidate.artist_mbid.as_str();
    !gates.artists.knows(artist)
        && !gates.artists.refuses(artist)
        && !gates.exposure.rests_artist(artist)
        && gates.admits(&candidate.release_group_mbid, artist)
}

/// Whether `candidate` came from a playlist made at `since` or later.
fn picked_since(candidate: &Candidate, since: i64) -> bool {
    candidate.paths.iter().any(|path| {
        path.kind == PathKind::Source && path.fetched_at.is_some_and(|made| made >= since)
    })
}

impl JobHandler for PoolJobs {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let runner = self.clone();
        Box::pin(async move {
            let user_id = job.key().to_owned();
            match runner.run_once(&user_id, move |tx| job.complete(tx)).await {
                Ok(()) => Outcome::Done,
                Err(error) if error.is_busy() => Outcome::Busy,
                // Another worker holds the job: its run keeps the rows.
                Err(JobError::LeaseLost) => Outcome::Done,
                Err(error) => {
                    tracing::warn!(%error, "a pool build not kept; trying again");
                    Outcome::Retry {
                        code: "DISCOVER_POOL_FAILED",
                        at: None,
                    }
                }
            }
        })
    }
}
