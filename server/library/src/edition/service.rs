//! The operations service's edition calls.

use crate::clock::LibraryClock as _;
use crate::edition::choice;
use crate::edition::decisions;
use crate::edition::decisions::indexed_track_ids;
use crate::edition::hand_match;
use crate::edition::looks_wrong;
use crate::edition::models::EditionChoice;
use crate::edition::models::ReleaseSearch;
use crate::edition::models::UndoOutcome;
use crate::edition::plan;
use crate::edition::plan::EditionPlan;
use crate::edition::plan::PlanReading;
use crate::identify::models::LocalAlbumFacts;
use crate::identify::providers::RELEASE_UNREADABLE;
use crate::identify::sources::EditionQuery;
use crate::identity::reads::album_identity_row;
use crate::jobs::models::OperationError;
use crate::jobs::service::Operations;
use crate::matching::model::Release;
use crate::reasons;
use crate::reidentify::candidates::evaluation;
use crate::reidentify::models::ReidentificationCandidate;
use crate::reidentify::store;
use droppedneedle_kernel::db::Lane;
use droppedneedle_providers::RequestPriority;

/// Largest edition-finder page.
pub const RELEASE_PAGE_MAX: u32 = 12;

impl Operations {
    /// Where the album's edition stands. Blocking.
    pub fn edition_status(
        &self,
        album_id: &str,
    ) -> Result<Option<crate::edition::status::EditionStatus>, OperationError> {
        self.db.read_blocking("operations.edition_status", |conn| {
            Ok::<_, OperationError>(crate::edition::status::edition_status(conn, album_id)?)
        })
    }

    /// One page of the albums whose match waits for a person. Blocking.
    pub fn waiting_albums(
        &self,
        which: crate::edition::status::Waiting,
        limit: u32,
        offset: u32,
    ) -> Result<(Vec<crate::edition::status::WaitingAlbum>, u64), OperationError> {
        self.db.read_blocking("operations.waiting_albums", |conn| {
            Ok::<_, OperationError>(crate::edition::status::waiting_albums(
                conn, which, limit, offset,
            )?)
        })
    }

    /// Choose the album's edition: any MusicBrainz release, from the
    /// album's release group or another one. MusicBrainz is asked for the
    /// release's tracklist, the files are placed on it where they fit, and
    /// the release becomes the album's protected edition. A release that
    /// fits none of the files is refused unless `use_anyway`, which takes
    /// it placing none. A stale `expected_identity_revision` is refused
    /// before MusicBrainz is asked and again when the choice is written.
    pub async fn choose_edition(
        &self,
        album_id: &str,
        raw_release: &str,
        expected_identity_revision: i64,
        actor: Option<&str>,
        use_anyway: bool,
    ) -> Result<EditionChoice, OperationError> {
        let release = parse_release(raw_release)?;
        let album = album_id.to_owned();
        self.db
            .read("operations.choose_edition_guard", move |conn| {
                choice::guard_revision(conn, &album, expected_identity_revision)
            })
            .await?;
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
                        choice::guard_revision(tx, &album, expected_identity_revision)?;
                        let chosen = choice::choose(
                            tx,
                            &album,
                            &candidate,
                            actor.as_deref(),
                            use_anyway,
                            ops.clock.now_secs(),
                        )?;
                        looks_wrong::clear(tx, &album)?;
                        Ok::<_, OperationError>(chosen)
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
                    )?;
                    Ok::<_, OperationError>(looks_wrong::clear(tx, &album)?)
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
                )?;
                Ok::<_, OperationError>(looks_wrong::clear(tx, album_id)?)
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

    /// The library's copy of a release group, when it has an edition: the
    /// copy the album page shows, as acquisition picks it.
    pub async fn album_for_group(
        &self,
        release_group_mbid: &str,
    ) -> Result<Option<String>, OperationError> {
        let (ops, group) = (self.clone(), release_group_mbid.trim().to_ascii_lowercase());
        tokio::task::spawn_blocking(move || {
            ops.db.read_blocking("operations.album_for_group", |conn| {
                Ok::<_, OperationError>(
                    crate::identity::reads::copy_for_group(conn, &group)?
                        .filter(|copy| copy.release_mbid.is_some())
                        .map(|copy| copy.album_id),
                )
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
                    )?;
                    // The match makes the edition the person's choice.
                    Ok::<_, OperationError>(looks_wrong::clear(tx, &album)?)
                })
        })
        .await
        .map_err(|error| OperationError::Store(error.to_string()))?
    }

    /// "Undo match" on one file the person matched by hand, decided on
    /// the album's `expected_identity_revision`. Blocking.
    pub fn unmatch_file(
        &self,
        album_id: &str,
        track_id: &str,
        expected_identity_revision: i64,
        actor: &str,
    ) -> Result<(), OperationError> {
        self.db
            .write_blocking(Lane::Foreground, "operations.unmatch_file", |tx| {
                hand_match::unmatch_file(
                    tx,
                    album_id,
                    track_id,
                    expected_identity_revision,
                    actor,
                    self.clock.now_secs(),
                )
            })
    }

    /// "Let DroppedNeedle choose": hand the album's edition back to
    /// automatic best fit and queue an identification, decided on the
    /// album's `expected_identity_revision`. Blocking.
    pub fn hand_back_edition(
        &self,
        album_id: &str,
        expected_identity_revision: i64,
        actor: &str,
    ) -> Result<(), OperationError> {
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
                choice::guard_revision(tx, album_id, expected_identity_revision)?;
                choice::hand_back(tx, album_id, actor, self.clock.now_secs())?;
                Ok::<_, OperationError>(looks_wrong::clear(tx, album_id)?)
            })
    }

    /// "Looks right": confirm the album's unconfirmed match, decided on
    /// the album's `expected_identity_revision`. Blocking.
    pub fn confirm_match(
        &self,
        album_id: &str,
        expected_identity_revision: i64,
        actor: &str,
    ) -> Result<(), OperationError> {
        self.db
            .write_blocking(Lane::Foreground, "operations.confirm_match", |tx| {
                choice::guard_revision(tx, album_id, expected_identity_revision)?;
                choice::confirm(tx, album_id, actor, self.clock.now_secs())?;
                Ok::<_, OperationError>(looks_wrong::clear(tx, album_id)?)
            })?;
        self.store.link_after_command(album_id);
        Ok(())
    }

    /// Take back the album's last edition change, decided on the album's
    /// `expected_identity_revision`. Blocking.
    pub fn undo_edition_choice(
        &self,
        album_id: &str,
        expected_identity_revision: i64,
        actor: &str,
    ) -> Result<(), OperationError> {
        self.db
            .write_blocking(Lane::Foreground, "operations.undo_edition_choice", |tx| {
                choice::guard_revision(tx, album_id, expected_identity_revision)?;
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
}

/// A MusicBrainz release id, hyphenated and lowercase.
fn parse_release(raw: &str) -> Result<String, OperationError> {
    uuid::Uuid::parse_str(raw.trim())
        .map(|id| id.hyphenated().to_string())
        .map_err(|_| OperationError::Invalid(reasons::RELEASE_MBID_INVALID))
}
