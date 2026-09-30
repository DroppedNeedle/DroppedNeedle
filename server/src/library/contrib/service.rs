//! Contribution service: draft lifecycle plus the MusicBrainz submission
//! path (duplicate check -> seed -> callback/manual result -> verify).
//!
//! Ports v2 `LibraryContributionService`. Submission performs NO provider
//! write: `create_seed` returns a form the curator's browser POSTs to the
//! release editor, and the editor calls back here. Tests post that form to a
//! scripted fake editor instead (see `memory::FakeReleaseEditor`).

use std::collections::HashMap;
use std::sync::Arc;

use droppedneedle::providers::slots::RequestPriority;
use sha2::{Digest as _, Sha256};

use super::error::ContribError;
use super::models::*;
use super::rules::*;
use super::seams::*;

pub struct ContributionService {
    store: Arc<dyn ContributionStore>,
    identity: Arc<dyn ContributionIdentity>,
    discogs: Option<Arc<dyn DiscogsContrib>>,
    musicbrainz: Option<Arc<dyn MusicBrainzContrib>>,
    catalog: Option<Arc<dyn ContributionCatalog>>,
    evidence: Arc<dyn AttachmentEvidence>,
    clock: Arc<dyn ContributionClock>,
}

impl ContributionService {
    pub fn new(
        store: Arc<dyn ContributionStore>,
        identity: Arc<dyn ContributionIdentity>,
        evidence: Arc<dyn AttachmentEvidence>,
        clock: Arc<dyn ContributionClock>,
    ) -> Self {
        Self {
            store,
            identity,
            discogs: None,
            musicbrainz: None,
            catalog: None,
            evidence,
            clock,
        }
    }

    pub fn with_discogs(mut self, discogs: Arc<dyn DiscogsContrib>) -> Self {
        self.discogs = Some(discogs);
        self
    }

    pub fn with_musicbrainz(mut self, musicbrainz: Arc<dyn MusicBrainzContrib>) -> Self {
        self.musicbrainz = Some(musicbrainz);
        self
    }

    pub fn with_catalog(mut self, catalog: Arc<dyn ContributionCatalog>) -> Self {
        self.catalog = Some(catalog);
        self
    }

    pub fn store(&self) -> &Arc<dyn ContributionStore> {
        &self.store
    }

    pub fn evidence(&self) -> &Arc<dyn AttachmentEvidence> {
        &self.evidence
    }

    pub fn now(&self) -> f64 {
        self.clock.now_seconds()
    }

    fn require_discogs(&self) -> Result<&Arc<dyn DiscogsContrib>, ContribError> {
        self.discogs
            .as_ref()
            .ok_or_else(|| ContribError::Data("The Discogs adapter is not available.".into()))
    }

    fn require_musicbrainz(&self) -> Result<&Arc<dyn MusicBrainzContrib>, ContribError> {
        self.musicbrainz
            .as_ref()
            .ok_or_else(|| ContribError::Data("The MusicBrainz adapter is not available.".into()))
    }

    // -- lifecycle ----------------------------------------------------------

    pub async fn create(
        &self,
        album_id: &str,
        actor_user_id: &str,
    ) -> Result<ContributionRecord, ContribError> {
        let snapshot = self.build_local_snapshot(album_id).await?;
        if snapshot.musicbrainz_release_id.is_some() {
            return Err(ContribError::state(
                "This local album already has an exact MusicBrainz release.",
            ));
        }
        let draft = draft_from_snapshot(&snapshot);
        let row = self
            .store
            .create_or_get(
                album_id,
                actor_user_id,
                snapshot.album_row_revision,
                &snapshot.input_revision,
                &snapshot,
                &draft,
                &ContributionSourceSelection::default(),
                self.now(),
            )
            .await?;
        self.record(row, None).await
    }

    pub async fn get(&self, contribution_id: &str) -> Result<ContributionRecord, ContribError> {
        let row = self
            .store
            .get(contribution_id)
            .await
            .ok_or(ContribError::ContributionNotFound)?;
        // Quirk (v2): terminal rows never transition; stale input on an
        // active row flips it to stale on READ.
        let row = if row_is_stale(&row)
            && !matches!(
                row.state,
                ContributionState::Linked | ContributionState::Cancelled | ContributionState::Stale
            ) {
            self.store
                .mark_stale(contribution_id, row.row_revision, self.now())
                .await?
        } else {
            row
        };
        self.record(row, None).await
    }

    pub async fn active_for_album(
        &self,
        album_id: &str,
    ) -> Result<Option<ContributionRecord>, ContribError> {
        let row = match self.store.get_active_for_album(album_id).await {
            Some(row) => row,
            None => return Ok(None),
        };
        // Quirk (v2): unlike `get`, this path marks stale WITHOUT the
        // terminal-state guard (the store no-ops terminal rows itself).
        let row = if row_is_stale(&row) {
            self.store
                .mark_stale(&row.id, row.row_revision, self.now())
                .await?
        } else {
            row
        };
        Ok(Some(self.record(row, None).await?))
    }

