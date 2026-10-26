//! The operations service: what the HTTP handlers and the worker loop call.
//!
//! Reads and writes go through the shared database's blocking calls, so
//! every method runs off the async workers (handlers use `spawn_blocking`;
//! the loop drives its tick on a blocking thread). Requests write on the
//! foreground lane, the worker on the background lane.

use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension as _};

use super::decisions::indexed_track_ids;
use super::models::{
    Control, EditionChoice, OperationDetail, OperationError, OperationJob,
    ReidentificationCandidate, ReleaseSearch, UndoOutcome,
};
use super::plan::{self, EditionPlan, PlanReading};
use super::reasons;
use super::reidentify::evaluation;
use super::store::{self, NewReidentification};
use super::{choice, decisions, hand_match, history, reviews};
use crate::clock::LibraryClock as _;
use crate::identify::models::LocalAlbumFacts;
use crate::identify::providers::RELEASE_UNREADABLE;
use crate::identify::queue::queue_retry;
use crate::identify::service::IdentifyService;
use crate::identify::sources::EditionQuery;
use crate::identify::sqlite::SqliteIdentifyStore;
use crate::identity::reads::album_identity_row;
use crate::jobs::{self, OperationKind};
use crate::matching::model::Release;
use crate::scan::watcher::WorkWakeups;
use droppedneedle_kernel::db::{Db, Lane, savepoint};
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::LibraryManagement;
use droppedneedle_providers::RequestPriority;

/// Most reviews one "retry all" or bulk action takes on.
pub const BULK_MAX: u32 = 5_000;

/// Largest edition-finder page, as in v2.
pub const RELEASE_PAGE_MAX: u32 = 12;

/// Operation jobs over the library's identify store and service.
#[derive(Clone)]
pub struct Operations {
    db: Db,
    store: Arc<SqliteIdentifyStore>,
    identify: Arc<IdentifyService>,
    ids: Arc<dyn IdGenerator>,
    management: Live<LibraryManagement>,
    wakeups: WorkWakeups,
    clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
}

impl Operations {
    pub fn new(
        db: Db,
        store: Arc<SqliteIdentifyStore>,
        identify: Arc<IdentifyService>,
        ids: Arc<dyn IdGenerator>,
        management: Live<LibraryManagement>,
        wakeups: WorkWakeups,
        clock: Arc<dyn droppedneedle_kernel::clock::Clock>,
    ) -> Self {
        Self {
            db,
            store,
            identify,
            ids,
            management,
            wakeups,
            clock,
        }
    }

    /// Where the album's edition stands. Blocking.
    pub fn edition_status(
        &self,
        album_id: &str,
    ) -> Result<Option<super::status::EditionStatus>, OperationError> {
        self.db.read_blocking("operations.edition_status", |conn| {
            Ok::<_, OperationError>(super::status::edition_status(conn, album_id)?)
        })
    }

    /// The live automatic-edition undo for one album: the revisions to
    /// echo. Blocking.
    pub fn automatic_edition_undo(
        &self,
        album_id: &str,
    ) -> Result<Option<(i64, i64)>, OperationError> {
        self.db
            .read_blocking("operations.automatic_edition_undo", |conn| {
                Ok::<_, OperationError>(decisions::live_automatic_edition_undo(conn, album_id)?)
            })
    }

    /// One page of the albums whose match waits for a person. Blocking.
    pub fn waiting_albums(
        &self,
        unmatched: bool,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<super::status::WaitingAlbum>, u64), OperationError> {
        self.db.read_blocking("operations.waiting_albums", |conn| {
            Ok::<_, OperationError>(super::status::waiting_albums(
                conn, unmatched, limit, offset,
            )?)
        })
    }

    /// One job with its results and, for a re-identification, its
    /// candidates. Blocking.
    pub fn get(&self, job_id: &str) -> Result<OperationDetail, OperationError> {
        self.db
            .read_blocking("operations.get", |conn| job_detail(conn, job_id))
    }

    /// Pause, resume, or stop a job. Blocking.
    pub fn control(
        &self,
        job_id: &str,
        control: Control,
        expected_row_revision: i64,
        idempotency_key: Option<&str>,
    ) -> Result<OperationJob, OperationError> {
        let job = self
            .db
            .write_blocking(Lane::Foreground, "operations.control", |tx| {
                store::request_control(
                    tx,
                    job_id,
                    control,
                    expected_row_revision,
                    idempotency_key,
                    self.clock.now_secs(),
                )
            })?;
        if let Some(kind) = OperationKind::parse(&job.kind) {
            jobs::wake(&self.wakeups, kind);
        }
        Ok(job)
    }

    /// A re-identification of one album.
    fn plain_reidentification(
        &self,
        album_id: &str,
        user_id: &str,
        idempotency_key: Option<String>,
        one_off_local_metadata: bool,
    ) -> NewReidentification {
        NewReidentification {
            job_id: self.ids.new_id(),
            requested_by_user_id: user_id.to_owned(),
            idempotency_key,
            local_album_id: album_id.to_owned(),
            one_off_local_metadata,
        }
    }

    /// One page of identification reviews. Blocking.
    pub fn reviews(
        &self,
        filter: &reviews::ReviewFilter,
    ) -> Result<(Vec<reviews::ReviewRow>, u64, reviews::ReasonCounts), OperationError> {
        self.db.read_blocking("operations.reviews", |conn| {
            Ok::<_, OperationError>(reviews::list(conn, filter)?)
        })
    }

    /// One identification review. Blocking.
    pub fn review(&self, review_id: &str) -> Result<reviews::ReviewRow, OperationError> {
        self.db
            .read_blocking("operations.review", |conn| {
                Ok::<_, OperationError>(reviews::get(conn, review_id)?)
            })?
            .ok_or(OperationError::NotFound(reasons::REVIEW_NOT_FOUND))
    }

    /// Dismiss, detach, exclude or restore from a review, then return the
    /// settled review. Blocking.
    pub fn settle_review(
        &self,
        review_id: &str,
        action: reviews::ReviewAction,
        actor: &str,
    ) -> Result<reviews::ReviewRow, OperationError> {
        self.db
            .write_blocking(Lane::Foreground, "operations.settle_review", |tx| {
                reviews::settle(tx, review_id, action, actor, self.clock.now_secs())
            })?;
        self.review(review_id)
    }

    /// Identify a pending review's album again. The job hands the album to
    /// identification, which settles it by its usual rules, so the review
    /// is dismissed in the same write. A review someone settled meanwhile,
    /// or an album that may not be retried, is refused and the review
    /// stays open. Blocking.
    pub fn retry_review(
        &self,
        review_id: &str,
        actor: &str,
        idempotency_key: Option<String>,
        one_off_local_metadata: bool,
    ) -> Result<OperationJob, OperationError> {
        let row = self.review(review_id)?;
        let new = self.plain_reidentification(
            &row.review.local_album_id,
            actor,
            idempotency_key,
            one_off_local_metadata,
        );
        let job = self
            .db
            .write_blocking(Lane::Foreground, "operations.retry_review", |tx| {
                if !reviews::mark(
                    tx,
                    review_id,
                    "pending",
                    crate::identify::models::ReviewState::Dismissed,
                    actor,
                    self.clock.now_secs(),
                )? {
                    return Err(OperationError::Conflict(reasons::REVIEW_SETTLED));
                }
                store::create_reidentification(tx, &self.management, &new, self.clock.now_secs())
            })?;
        jobs::wake(&self.wakeups, OperationKind::ExplicitReidentification);
        Ok(job)
    }

    /// Start identification again for every pending review `filter`
    /// selects, up to [`BULK_MAX`]. Albums that cannot start (excluded, no
    /// files, Local metadata without the confirmation or under management,
    /// an edition someone chose) keep their review and are counted by
    /// reason. With `preview`, nothing starts: each
    /// start is tried and rolled back, so the counts are exactly what a
    /// real run would do. Blocking.
    pub fn retry_pending_reviews(
        &self,
        actor: &str,
        one_off_local_metadata: bool,
        filter: &reviews::ReviewFilter,
        preview: bool,
    ) -> Result<reviews::Tally, OperationError> {
        let filter = reviews::ReviewFilter {
            state: Some(crate::identify::models::ReviewState::Pending),
            limit: BULK_MAX,
            offset: 0,
            ..filter.clone()
        };
        let (rows, _, _) = self.reviews(&filter)?;
        let mut outcome = reviews::Tally::default();
        for row in rows {
            let started = if preview {
                let new = self.plain_reidentification(
                    &row.review.local_album_id,
                    actor,
                    None,
                    one_off_local_metadata,
                );
                // Tried inside a savepoint that is always rolled back, so
                // nothing commits.
                self.db.write_blocking(
                    Lane::Foreground,
                    "operations.retry_pending_reviews_preview",
                    |tx| {
                        let tried = savepoint(tx, "preview", || {
                            Err::<(), _>(
                                store::create_reidentification(
                                    tx,
                                    &self.management,
                                    &new,
                                    self.clock.now_secs(),
                                )
                                .map(|_| ()),
                            )
                        })?;
                        Ok::<_, OperationError>(tried.err().unwrap_or(Ok(())))
                    },
                )?
            } else {
                self.retry_review(&row.review.id, actor, None, one_off_local_metadata)
                    .map(|_| ())
            };
            match started {
                Ok(()) => outcome.done += 1,
                Err(
                    OperationError::NotFound(reason)
                    | OperationError::Invalid(reason)
                    | OperationError::Conflict(reason),
                ) => outcome.skip(reason),
                Err(other) => return Err(other),
            }
        }
        Ok(outcome)
    }

    /// What a bulk action would do to the reviews `filter` selects: the
    /// reviews it changes, the ones it skips by reason, and the token that
    /// apply needs. Blocking.
    pub fn preview_bulk_reviews(
        &self,
        action: reviews::BulkAction,
        filter: &reviews::ReviewFilter,
        candidate_key: Option<&str>,
    ) -> Result<reviews::BulkPlan, OperationError> {
        let filter = reviews::ReviewFilter {
            limit: BULK_MAX,
            offset: 0,
            ..filter.clone()
        };
        let (rows, matched, _) = self.reviews(&filter)?;
        Ok(reviews::plan_bulk(&rows, matched, action, candidate_key))
    }