    pub async fn update(
        &self,
        contribution_id: &str,
        expected_row_revision: i64,
        draft: &ReleaseDraft,
        actor_user_id: &str,
    ) -> Result<ContributionRecord, ContribError> {
        let current = self.get(contribution_id).await?;
        if current.state == ContributionState::Stale {
            return Err(ContribError::state(
                "The local album changed. Rebuild this contribution before editing.",
            ));
        }
        let discogs_release = self.current_discogs_release(&current).await?;
        let normalized = normalize_draft(
            draft,
            &current.local_snapshot,
            &current.source_selection,
            discogs_release.as_ref(),
        )
        .map_err(map_draft_error)?;
        let issues = validate_draft(&normalized, &current.local_snapshot);
        let state = if issues.is_empty() {
            ContributionState::Ready
        } else {
            ContributionState::Draft
        };
        let row = self
            .store
            .compare_and_set(
                contribution_id,
                expected_row_revision,
                actor_user_id,
                self.now(),
                ContributionUpdate::Draft {
                    draft: normalized,
                    state,
                },
            )
            .await?;
        self.record(row, None).await
    }

    pub async fn rebuild(
        &self,
        contribution_id: &str,
        expected_row_revision: i64,
        actor_user_id: &str,
    ) -> Result<ContributionRecord, ContribError> {
        let current = self.get(contribution_id).await?;
        if current.state != ContributionState::Stale {
            return Err(ContribError::state(
                "Only a stale contribution needs to be rebuilt.",
            ));
        }
        let snapshot = self.build_local_snapshot(&current.local_album_id).await?;
        if snapshot.musicbrainz_release_id.is_some() {
            return Err(ContribError::state(
                "This local album already has an exact MusicBrainz release.",
            ));
        }
        let draft = draft_from_snapshot(&snapshot);
        let row = self
            .store
            .rebuild(
                contribution_id,
                expected_row_revision,
                actor_user_id,
                snapshot.album_row_revision,
                &snapshot.input_revision,
                &snapshot,
                &draft,
                &ContributionSourceSelection::default(),
                self.now(),
            )
            .await?;
        self.record(row, None).await
    }

    pub async fn cancel(
        &self,
        contribution_id: &str,
        expected_row_revision: i64,
        actor_user_id: &str,
    ) -> Result<ContributionRecord, ContribError> {
        let now = self.now();
        let row = self
            .store
            .compare_and_set(
                contribution_id,
                expected_row_revision,
                actor_user_id,
                now,
                ContributionUpdate::Cancel,
            )
            .await?;
        let row = self.maybe_purge_row(&row, now).await?;
        self.record(row, None).await
    }

    // -- Discogs source ------------------------------------------------------

    pub async fn search_discogs(
        &self,
        contribution_id: &str,
        query: Option<&str>,
    ) -> Result<Vec<DiscogsReleaseCandidate>, ContribError> {
        let current = self.get(contribution_id).await?;
        if matches!(
            current.state,
            ContributionState::Stale | ContributionState::Linked | ContributionState::Cancelled
        ) {
            return Err(ContribError::state(
                "This contribution cannot search for another source.",
            ));
        }
        let collapsed = query
            .unwrap_or("")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let search_query = if collapsed.is_empty() {
            format!(
                "{} {}",
                current.local_snapshot.album_artist_name, current.local_snapshot.title
            )
            .trim()
            .to_string()
        } else {
            collapsed
        };
        if search_query.len() < 2 {
            return Err(ContribError::validation(
                "Enter a release title, artist, barcode, URL, or ID.",
            ));
        }
        if search_query.len() > 200 {
            return Err(ContribError::validation("The Discogs search is too long."));
        }
        self.require_discogs()?
            .search_releases(&search_query, 8, RequestPriority::UserInitiated)
            .await
    }

    pub async fn select_discogs(
        &self,
        contribution_id: &str,
        release_id_or_url: &str,
        expected_row_revision: i64,
        actor_user_id: &str,
    ) -> Result<ContributionRecord, ContribError> {
        let current = self.get(contribution_id).await?;
        if current.row_revision != expected_row_revision {
            return Err(ContribError::state(
                "The contribution changed before the source was selected.",
            ));
        }
        let release_id = parse_discogs_release_id(release_id_or_url)
            .map_err(|e| ContribError::validation(e.message()))?;
        // Quirk (v2): no editable-state guard here; the store owns it
        // ("This contribution can no longer be edited.").
        let release = self
            .require_discogs()?
            .get_release(&release_id, RequestPriority::UserInitiated)
            .await?
            .ok_or_else(|| {
                ContribError::Missing("That Discogs release could not be found.".into())
            })?;
        let alignments = align_tracks(&current.local_snapshot, &release);
        let mut sources = vec![SourceReference {
            provider: "discogs".into(),
            entity_type: "release".into(),
            external_id: release.release_id.clone(),
            canonical_url: release.canonical_release_url.clone(),
            fetched_at: Some(release.source_fetched_at),
        }];
        if let (Some(master_id), Some(master_url)) = (
            release.master_id.clone(),
            release.canonical_master_url.clone(),
        ) {
            sources.push(SourceReference {
                provider: "discogs".into(),
                entity_type: "master".into(),
                external_id: master_id,
                canonical_url: master_url,
                fetched_at: Some(release.source_fetched_at),
            });
        }
        let selection = ContributionSourceSelection {
            schema_version: 1,
            sources,
            alignments,
        };
        let expires_at = release.source_fetched_at + DISCOGS_DISPLAY_SECONDS;
        let row = self
            .store
            .compare_and_set(
                contribution_id,
                expected_row_revision,
                actor_user_id,
                self.now(),
                ContributionUpdate::SelectDiscogs {
                    release: release.clone(),
                    selection,
                    expires_at,
                },
            )
            .await?;
        self.record(row, Some(release)).await
    }