    /// Apply a previewed bulk action. Refused when the selection changed
    /// since the preview (the token no longer matches). Blocking.
    pub fn apply_bulk_reviews(
        &self,
        action: reviews::BulkAction,
        filter: &reviews::ReviewFilter,
        candidate_key: Option<&str>,
        preview_token: &str,
        actor: &str,
    ) -> Result<reviews::Tally, OperationError> {
        let plan = self.preview_bulk_reviews(action, filter, candidate_key)?;
        if plan.token != preview_token {
            return Err(OperationError::Conflict(reasons::REVIEW_SELECTION_CHANGED));
        }
        let mut outcome = reviews::Tally {
            done: 0,
            skipped: plan.skipped.clone(),
        };
        for item in &plan.items {
            let applied = match (action, item.candidate_key.as_deref()) {
                (reviews::BulkAction::KeepTagged, _) => self
                    .identify
                    .reject_candidates(&item.review_id, actor)
                    .map_err(|error| error.to_string()),
                (reviews::BulkAction::Accept, Some(key)) => self
                    .identify
                    .approve_candidate(&item.review_id, actor, key)
                    .map_err(|error| error.to_string()),
                (reviews::BulkAction::Accept, None) => Ok(false),
                (reviews::BulkAction::Exclude, _) => {
                    match self.db.write_blocking(
                        Lane::Foreground,
                        "operations.apply_bulk_reviews",
                        |tx| {
                            reviews::settle(
                                tx,
                                &item.review_id,
                                reviews::ReviewAction::Exclude,
                                actor,
                                self.clock.now_secs(),
                            )
                        },
                    ) {
                        Ok(()) => Ok(true),
                        Err(
                            OperationError::NotFound(reason)
                            | OperationError::Invalid(reason)
                            | OperationError::Conflict(reason),
                        ) => {
                            outcome.skip(reason);
                            continue;
                        }
                        Err(other) => Err(other.to_string()),
                    }
                }
            };
            match applied {
                Ok(true) => outcome.done += 1,
                Ok(false) => outcome.skip(reasons::REVIEW_SETTLED),
                Err(cause) => {
                    tracing::warn!(review = item.review_id, %cause, "bulk review action failed");
                    outcome.skip(reasons::REVIEW_ACTION_FAILED);
                }
            }
        }
        Ok(outcome)
    }

    /// Choose the album's edition: any MusicBrainz release, from the
    /// album's release group or another one. MusicBrainz is asked for the
    /// release's tracklist, the files are placed on it where they fit, and
    /// the release becomes the album's protected edition.
    pub async fn choose_edition(
        &self,
        album_id: &str,
        raw_release: &str,
        actor: Option<&str>,
    ) -> Result<EditionChoice, OperationError> {
        let release = parse_release(raw_release)?;
        let candidate = self
            .candidate_for(
                album_id,
                &release,
                Lane::Foreground,
                RequestPriority::UserInitiated,
            )
            .await?;
        let (ops, album, actor) = (self.clone(), album_id.to_owned(), actor.map(str::to_owned));
        tokio::task::spawn_blocking(move || {
            let chosen =
                ops.db
                    .write_blocking(Lane::Foreground, "operations.choose_edition", |tx| {
                        choice::choose(
                            tx,
                            &album,
                            &candidate,
                            actor.as_deref(),
                            ops.clock.now_secs(),
                        )
                    })?;
            ops.store.link_after_command(&album);
            Ok::<_, OperationError>(chosen)
        })
        .await
        .map_err(|error| OperationError::Store(error.to_string()))?
    }

    /// Seal the album's files as they are under the release's group: a
    /// custom edition, made directly on the album. The album's revisions
    /// are read before MusicBrainz is asked and checked again when the
    /// edition is written; a stale `expected_identity_revision` is refused
    /// before MusicBrainz is asked.
    pub async fn seal_custom_edition(
        &self,
        album_id: &str,
        raw_release: &str,
        expected_identity_revision: i64,
        actor: &str,
    ) -> Result<(), OperationError> {
        let release = parse_release(raw_release)?;
        let guard = {
            let ops = self.clone();
            let album = album_id.to_owned();
            tokio::task::spawn_blocking(move || {
                ops.db.read_blocking("operations.album_revisions", |conn| {
                    let known = store::album_revisions(conn, &album)?
                        .ok_or(OperationError::NotFound(reasons::ALBUM_NOT_FOUND))?;
                    Ok::<_, OperationError>(decisions::SealGuard {
                        identity_revision: choice::identity_revision(conn, &album)?,
                        album_revision: known.album_revision,
                        input_revision: known.input_revision,
                    })
                })
            })
            .await
            .map_err(|error| OperationError::Store(error.to_string()))??
        };
        if guard.identity_revision != expected_identity_revision {
            return Err(OperationError::Conflict(reasons::EDITION_CHANGED));
        }
        let candidate = self
            .candidate_for(
                album_id,
                &release,
                Lane::Foreground,
                RequestPriority::UserInitiated,
            )
            .await?;
        let (ops, album, actor) = (self.clone(), album_id.to_owned(), actor.to_owned());
        tokio::task::spawn_blocking(move || {
            ops.db
                .write_blocking(Lane::Foreground, "operations.custom_edition", |tx| {
                    decisions::custom_edition(
                        tx,
                        &album,
                        &candidate,
                        &guard,
                        &actor,
                        ops.clock.now_secs(),
                    )
                })
        })
        .await
        .map_err(|error| OperationError::Store(error.to_string()))?
    }

    /// Leave the album unmanaged, directly on the album. Blocking.
    pub fn leave_unmanaged(
        &self,
        album_id: &str,
        expected_identity_revision: i64,
        actor: &str,
    ) -> Result<(), OperationError> {
        self.db
            .write_blocking(Lane::Foreground, "operations.leave_unmanaged", |tx| {
                decisions::leave_album_unmanaged(
                    tx,
                    album_id,
                    expected_identity_revision,
                    actor,
                    self.clock.now_secs(),
                )
            })
    }

    /// The release scored against the album's files, as a candidate.
    /// MusicBrainz is asked at `priority`; documents the recall fetched
    /// are kept on `lane`.
    async fn candidate_for(
        &self,
        album_id: &str,
        release_mbid: &str,
        lane: Lane,
        priority: RequestPriority,
    ) -> Result<ReidentificationCandidate, OperationError> {
        Ok(self.scored(album_id, release_mbid, lane, priority).await?.2)
    }

    /// The album's files, the release as MusicBrainz lists it, and the
    /// release scored against the files as a candidate.
    async fn scored(
        &self,
        album_id: &str,
        release_mbid: &str,
        lane: Lane,
        priority: RequestPriority,
    ) -> Result<(LocalAlbumFacts, Release, ReidentificationCandidate), OperationError> {
        let facts = {
            let store = self.store.clone();
            let album = album_id.to_owned();
            tokio::task::spawn_blocking(move || store.album_facts(&album))
                .await
                .map_err(|error| OperationError::Store(error.to_string()))??
        }
        .filter(|facts| !facts.tracks.is_empty())
        .ok_or(OperationError::NotFound(reasons::ALBUM_NOT_FOUND))?;
        let recall = self
            .identify
            .recall(&facts, Some(release_mbid), lane, priority)
            .await?;
        if recall.provider_deferred {
            return Err(OperationError::Unavailable(
                recall
                    .failure_code
                    .unwrap_or_else(|| "musicbrainz_unavailable".to_owned()),
            ));
        }
        if recall.failure_code.as_deref() == Some(RELEASE_UNREADABLE) {
            return Err(OperationError::Conflict(reasons::EDITION_UNREADABLE));
        }
        if recall.releases.is_empty() {
            return Err(OperationError::NotFound(reasons::EDITION_NOT_FOUND));
        }
        let ranking = self.identify.rank(&facts, &recall).ok_or_else(|| {
            OperationError::Store("edition preferences cannot be read".to_owned())
        })?;
        let candidate = evaluation(&facts, &recall, &ranking, true)
            .candidates
            .into_iter()
            .next()
            .ok_or(OperationError::NotFound(reasons::EDITION_NOT_FOUND))?;
        let scored = candidate
            .evidence
            .release_mbid
            .clone()
            .unwrap_or_else(|| release_mbid.to_owned());
        let release = recall
            .releases
            .into_iter()
            .find(|release| release.answers_to(&scored))
            .ok_or(OperationError::NotFound(reasons::EDITION_NOT_FOUND))?;
        Ok((facts, release, candidate))
    }

    /// The library's copy of a release group that has an edition: a copy a
    /// person chose first, then the oldest, as acquisition picks it.
    pub async fn album_for_group(
        &self,
        release_group_mbid: &str,
    ) -> Result<Option<String>, OperationError> {
        let (ops, group) = (self.clone(), release_group_mbid.trim().to_ascii_lowercase());
        tokio::task::spawn_blocking(move || {
            ops.db.read_blocking("operations.album_for_group", |conn| {
                conn.query_row(
                    "SELECT e.local_album_id FROM local_album_external_identities e \
                     JOIN local_albums b ON b.id = e.local_album_id \
                     WHERE b.retired_into_album_id IS NULL AND e.provider = ?2 \
                       AND lower(e.release_group_mbid) = ?1 AND e.release_mbid IS NOT NULL \
                     ORDER BY e.decision_source <> 'manual', b.created_at LIMIT 1",
                    rusqlite::params![group, store::PROVIDER],
                    |row| row.get(0),
                )
                .optional()
                .map_err(OperationError::from)
            })
        })
        .await
        .map_err(|error| OperationError::Store(error.to_string()))?
    }