    pub async fn remove_discogs(
        &self,
        contribution_id: &str,
        expected_row_revision: i64,
        actor_user_id: &str,
    ) -> Result<ContributionRecord, ContribError> {
        let current = self.get(contribution_id).await?;
        if current.row_revision != expected_row_revision {
            return Err(ContribError::state(
                "The contribution changed before the source was removed.",
            ));
        }
        let draft = without_discogs_values(&current.draft, &current.local_snapshot);
        let issues = validate_draft(&draft, &current.local_snapshot);
        let state = if issues.is_empty() {
            ContributionState::Ready
        } else {
            ContributionState::Draft
        };
        let row = self
            .store
            .compare_and_set(
                contribution_id,
                expected_row_revision,
                actor_user_id,
                self.now(),
                ContributionUpdate::RemoveDiscogs { draft, state },
            )
            .await?;
        self.record(row, None).await
    }

    // -- duplicate check -----------------------------------------------------

    pub async fn check_duplicates(
        &self,
        contribution_id: &str,
        expected_row_revision: i64,
        actor_user_id: &str,
        different_edition_confirmed: bool,
    ) -> Result<ContributionRecord, ContribError> {
        let current = self.get(contribution_id).await?;
        if current.row_revision != expected_row_revision {
            return Err(ContribError::state(
                "The contribution changed before the duplicate check started.",
            ));
        }
        if !matches!(
            current.state,
            ContributionState::Ready | ContributionState::NeedsReview
        ) || !current.validation.is_empty()
        {
            return Err(ContribError::state(
                "Complete the contribution draft before checking MusicBrainz.",
            ));
        }
        if current.discogs_source.as_ref().is_some_and(|s| s.expired) {
            return Err(ContribError::ProviderExpired(
                "Refresh the Discogs source before checking MusicBrainz.".into(),
            ));
        }
        let musicbrainz = self.require_musicbrainz()?.clone();
        let discogs_release = current
            .discogs_source
            .as_ref()
            .and_then(|s| s.release.clone());
        let mut candidates: HashMap<String, DuplicateCandidate> = HashMap::new();
        let mut exact_release_ids: Vec<String> = Vec::new();
        let mut group_ids: Vec<String> = Vec::new();
        if let Some(release) = discogs_release.as_ref() {
            let exact = musicbrainz
                .resolve_url(
                    &release.canonical_release_url,
                    UrlRelation::Release,
                    RequestPriority::UserInitiated,
                    false,
                )
                .await?;
            exact_release_ids = exact.release_mbids;
            if let Some(master_url) = release.canonical_master_url.as_deref() {
                let groups = musicbrainz
                    .resolve_url(
                        master_url,
                        UrlRelation::ReleaseGroup,
                        RequestPriority::UserInitiated,
                        false,
                    )
                    .await?;
                group_ids = groups.release_group_mbids;
            }
        }
        for release_mbid in &exact_release_ids {
            let verified = musicbrainz
                .get_release_for_verification(release_mbid, RequestPriority::UserInitiated, false)
                .await
                .map_err(provider_failure_to_contrib)?;
            candidates.insert(
                format!("release:{release_mbid}"),
                duplicate_candidate(
                    &current.draft,
                    verified.as_ref(),
                    release_mbid.clone(),
                    DuplicateEvidenceKind::ExactDiscogsUrl,
                    true,
                ),
            );
        }
        for group_mbid in &group_ids {
            candidates.insert(
                format!("group:{group_mbid}"),
                DuplicateCandidate {
                    release_mbid: None,
                    release_group_mbid: Some(group_mbid.clone()),
                    title: current.draft.title.text().to_string(),
                    artist_name: current.draft.artist_credit.text().to_string(),
                    evidence_kind: DuplicateEvidenceKind::ReleaseGroup,
                    exact: false,
                    differences: vec![
                        "This Discogs master is linked to an existing release group.".into(),
                    ],
                },
            );
        }
        let facts = DuplicateSearchFacts {
            title: current.draft.title.text().to_string(),
            artist_name: current.draft.artist_credit.text().to_string(),
            barcode: current.draft.barcode.value.clone(),
            country: current.draft.country.value.clone(),
            date: current.draft.release_date.value.clone(),
        };
        let similar = musicbrainz
            .search_duplicate_releases(&facts, 8, RequestPriority::UserInitiated)
            .await?;
        for verified in &similar {
            let key = format!("release:{}", verified.release_mbid);
            if candidates.contains_key(&key) {
                continue;
            }
            // Quirk (v2): barcode evidence needs the barcode on BOTH sides.
            let evidence_kind = if facts.barcode.is_some()
                && verified.barcode.is_some()
                && facts.barcode == verified.barcode
            {
                DuplicateEvidenceKind::Barcode
            } else {
                DuplicateEvidenceKind::Similar
            };
            candidates.insert(
                key,
                duplicate_candidate(
                    &current.draft,
                    Some(verified),
                    verified.release_mbid.clone(),
                    evidence_kind,
                    false,
                ),
            );
        }
        let mut ordered: Vec<DuplicateCandidate> = candidates.into_values().collect();
        sort_duplicate_candidates(&mut ordered);
        let serious_similar = ordered.iter().any(|c| {
            matches!(
                c.evidence_kind,
                DuplicateEvidenceKind::Barcode | DuplicateEvidenceKind::Similar
            )
        });
        let state = duplicate_check_state(
            !exact_release_ids.is_empty(),
            group_ids.len(),
            serious_similar,
            different_edition_confirmed,
        );
        let now = self.now();
        let result = DuplicateCheckResult {
            schema_version: 1,
            checked_at: now,
            input_revision: current.input_revision.clone(),
            candidates: ordered,
            // Quirk (v2): the confirmation is dropped when exact hits exist.
            different_edition_confirmed: different_edition_confirmed
                && exact_release_ids.is_empty(),
        };
        let row = self
            .store
            .compare_and_set(
                contribution_id,
                expected_row_revision,
                actor_user_id,
                now,
                ContributionUpdate::DuplicateResult { result, state },
            )
            .await?;
        self.record(row, discogs_release).await
    }