    /// What converting the album to `release_mbid` would do: which files
    /// keep their place, which edition tracks are missing, and which files
    /// the edition has no room for. With no release, the album's current
    /// edition is planned. Nothing changes.
    pub async fn edition_plan(
        &self,
        album_id: &str,
        raw_release: Option<&str>,
    ) -> Result<EditionPlan, OperationError> {
        Ok(self.read_edition_plan(album_id, raw_release).await?.plan)
    }

    /// [`Self::edition_plan`] with the identity revision it was read at and
    /// the files on it a person matched by hand. On the album's current
    /// edition (or a release that answers to it), files whose stored
    /// identity names one of its tracks keep that track.
    pub async fn read_edition_plan(
        &self,
        album_id: &str,
        raw_release: Option<&str>,
    ) -> Result<PlanReading, OperationError> {
        let (identity, revision, indexed, stored, written) = {
            let ops = self.clone();
            let album = album_id.to_owned();
            tokio::task::spawn_blocking(move || {
                ops.db
                    .read_blocking("operations.read_edition_plan", |conn| {
                        let identity = album_identity_row(conn, &album)?;
                        let stored = hand_match::stored_placements(conn, &album)?;
                        let mut written = Vec::new();
                        for row in &stored {
                            if let Some(matched_at) = row.matched_at
                                && hand_match::written_since(conn, &row.local_track_id, matched_at)?
                            {
                                written.push(row.local_track_id.clone());
                            }
                        }
                        Ok::<_, OperationError>((
                            identity.map(|row| (row.release_mbid, row.release_group_mbid)),
                            choice::identity_revision(conn, &album)?,
                            indexed_track_ids(conn, &album)?,
                            stored,
                            written,
                        ))
                    })
            })
            .await
            .map_err(|error| OperationError::Store(error.to_string()))??
        };
        if indexed.is_empty() {
            return Err(OperationError::NotFound(reasons::ALBUM_NOT_FOUND));
        }
        let (current, group) = identity.unwrap_or_default();
        let release = match raw_release {
            Some(raw) => parse_release(raw)?,
            None => current
                .clone()
                .ok_or(OperationError::Invalid(reasons::NO_EDITION_TO_PLAN))?,
        };
        let (facts, release, candidate) = self
            .scored(
                album_id,
                &release,
                Lane::Foreground,
                RequestPriority::UserInitiated,
            )
            .await?;
        let stored = match current.as_deref() {
            Some(current) if release.answers_to(current.trim()) => {
                hand_match::on_release(stored, &release)
            }
            _ => Vec::new(),
        };
        let (plan, matched_by_hand) = plan::build(
            &facts,
            &release,
            &candidate,
            &indexed,
            &stored,
            (current.as_deref(), group.as_deref()),
        );
        let match_written = written
            .into_iter()
            .filter(|file| matched_by_hand.contains(file))
            .collect();
        Ok(PlanReading {
            plan,
            identity_revision: revision,
            matched_by_hand,
            match_written,
        })
    }

    /// Match one file of the album to a track of its edition by hand. The
    /// album's identity revision is checked before MusicBrainz is asked for
    /// the release and again when the match is written.
    pub async fn match_file(
        &self,
        album_id: &str,
        track_id: &str,
        release_track_mbid: &str,
        expected_identity_revision: i64,
        actor: &str,
    ) -> Result<(), OperationError> {
        let release = {
            let ops = self.clone();
            let album = album_id.to_owned();
            tokio::task::spawn_blocking(move || {
                ops.db.read_blocking("operations.guard", |conn| {
                    hand_match::guard(conn, &album, expected_identity_revision)
                })
            })
            .await
            .map_err(|error| OperationError::Store(error.to_string()))??
        };
        let (_, release, candidate) = self
            .scored(
                album_id,
                &release,
                Lane::Foreground,
                RequestPriority::UserInitiated,
            )
            .await?;
        let ops = self.clone();
        let (album, track, wanted, actor) = (
            album_id.to_owned(),
            track_id.to_owned(),
            release_track_mbid.to_owned(),
            actor.to_owned(),
        );
        tokio::task::spawn_blocking(move || {
            let asked = hand_match::HandMatch {
                album_id: &album,
                track_id: &track,
                release_track_mbid: &wanted,
                expected_identity_revision,
                actor: &actor,
            };
            ops.db
                .write_blocking(Lane::Foreground, "operations.match_file", |tx| {
                    hand_match::match_file(
                        tx,
                        &asked,
                        &release,
                        &candidate,
                        ops.clock.now_secs(),
                        ops.clock.now_ms(),
                    )
                })
        })
        .await
        .map_err(|error| OperationError::Store(error.to_string()))?
    }

    /// "Undo match" on one file the person matched by hand. Blocking.
    pub fn unmatch_file(
        &self,
        album_id: &str,
        track_id: &str,
        actor: &str,
    ) -> Result<(), OperationError> {
        self.db
            .write_blocking(Lane::Foreground, "operations.unmatch_file", |tx| {
                hand_match::unmatch_file(tx, album_id, track_id, actor, self.clock.now_secs())
            })
    }