    // -- attach ---------------------------------------------------------------

    pub async fn attach_existing(
        &self,
        contribution_id: &str,
        release_mbid: &str,
        expected_row_revision: i64,
        actor_user_id: &str,
    ) -> Result<ContributionRecord, ContribError> {
        let current = self.get(contribution_id).await?;
        if current.row_revision != expected_row_revision {
            return Err(ContribError::state(
                "The contribution changed before the release could be attached.",
            ));
        }
        let duplicate = current.duplicate_result.as_ref().ok_or_else(|| {
            ContribError::DuplicateCheckRequired(
                "Run the MusicBrainz duplicate check first.".into(),
            )
        })?;
        if duplicate.candidates.iter().filter(|c| c.exact).count() > 1 {
            return Err(ContribError::ResultMismatch(
                "The Discogs relationship points to more than one MusicBrainz release.".into(),
            ));
        }
        if !duplicate
            .candidates
            .iter()
            .any(|c| c.release_mbid.as_deref() == Some(release_mbid))
        {
            return Err(ContribError::ResultMismatch(
                "That release is not in the current duplicate-check result.".into(),
            ));
        }
        let verified = self
            .require_musicbrainz()?
            .get_release_for_verification(release_mbid, RequestPriority::UserInitiated, true)
            .await
            .map_err(provider_failure_to_contrib)?
            .ok_or_else(|| {
                ContribError::Missing("The MusicBrainz release could not be verified.".into())
            })?;
        let (decision, context) = self.build_attachment_evidence(&current, &verified).await?;
        if decision.outcome != AttachmentOutcome::Identified {
            return Err(ContribError::ResultMismatch(
                "The MusicBrainz release does not safely match the current draft.".into(),
            ));
        }
        let now = self.now();
        let attempt = self.verification_attempt(
            &current,
            &decision,
            Some(actor_user_id),
            &context.tracks,
            "identified",
            decision.reason_code.clone(),
            now,
        );
        let row = self
            .store
            .compare_and_set(
                contribution_id,
                expected_row_revision,
                actor_user_id,
                now,
                ContributionUpdate::AttachExisting {
                    release_mbid: verified.release_mbid.clone(),
                    release_group_mbid: verified.release_group_mbid.clone(),
                    artist_mbid: verified.artist_mbid.clone(),
                    attempt,
                },
            )
            .await?;
        let row = self.maybe_purge_row(&row, now).await?;
        // ST1: scoped invalidation BEFORE returning; then the identified hook.
        if let Some(catalog) = self.catalog.as_ref() {
            let groups = vec![verified.release_group_mbid.clone()];
            let artists: Vec<String> = verified.artist_mbid.clone().into_iter().collect();
            catalog.invalidate_identity_scope(&groups, &artists).await;
            let (_, _, policy) = self.identity.input_revisions(&context.tracks);
            catalog
                .after_identified(&current.local_album_id, &policy)
                .await;
        }
        self.record(row, None).await
    }

    // -- submission: seed -----------------------------------------------------

    pub async fn create_seed(
        &self,
        contribution_id: &str,
        expected_row_revision: i64,
        actor_user_id: &str,
        public_base_url: &str,
    ) -> Result<MusicBrainzSeed, ContribError> {
        validate_public_base_url(public_base_url)?;
        let current = self.get(contribution_id).await?;
        if current.row_revision != expected_row_revision {
            return Err(ContribError::state(
                "The contribution changed before the editor could be opened.",
            ));
        }
        let duplicate = current.duplicate_result.as_ref().ok_or_else(|| {
            ContribError::DuplicateCheckRequired(
                "Run the MusicBrainz duplicate check first.".into(),
            )
        })?;
        if duplicate.candidates.iter().any(|c| c.exact) {
            return Err(ContribError::ExactDuplicate(
                "This Discogs release already has a MusicBrainz release.".into(),
            ));
        }
        let serious = duplicate.candidates.iter().any(|c| {
            matches!(
                c.evidence_kind,
                DuplicateEvidenceKind::Barcode | DuplicateEvidenceKind::Similar
            )
        });
        if serious && !duplicate.different_edition_confirmed {
            return Err(ContribError::DuplicateCheckRequired(
                "Confirm that the MusicBrainz candidates are different editions.".into(),
            ));
        }
        let discogs_release = self.current_discogs_release(&current).await?;
        if current.discogs_source.is_some() && discogs_release.is_none() {
            return Err(ContribError::ProviderExpired(
                "Refresh the Discogs source before opening MusicBrainz.".into(),
            ));
        }
        if let Some(release) = discogs_release.as_ref() {
            // Race guard (v2): re-resolve fresh; a new link aborts the seed.
            let fresh = self
                .require_musicbrainz()?
                .resolve_url(
                    &release.canonical_release_url,
                    UrlRelation::Release,
                    RequestPriority::UserInitiated,
                    true,
                )
                .await?;
            if !fresh.release_mbids.is_empty() {
                return Err(ContribError::ExactDuplicate(
                    "This Discogs release is now linked to MusicBrainz. Run the duplicate check again."
                        .into(),
                ));
            }
        }
        let token = callback_token();
        let token_hash = sha256_hex(token.as_bytes());
        let now = self.now();
        let expires_at = now + CALLBACK_TOKEN_SECONDS;
        let callback_url = format!(
            "{}{CALLBACK_PATH}?token={token}",
            public_base_url.trim_end_matches('/')
        );
        let context = self
            .identity
            .album_context(&current.local_album_id)
            .await
            .unwrap_or_default();
        let recording_ids: HashMap<String, String> = context
            .tracks
            .iter()
            .filter_map(|t| t.recording_mbid.clone().map(|mbid| (t.id.clone(), mbid)))
            .collect();
        let fields = musicbrainz_seed_fields(
            &current.draft,
            &current.local_snapshot,
            current.duplicate_result.as_ref(),
            discogs_release.as_ref(),
            &recording_ids,
            &callback_url,
        );
        // Quirk (v2): the persisted snapshot omits redirect_uri (token
        // secrecy); the hash covers the FULL field list.
        let safe_snapshot = serde_json::json!({
            "schema_version": 1,
            "input_revision": current.input_revision,
            "fields": fields.iter()
                .filter(|f| f.name != "redirect_uri")
                .map(|f| serde_json::json!({"name": f.name, "value": f.value}))
                .collect::<Vec<_>>(),
        });
        let snapshot_json = serde_json::to_string(&safe_snapshot).unwrap_or_default();
        let seed_hash = sha256_hex(
            serde_json::to_string(&fields)
                .unwrap_or_default()
                .as_bytes(),
        );
        let row = self
            .store
            .compare_and_set(
                contribution_id,
                expected_row_revision,
                actor_user_id,
                now,
                ContributionUpdate::PrepareSeed {
                    token_hash,
                    token_expires_at: expires_at,
                    seed_snapshot_json: snapshot_json,
                    seed_hash,
                },
            )
            .await?;
        Ok(MusicBrainzSeed {
            action_url: MUSICBRAINZ_RELEASE_EDITOR.into(),
            method: "POST".into(),
            fields,
            contribution_revision: row.row_revision,
            expires_at,
        })
    }

    // -- submission: results ---------------------------------------------------

    pub async fn consume_callback(
        &self,
        token: Option<&str>,
        release_mbid: Option<&str>,
    ) -> Result<String, ContribError> {
        let token = token.unwrap_or("");
        if !valid_callback_token(token) {
            return Err(ContribError::validation(
                "The MusicBrainz callback token is invalid.",
            ));
        }
        let release_mbid = release_mbid.unwrap_or("");
        if release_mbid.is_empty() || release_mbid.len() > 64 {
            return Err(ContribError::validation(
                "The MusicBrainz release MBID is invalid.",
            ));
        }
        let normalized = parse_musicbrainz_release_id(release_mbid)
            .map_err(|_| ContribError::validation("The MusicBrainz release MBID is invalid."))?;
        let token_hash = sha256_hex(token.as_bytes());
        let (contribution_id, _) = self
            .store
            .consume_callback_token(&token_hash, &normalized, self.now())
            .await?
            .ok_or_else(|| {
                ContribError::Missing("Contribution callback is invalid or expired.".into())
            })?;
        Ok(contribution_id)
    }