    /// "Let DroppedNeedle choose": hand the album's edition back to
    /// automatic best fit and queue an identification. Blocking.
    pub fn hand_back_edition(&self, album_id: &str, actor: &str) -> Result<(), OperationError> {
        let known = self
            .db
            .read_blocking("operations.hand_back_edition_album_revisions", |conn| {
                Ok::<_, OperationError>(store::album_revisions(conn, album_id)?)
            })?;
        if known.is_none() {
            return Err(OperationError::NotFound(reasons::ALBUM_NOT_FOUND));
        }
        self.db
            .write_blocking(Lane::Foreground, "operations.hand_back", |tx| {
                choice::hand_back(tx, album_id, actor, self.clock.now_secs())
            })
    }

    /// What the app did by itself to the album, newest first, as
    /// `caller_user_id` may see it. Blocking.
    pub fn album_history(
        &self,
        album_id: &str,
        caller_user_id: &str,
    ) -> Result<Vec<history::HistoryEntry>, OperationError> {
        self.db.read_blocking("operations.album_history", |conn| {
            history::album_history(conn, album_id, caller_user_id, self.clock.now_secs())?
                .ok_or(OperationError::NotFound(reasons::ALBUM_NOT_FOUND))
        })
    }

    /// Undo the fold of a leftover album recorded as `fold_id`, for
    /// `actor`: the album and its v2 match come back. Blocking.
    pub fn undo_fold(&self, fold_id: &str, actor: &str) -> Result<(), OperationError> {
        use crate::identity::{FoldUndo, undo_fold};

        self.db.write_blocking(
            Lane::Foreground,
            "operations.undo_fold",
            |tx| match undo_fold(tx, fold_id, Some(actor), self.clock.now_secs())? {
                FoldUndo::Restored => Ok(()),
                FoldUndo::Stale => Err(OperationError::Conflict(reasons::FOLD_STALE)),
                FoldUndo::Unknown => Err(OperationError::NotFound(reasons::FOLD_NOT_FOUND)),
            },
        )
    }

    /// Undo the placeholder-artist repair recorded as `repair_id`, for
    /// `actor`: the album's earlier artist and credits come back. Blocking.
    pub fn undo_artist_repair(&self, repair_id: &str, actor: &str) -> Result<(), OperationError> {
        use crate::repairs::hygiene::{ArtistRepairUndo, undo_artist_from_tracks};

        self.db
            .write_blocking(Lane::Foreground, "operations.undo_artist_repair", |tx| {
                match undo_artist_from_tracks(tx, repair_id, actor, self.clock.now_secs())? {
                    ArtistRepairUndo::Restored => Ok(()),
                    ArtistRepairUndo::Stale => {
                        Err(OperationError::Conflict(reasons::ARTIST_REPAIR_STALE))
                    }
                    ArtistRepairUndo::Unknown => {
                        Err(OperationError::NotFound(reasons::ARTIST_REPAIR_NOT_FOUND))
                    }
                }
            })
    }

    /// "Looks right": confirm the album's unconfirmed match. Blocking.
    pub fn confirm_match(&self, album_id: &str, actor: &str) -> Result<(), OperationError> {
        self.db
            .write_blocking(Lane::Foreground, "operations.confirm_match", |tx| {
                choice::confirm(tx, album_id, actor, self.clock.now_secs())
            })?;
        self.store.link_after_command(album_id);
        Ok(())
    }

    /// Take back the album's last edition change. Blocking.
    pub fn undo_edition_choice(&self, album_id: &str, actor: &str) -> Result<(), OperationError> {
        self.db
            .write_blocking(Lane::Foreground, "operations.undo_edition_choice", |tx| {
                choice::undo(tx, album_id, actor, self.clock.now_secs())
            })
    }