    pub async fn record_manual_result(
        &self,
        contribution_id: &str,
        release_id_or_url: &str,
        expected_row_revision: i64,
        actor_user_id: &str,
        replace_existing_result: bool,
    ) -> Result<ContributionRecord, ContribError> {
        let release_mbid = parse_musicbrainz_release_id(release_id_or_url)
            .map_err(|e| ContribError::validation(e.message()))?;
        let row = self
            .store
            .compare_and_set(
                contribution_id,
                expected_row_revision,
                actor_user_id,
                self.now(),
                ContributionUpdate::ManualResult {
                    release_mbid,
                    replace_existing: replace_existing_result,
                },
            )
            .await?;
        self.record(row, None).await
    }

    pub async fn retry_verification(
        &self,
        contribution_id: &str,
        expected_row_revision: i64,
        actor_user_id: &str,
    ) -> Result<ContributionRecord, ContribError> {
        let row = self
            .store
            .compare_and_set(
                contribution_id,
                expected_row_revision,
                actor_user_id,
                self.now(),
                ContributionUpdate::RequeueVerification,
            )
            .await?;
        self.record(row, None).await
    }

    // -- evidence + attempts ---------------------------------------------------

    pub async fn build_attachment_evidence(
        &self,
        contribution: &ContributionRecord,
        verified: &MusicBrainzVerifiedRelease,
    ) -> Result<(AttachmentDecision, AlbumIdentificationContext), ContribError> {
        let context = self
            .identity
            .album_context(&contribution.local_album_id)
            .await
            .ok_or(ContribError::AlbumNotFound)?;
        if context.album.is_none() {
            return Err(ContribError::AlbumNotFound);
        }
        let recording_mbids: HashMap<String, Option<String>> = context
            .tracks
            .iter()
            .map(|t| {
                (
                    t.id.clone(),
                    t.recording_mbid
                        .clone()
                        .or_else(|| t.embedded_recording_mbid.clone()),
                )
            })
            .collect();
        let relative_paths: HashMap<String, String> = context
            .tracks
            .iter()
            .map(|t| (t.id.clone(), t.relative_path.clone()))
            .collect();
        let decision = self
            .evidence
            .decide_attachment(contribution, verified, &recording_mbids, &relative_paths)
            .await;
        Ok((decision, context))
    }

    #[allow(clippy::too_many_arguments)]
    pub fn verification_attempt(
        &self,
        contribution: &ContributionRecord,
        decision: &AttachmentDecision,
        requested_by: Option<&str>,
        tracks: &[IdentityTrack],
        state: &str,
        reason_code: Option<String>,
        now: f64,
    ) -> ContributionVerificationAttempt {
        let _ = tracks;
        ContributionVerificationAttempt {
            id: uuid::Uuid::new_v4().to_string(),
            local_album_id: contribution.local_album_id.clone(),
            requested_by_user_id: requested_by.map(str::to_string),
            matcher_version: self.evidence.matcher_version(),
            state: state.to_string(),
            terminal_reason_code: reason_code.or_else(|| decision.reason_code.clone()),
            selected_candidate_key: decision.selected_candidate_key.clone(),
            candidate_count: decision.candidates.len(),
            candidate_keys: decision
                .candidates
                .iter()
                .map(AttachmentCandidate::key)
                .collect(),
            started_at: now,
            completed_at: now,
        }
    }

    // -- purge + cache ----------------------------------------------------------

    pub async fn purge_expired_provider_data(
        &self,
        now: f64,
        limit: usize,
    ) -> Result<usize, ContribError> {
        let rows = self.store.list_for_provider_purge(now, limit).await;
        let mut purged = 0;
        for row in rows {
            if self.purge_row(&row, now).await? {
                purged += 1;
            }
        }
        Ok(purged)
    }

    pub async fn purge_provider_data(
        &self,
        contribution_id: &str,
        now: f64,
    ) -> Result<bool, ContribError> {
        let row = match self.store.get(contribution_id).await {
            Some(row) => row,
            None => return Ok(false),
        };
        self.purge_row(&row, now).await
    }

    async fn maybe_purge_row(
        &self,
        row: &ContributionRow,
        now: f64,
    ) -> Result<ContributionRow, ContribError> {
        if self.purge_row(row, now).await?
            && let Some(fresh) = self.store.get(&row.id).await
        {
            return Ok(fresh);
        }
        Ok(row.clone())
    }

    async fn purge_row(&self, row: &ContributionRow, now: f64) -> Result<bool, ContribError> {
        if row.provider_snapshot_expires_at.is_none() {
            return Ok(false);
        }
        let draft = without_discogs_values(&row.draft, &row.local_snapshot);
        let mut selection = row.source_selection.clone();
        selection.alignments.clear();
        match self
            .store
            .compare_and_set(
                &row.id,
                row.row_revision,
                row.updated_by_user_id.as_deref().unwrap_or(""),
                now,
                ContributionUpdate::PurgeProviderData { draft, selection },
            )
            .await
        {
            Ok(_) => Ok(true),
            Err(ContribError::State(_)) => Ok(false),
            Err(other) => Err(other),
        }
    }

    pub async fn invalidate_catalog_cache(&self) {
        if let Some(catalog) = self.catalog.as_ref() {
            catalog.invalidate_identification().await;
        }
    }

    /// Post-link housekeeping (v2 worker `run_claimed`): drop provider
    /// snapshots, sweep identification caches, fire the identified hook.
    pub async fn after_linked(
        &self,
        contribution: &ContributionRecord,
        input_policy_revision: &str,
        now: f64,
    ) -> Result<(), ContribError> {
        self.purge_provider_data(&contribution.id, now).await?;
        self.invalidate_catalog_cache().await;
        if let Some(catalog) = self.catalog.as_ref() {
            catalog
                .after_identified(&contribution.local_album_id, input_policy_revision)
                .await;
        }
        Ok(())
    }

    // -- internals ---------------------------------------------------------------

    async fn current_discogs_release(
        &self,
        contribution: &ContributionRecord,
    ) -> Result<Option<DiscogsRelease>, ContribError> {
        let source = contribution
            .source_selection
            .sources
            .iter()
            .find(|s| s.provider == "discogs" && s.entity_type == "release");
        let Some(source) = source else {
            return Ok(None);
        };
        // Quirk (v2): a missing expiry counts as expired.
        match contribution.provider_snapshot_expires_at {
            Some(expires_at) if expires_at > self.now() => {}
            _ => return Ok(None),
        }
        self.require_discogs()?
            .get_release(&source.external_id, RequestPriority::UserInitiated)
            .await
    }

    async fn build_local_snapshot(
        &self,
        album_id: &str,
    ) -> Result<LocalReleaseSnapshot, ContribError> {
        let context = self
            .identity
            .album_context(album_id)
            .await
            .ok_or(ContribError::AlbumNotFound)?;
        let album = context.album.clone().ok_or(ContribError::AlbumNotFound)?;
        if !album.active {
            return Err(ContribError::AlbumNotFound);
        }
        // Quirk (v2): only indexed tracks count, and zero indexed tracks
        // reads as "album not found", not "empty album".
        let tracks: Vec<&IdentityTrack> = context
            .tracks
            .iter()
            .filter(|t| t.availability == "indexed")
            .collect();
        if tracks.is_empty() {
            return Err(ContribError::AlbumNotFound);
        }
        let owned: Vec<IdentityTrack> = tracks.into_iter().cloned().collect();
        let (tag, file, policy) = self.identity.input_revisions(&owned);
        let input_revision = format!("{tag}:{file}:{policy}");
        let mut grouped: HashMap<i64, Vec<ReleaseTrackSnapshot>> = HashMap::new();
        let mut medium_titles: HashMap<i64, Option<String>> = HashMap::new();
        for track in &owned {
            let disc_number = track.disc_number.max(1);
            grouped
                .entry(disc_number)
                .or_default()
                .push(ReleaseTrackSnapshot {
                    local_track_id: track.id.clone(),
                    disc_number,
                    track_number: track.track_number,
                    title: track.title.clone(),
                    artist_name: track.artist_name.clone(),
                    duration_seconds: track.duration_seconds,
                    duration_reliable: track.duration_seconds.is_some_and(|d| d > 0.0),
                });
            medium_titles
                .entry(disc_number)
                .or_insert_with(|| track.disc_subtitle.clone());
        }
        let mut positions: Vec<i64> = grouped.keys().copied().collect();
        positions.sort();
        let media = positions
            .into_iter()
            .map(|position| {
                let mut media_tracks = grouped.remove(&position).unwrap_or_default();
                media_tracks.sort_by(|a, b| {
                    a.track_number
                        .cmp(&b.track_number)
                        .then_with(|| a.local_track_id.cmp(&b.local_track_id))
                });
                ReleaseMediumSnapshot {
                    position,
                    title: medium_titles.get(&position).cloned().flatten(),
                    tracks: media_tracks,
                }
            })
            .collect();
        Ok(LocalReleaseSnapshot {
            schema_version: 1,
            local_album_id: album.id.clone(),
            local_artist_id: album.album_artist_id.clone(),
            album_row_revision: album.row_revision,
            input_revision,
            title: album.title.clone(),
            album_artist_name: album.album_artist_name.clone(),
            artist_kind: context.artist.kind.clone(),
            musicbrainz_artist_id: context.artist.provider_artist_id.clone(),
            musicbrainz_release_group_id: context.identity.release_group_mbid.clone(),
            musicbrainz_release_id: context.identity.release_mbid.clone(),
            release_date: album.original_release_date.clone(),
            year: album.year,
            is_compilation: album.is_compilation,
            captured_at: self.now(),
            media,
        })
    }