    /// Place the files of the next album whose edition still waits for
    /// them (pins converted by an upgrade, v2 imports, a release a person
    /// approved, an album that gained files). True when
    /// one was handled. Store calls block: run this on a blocking thread.
    pub async fn remap_next(&self) -> Result<bool, OperationError> {
        use choice::RemapOrigin;

        let now = self.clock.now_secs();
        let Some(pending) = self.db.read_blocking("operations.next_remap", |conn| {
            Ok::<_, OperationError>(choice::next_remap(conn, now)?)
        })?
        else {
            return Ok(false);
        };
        let outcome = self
            .candidate_for(
                &pending.local_album_id,
                &pending.release_mbid,
                Lane::Background,
                RequestPriority::BackgroundSync,
            )
            .await;
        let place_only = pending.origin == RemapOrigin::PlaceOnly;
        self.db
            .write_blocking(Lane::Background, "operations.remap", |tx| {
                let now = self.clock.now_secs();
                // A person may have chosen again, or handed the album back,
                // while MusicBrainz was asked: then this remap is stale.
                if !choice::remap_still_wanted(tx, &pending)? {
                    return Ok(true);
                }
                // A person's later word on the album ("leave unmanaged", a
                // custom edition, another edition) stands over the queued one.
                if !choice::remap_still_holds(tx, &pending)? {
                    choice::drop_remap(tx, &pending)?;
                    return Ok(true);
                }
                // Placing files writes only their track rows: the album's
                // match, and the undo of the choice that queued them, stay.
                // Placing on an edition the album already has needs only
                // the candidate's spots; a choice needs its release too.
                let placed = match outcome {
                    Ok(candidate) if place_only => crate::identity::remap::place_files(
                        tx,
                        &pending,
                        &choice::offer_on(&candidate, &pending.release_mbid),
                        now,
                    )
                    .map(|_| ())
                    .map_err(OperationError::from),
                    Ok(candidate) => choice::offer(&candidate).and_then(|offer| {
                        crate::identity::remap::place_choice(tx, &pending, &offer, now)
                            .map(|_| ())
                            .map_err(OperationError::from)
                    }),
                    Err(error) => Err(error),
                };
                match placed {
                    Ok(()) => {
                        crate::catalog_revision::bump_catalog(tx)?;
                    }
                    Err(OperationError::Unavailable(_)) => {
                        choice::retry_remap(tx, &pending, reasons::MUSICBRAINZ_UNAVAILABLE, now)?
                    }
                    // A release whose answer does not read says nothing
                    // about the choice: it is kept and asked for again.
                    Err(OperationError::Conflict(reason))
                        if reason.code == reasons::EDITION_UNREADABLE.code =>
                    {
                        choice::retry_remap(tx, &pending, reason.code, now)?
                    }
                    // The album's files are out of reach for now (a scan may
                    // be moving them): never a reason to drop the choice.
                    Err(OperationError::NotFound(reason))
                        if reason.code == reasons::ALBUM_NOT_FOUND.code =>
                    {
                        choice::retry_remap(tx, &pending, reason.code, now)?
                    }
                    // An edition the album already has is never given up: it
                    // stays, and the album shows why its files are not on it.
                    Err(OperationError::NotFound(reason))
                        if reason.code == reasons::EDITION_NOT_FOUND.code && place_only =>
                    {
                        choice::keep_unplaceable_choice(
                            tx,
                            &pending,
                            reason.code,
                            choice::CHOSEN_NOT_LISTED,
                            now,
                        )?;
                        crate::catalog_revision::bump_catalog(tx)?;
                    }
                    Err(OperationError::NotFound(reason))
                        if reason.code == reasons::EDITION_NOT_FOUND.code =>
                    {
                        choice::give_up_remap(tx, &pending, reason.code, now)?;
                        crate::catalog_revision::bump_catalog(tx)?;
                    }
                    Err(OperationError::Invalid(reason))
                        if reason.code == reasons::EDITION_FITS_NO_FILE.code
                            && pending.origin == RemapOrigin::GroupPin =>
                    {
                        choice::give_up_remap(tx, &pending, reason.code, now)?;
                        crate::catalog_revision::bump_catalog(tx)?;
                    }
                    Err(OperationError::Invalid(reason) | OperationError::NotFound(reason)) => {
                        choice::keep_unplaceable_choice(
                            tx,
                            &pending,
                            reason.code,
                            choice::CHOSEN_FITS_NO_FILE,
                            now,
                        )?;
                        crate::catalog_revision::bump_catalog(tx)?;
                    }
                    Err(other) => return Err(other),
                }
                Ok(true)
            })
    }

    /// Undo the album's last automatic edition. Blocking.
    pub fn undo_automatic_edition(
        &self,
        album_id: &str,
        expected_album_revision: i64,
        expected_identity_revision: i64,
        user_id: &str,
    ) -> Result<UndoOutcome, OperationError> {
        self.db.write_blocking(
            Lane::Foreground,
            "operations.undo_automatic_edition",
            |tx| {
                decisions::undo_automatic_edition(
                    tx,
                    album_id,
                    expected_album_revision,
                    expected_identity_revision,
                    user_id,
                    self.clock.now_secs(),
                )
            },
        )
    }