    /// Assemble the presentation record (v2 `_record`): schema guard,
    /// expiry/redaction, lazy Discogs fetch, validation, next actions.
    async fn record(
        &self,
        row: ContributionRow,
        discogs_override: Option<DiscogsRelease>,
    ) -> Result<ContributionRecord, ContribError> {
        if row.local_snapshot.schema_version != 1
            || row.draft.schema_version != 1
            || row.source_selection.schema_version != 1
        {
            return Err(ContribError::Data(
                "Unsupported persisted contribution document.".into(),
            ));
        }
        let discogs_ref = row
            .source_selection
            .sources
            .iter()
            .find(|s| s.provider == "discogs" && s.entity_type == "release")
            .cloned();
        let expires_at = row.provider_snapshot_expires_at;
        let discogs_expired = discogs_ref.is_some() && expires_at.is_none_or(|e| e <= self.now());
        let mut draft = row.draft.clone();
        if discogs_expired {
            draft = redact_expired_discogs(&draft);
        }
        let mut discogs_release = if discogs_expired {
            None
        } else {
            discogs_override
        };
        if discogs_ref.is_some()
            && !discogs_expired
            && discogs_release.is_none()
            && let (Some(source), Some(discogs)) = (discogs_ref.as_ref(), self.discogs.as_ref())
        {
            discogs_release = discogs
                .get_release(&source.external_id, RequestPriority::UserInitiated)
                .await?;
        }
        let issues = validate_draft(&draft, &row.local_snapshot);
        let input_is_current = !row_is_stale(&row);
        let actions = next_actions(
            row.state,
            !issues.is_empty(),
            discogs_ref.is_some(),
            discogs_expired,
            row.duplicate_result.as_ref(),
            row.result_release_mbid.is_some(),
            row.album_active,
        );
        Ok(ContributionRecord {
            id: row.id.clone(),
            local_album_id: row.local_album_id.clone(),
            created_by_user_id: row.created_by_user_id.clone(),
            updated_by_user_id: row.updated_by_user_id.clone(),
            state: row.state,
            album_row_revision: row.album_row_revision,
            input_revision: row.input_revision.clone(),
            local_snapshot: row.local_snapshot.clone(),
            draft,
            source_selection: row.source_selection.clone(),
            provider_snapshot_expires_at: row.provider_snapshot_expires_at,
            discogs_source: discogs_ref.map(|_| DiscogsSourceView {
                release: discogs_release,
                expired: discogs_expired,
                expires_at,
            }),
            duplicate_result: row.duplicate_result.clone(),
            duplicate_checked_at: row.duplicate_checked_at,
            result_release_mbid: row.result_release_mbid.clone(),
            result_source: row.result_source.clone(),
            result_received_at: row.result_received_at,
            seeded_at: row.seeded_at,
            terminal_at: row.terminal_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
            row_revision: row.row_revision,
            input_is_current,
            validation: issues,
            next_actions: actions,
        })
    }
}

pub fn row_is_stale(row: &ContributionRow) -> bool {
    !row.album_active
        || row.current_input_revision != row.input_revision
        || row.current_album_row_revision != row.album_row_revision
}

fn map_draft_error(error: DraftError) -> ContribError {
    // Quirk (v2): the "refresh the Discogs source" failure surfaces as a
    // provider-expired error, everything else as a validation error.
    if error.0 == "Refresh the Discogs source before using its values." {
        ContribError::ProviderExpired(error.0)
    } else {
        ContribError::Validation(error.0)
    }
}

fn provider_failure_to_contrib(failure: ProviderFailure) -> ContribError {
    match failure {
        ProviderFailure::Unavailable { .. } => ContribError::ProviderUnavailable,
        ProviderFailure::Unmappable => ContribError::ProviderUnmappable,
    }
}

fn validate_public_base_url(base_url: &str) -> Result<(), ContribError> {
    let invalid = ContribError::validation("The public DroppedNeedle URL is not valid.");
    let rest = base_url
        .strip_prefix("https://")
        .or_else(|| base_url.strip_prefix("http://"))
        .ok_or_else(|| invalid.clone())?;
    let authority = rest.split('/').next().unwrap_or("");
    let authority = authority.split(['?', '#']).next().unwrap_or("");
    // Quirk (v2): userinfo is rejected but an explicit port is FINE.
    if authority.is_empty() || authority.contains('@') {
        return Err(invalid);
    }
    let host = authority.split(':').next().unwrap_or("");
    if host.trim().is_empty() {
        return Err(invalid);
    }
    Ok(())
}

fn callback_token() -> String {
    use base64::Engine as _;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_url_rejects_userinfo_and_garbage() {
        assert!(validate_public_base_url("https://music.example.com").is_ok());
        assert!(validate_public_base_url("http://localhost:8080").is_ok());
        assert!(validate_public_base_url("https://user@host.example").is_err());
        assert!(validate_public_base_url("ftp://host.example").is_err());
        assert!(validate_public_base_url("not a url").is_err());
    }
}