    /// One page of MusicBrainz releases for the album's edition finder,
    /// with the album's current identity marked: a title (and artist)
    /// search, or every release of `release_group` so the administrator can
    /// pick any edition of the album. Store reads block, so they run on a
    /// blocking thread.
    pub async fn search_releases(
        &self,
        album_id: &str,
        title: &str,
        artist: &str,
        release_group: Option<&str>,
        limit: u32,
        offset: u32,
    ) -> Result<ReleaseSearch, OperationError> {
        let title_query = title.split_whitespace().collect::<Vec<_>>().join(" ");
        let artist_query = artist.split_whitespace().collect::<Vec<_>>().join(" ");
        let release_group_mbid = release_group
            .map(|raw| {
                uuid::Uuid::parse_str(raw.trim())
                    .map(|id| id.hyphenated().to_string())
                    .map_err(|_| OperationError::Invalid(reasons::RELEASE_GROUP_MBID_INVALID))
            })
            .transpose()?;
        if title_query.is_empty() && release_group_mbid.is_none() {
            return Err(OperationError::Invalid(reasons::SEARCH_NEEDS_TITLE));
        }
        let limit = limit.clamp(1, RELEASE_PAGE_MAX);
        let identity = {
            let ops = self.clone();
            let album_id = album_id.to_owned();
            tokio::task::spawn_blocking(move || {
                let known = ops.db.read_blocking("operations.search_releases", |conn| {
                    Ok::<_, OperationError>(store::album_revisions(conn, &album_id)?)
                })?;
                if known.is_none() {
                    return Err(OperationError::NotFound(reasons::ALBUM_NOT_FOUND));
                }
                Ok(ops.store.album_identity(&album_id)?)
            })
            .await
            .map_err(|error| OperationError::Store(error.to_string()))??
        };
        let page = self
            .identify
            .search_editions(
                EditionQuery {
                    title: title_query.clone(),
                    artist: artist_query.clone(),
                    release_group_mbid: release_group_mbid.clone(),
                    limit,
                    offset,
                },
                RequestPriority::UserInitiated,
            )
            .await
            .map_err(|error| OperationError::Unavailable(error.0))?;
        Ok(ReleaseSearch {
            title_query,
            artist_query,
            release_group_query: release_group_mbid,
            current_release_group_mbid: identity
                .as_ref()
                .and_then(|row| row.release_group_mbid.clone()),
            current_release_mbid: identity.and_then(|row| row.release_mbid),
            page,
            limit,
        })
    }

    /// Requeue every re-identification an earlier process left running.
    /// The loop's first turn calls this. Blocking.
    pub fn recover_at_start(&self) -> Result<u64, OperationError> {
        self.db
            .write_blocking(Lane::Background, "operations.recover_at_start", |tx| {
                Ok::<_, OperationError>(jobs::recover_at_start(
                    tx,
                    OperationKind::ExplicitReidentification,
                    self.clock.now_secs(),
                )?)
            })
    }

    /// Requeue the re-identifications whose worker vanished, then claim
    /// the next due one and hand its album to the identification queue,
    /// all in one write. Returns the job as it was left, or `None` when
    /// nothing was due. Blocking.
    pub fn run_next(&self, worker: &str) -> Result<Option<OperationJob>, OperationError> {
        self.db
            .write_blocking(Lane::Background, "operations.run_next", |tx| {
                let now = self.clock.now_secs();
                let claimed =
                    jobs::claim(tx, OperationKind::ExplicitReidentification, worker, now)?;
                let Some(job) = claimed else {
                    return Ok(None);
                };
                if jobs::claim_work(tx, &job.id, now)?.is_none() {
                    return Ok(store::fail(tx, &job.id, worker, "MISSING_WORK", now)?);
                }
                // A pause or stop asked for while the job was queued wins.
                if let Some(halted) = jobs::checkpoint(tx, &job.id, worker, now)? {
                    return Ok(Some(halted));
                }
                let Some(snap) = store::snapshot(tx, &job.id)? else {
                    return Ok(store::fail(tx, &job.id, worker, "MISSING_SNAPSHOT", now)?);
                };
                let gate = store::retry_gate(
                    tx,
                    &self.management,
                    &snap.local_album_id,
                    snap.one_off_local_metadata,
                );
                match gate {
                    Ok(_) => {}
                    Err(
                        OperationError::NotFound(reason)
                        | OperationError::Invalid(reason)
                        | OperationError::Conflict(reason),
                    ) => return Ok(store::fail(tx, &job.id, worker, reason.code, now)?),
                    Err(other) => return Err(other),
                }
                queue_retry(
                    tx,
                    &snap.local_album_id,
                    job.requested_by_user_id.as_deref(),
                    self.clock.now_ms(),
                )?;
                Ok(store::hand_over(tx, &job.id, worker, now)?)
            })
    }
}

/// One job with its results and, for a re-identification, its candidates,
/// read on `conn`. Callers that read other rows too do it in the same read,
/// so every row comes from one commit.
pub(crate) fn job_detail(
    conn: &Connection,
    job_id: &str,
) -> Result<OperationDetail, OperationError> {
    let job =
        jobs::job(conn, job_id)?.ok_or(OperationError::NotFound(reasons::OPERATION_NOT_FOUND))?;
    let (results, results_truncated) = store::work_results(conn, job_id)?;
    let (candidates, selected_candidate_key) = if job.is_reidentification() {
        match store::snapshot(conn, job_id)? {
            Some(snap) => (
                snap.evaluation.map(|e| e.candidates).unwrap_or_default(),
                snap.selected_candidate_key,
            ),
            None => (Vec::new(), None),
        }
    } else {
        (Vec::new(), None)
    };
    Ok(OperationDetail {
        job,
        results,
        results_truncated,
        candidates,
        selected_candidate_key,
    })
}

/// A MusicBrainz release id, hyphenated and lowercase.
fn parse_release(raw: &str) -> Result<String, OperationError> {
    uuid::Uuid::parse_str(raw.trim())
        .map(|id| id.hyphenated().to_string())
        .map_err(|_| OperationError::Invalid(reasons::RELEASE_MBID_INVALID))
}
