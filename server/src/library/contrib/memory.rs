//! In-memory store plus scripted fakes for contribution tests.
//!
//! `MemoryStore` implements [`ContributionStore`] with v2's guards and state
//! transitions (messages verbatim); the scripted providers/identity/evidence
//! stand in for the identity, evidence and provider adapters. `FakeReleaseEditor` is
//! the mocked submission endpoint: the seed form is POSTed to it instead of
//! MusicBrainz, so tests never perform a live provider write.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::providers::slots::RequestPriority;
use futures_util::future::BoxFuture;

use super::error::ContribError;
use super::models::*;
use super::seams::*;

// ---------------------------------------------------------------------------
// Clock
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct TestClock {
    now: Mutex<f64>,
}

#[cfg(any(test, feature = "test-support"))]
impl TestClock {
    pub fn new(now: f64) -> Self {
        Self {
            now: Mutex::new(now),
        }
    }

    pub fn set(&self, now: f64) {
        *self
            .now
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = now;
    }

    pub fn advance(&self, seconds: f64) {
        *self
            .now
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) += seconds;
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ContributionClock for TestClock {
    fn now_seconds(&self) -> f64 {
        *self
            .now
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

// ---------------------------------------------------------------------------
// Fakes for the identity, evidence and catalog seams
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemoryIdentity {
    contexts: Mutex<HashMap<String, AlbumIdentificationContext>>,
    revisions: Mutex<(String, String, String)>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryIdentity {
    pub fn new() -> Self {
        Self {
            contexts: Mutex::new(HashMap::new()),
            revisions: Mutex::new((
                "tag-rev".to_string(),
                "file-rev".to_string(),
                "policy-rev".to_string(),
            )),
        }
    }

    pub fn insert(&self, album_id: &str, context: AlbumIdentificationContext) {
        self.contexts
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(album_id.to_string(), context);
    }

    pub fn set_revisions(&self, tag: &str, file: &str, policy: &str) {
        *self
            .revisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            (tag.to_string(), file.to_string(), policy.to_string());
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ContributionIdentity for MemoryIdentity {
    fn album_context<'a>(
        &'a self,
        album_id: &'a str,
    ) -> BoxFuture<'a, Option<AlbumIdentificationContext>> {
        Box::pin(async move {
            self.contexts
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(album_id)
                .cloned()
        })
    }

    fn input_revisions(&self, _tracks: &[IdentityTrack]) -> (String, String, String) {
        self.revisions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct MemoryCatalog {
    identity_scopes: Mutex<Vec<(Vec<String>, Vec<String>)>>,
    identification_sweeps: Mutex<usize>,
    identified: Mutex<Vec<(String, String)>>,
}

#[cfg(any(test, feature = "test-support"))]
impl MemoryCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scopes(&self) -> Vec<(Vec<String>, Vec<String>)> {
        self.identity_scopes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn sweeps(&self) -> usize {
        *self
            .identification_sweeps
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn identified_calls(&self) -> Vec<(String, String)> {
        self.identified
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl ContributionCatalog for MemoryCatalog {
    fn invalidate_identity_scope<'a>(
        &'a self,
        album_mbids: &'a [String],
        artist_mbids: &'a [String],
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.identity_scopes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((album_mbids.to_vec(), artist_mbids.to_vec()));
        })
    }

    fn invalidate_identification<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            *self
                .identification_sweeps
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
        })
    }

    fn after_identified<'a>(
        &'a self,
        local_album_id: &'a str,
        input_policy_revision: &'a str,
    ) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            self.identified
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push((
                    local_album_id.to_string(),
                    input_policy_revision.to_string(),
                ));
        })
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug)]
pub struct ScriptedEvidence {
    decision: Mutex<AttachmentDecision>,
    calls: Mutex<usize>,
    matcher_version: String,
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedEvidence {
    pub fn new(decision: AttachmentDecision) -> Self {
        Self {
            decision: Mutex::new(decision),
            calls: Mutex::new(0),
            matcher_version: "test-1".into(),
        }
    }

    pub fn identified(
        release_group_mbid: &str,
        release_mbid: &str,
        artist_mbid: Option<&str>,
    ) -> AttachmentDecision {
        let candidate = AttachmentCandidate {
            release_group_mbid: release_group_mbid.to_string(),
            release_mbid: Some(release_mbid.to_string()),
            artist_mbid: artist_mbid.map(str::to_string),
        };
        AttachmentDecision {
            outcome: AttachmentOutcome::Identified,
            reason_code: Some("EXACT".to_string()),
            selected_candidate_key: Some(candidate.key()),
            candidates: vec![candidate],
        }
    }

    pub fn needs_review(reason: &str) -> AttachmentDecision {
        AttachmentDecision {
            outcome: AttachmentOutcome::NeedsReview,
            reason_code: Some(reason.to_string()),
            selected_candidate_key: None,
            candidates: Vec::new(),
        }
    }

    pub fn calls(&self) -> usize {
        *self
            .calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl AttachmentEvidence for ScriptedEvidence {
    fn matcher_version(&self) -> String {
        self.matcher_version.clone()
    }

    fn decide_attachment<'a>(
        &'a self,
        _contribution: &'a ContributionRecord,
        _verified: &'a MusicBrainzVerifiedRelease,
        _recording_mbids: &'a HashMap<String, Option<String>>,
        _relative_paths: &'a HashMap<String, String>,
    ) -> BoxFuture<'a, AttachmentDecision> {
        Box::pin(async move {
            *self
                .calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) += 1;
            self.decision
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone()
        })
    }
}

// ---------------------------------------------------------------------------
// Provider fakes (reads only - the submission POST goes to FakeReleaseEditor)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderCall {
    pub method: &'static str,
    pub priority: RequestPriority,
    pub bypass_cache: bool,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct ScriptedDiscogs {
    releases: Mutex<HashMap<String, DiscogsRelease>>,
    search_results: Mutex<Vec<DiscogsReleaseCandidate>>,
    calls: Mutex<Vec<ProviderCall>>,
    queries: Mutex<Vec<String>>,
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedDiscogs {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_release(&self, release: DiscogsRelease) {
        self.releases
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(release.release_id.clone(), release);
    }

    pub fn set_search_results(&self, results: Vec<DiscogsReleaseCandidate>) {
        *self
            .search_results
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = results;
    }

    pub fn calls(&self) -> Vec<ProviderCall> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn queries(&self) -> Vec<String> {
        self.queries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl DiscogsContrib for ScriptedDiscogs {
    fn search_releases<'a>(
        &'a self,
        query: &'a str,
        limit: usize,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<DiscogsReleaseCandidate>, ContribError>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(ProviderCall {
                    method: "search_releases",
                    priority,
                    bypass_cache: false,
                });
            self.queries
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(query.to_string());
            Ok(self
                .search_results
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .take(limit)
                .cloned()
                .collect())
        })
    }

    fn get_release<'a>(
        &'a self,
        release_id: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<DiscogsRelease>, ContribError>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(ProviderCall {
                    method: "get_release",
                    priority,
                    bypass_cache: false,
                });
            Ok(self
                .releases
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(release_id)
                .cloned())
        })
    }
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct ScriptedMusicBrainz {
    resolutions: Mutex<HashMap<(String, UrlRelation), MusicBrainzUrlResolution>>,
    verifications:
        Mutex<HashMap<String, Result<Option<MusicBrainzVerifiedRelease>, ProviderFailure>>>,
    duplicates: Mutex<Vec<MusicBrainzVerifiedRelease>>,
    calls: Mutex<Vec<ProviderCall>>,
}

#[cfg(any(test, feature = "test-support"))]
impl ScriptedMusicBrainz {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_resolution(
        &self,
        url: &str,
        relation: UrlRelation,
        resolution: MusicBrainzUrlResolution,
    ) {
        self.resolutions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert((url.to_string(), relation), resolution);
    }

    pub fn insert_verification(
        &self,
        release_mbid: &str,
        result: Result<Option<MusicBrainzVerifiedRelease>, ProviderFailure>,
    ) {
        self.verifications
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(release_mbid.to_string(), result);
    }

    pub fn set_duplicates(&self, releases: Vec<MusicBrainzVerifiedRelease>) {
        *self
            .duplicates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = releases;
    }

    pub fn calls(&self) -> Vec<ProviderCall> {
        self.calls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub fn verify_calls(&self) -> Vec<ProviderCall> {
        self.calls()
            .into_iter()
            .filter(|c| c.method == "get_release_for_verification")
            .collect()
    }
}

#[cfg(any(test, feature = "test-support"))]
impl MusicBrainzContrib for ScriptedMusicBrainz {
    fn resolve_url<'a>(
        &'a self,
        url: &'a str,
        relation: UrlRelation,
        priority: RequestPriority,
        bypass_cache: bool,
    ) -> BoxFuture<'a, Result<MusicBrainzUrlResolution, ContribError>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(ProviderCall {
                    method: "resolve_url",
                    priority,
                    bypass_cache,
                });
            Ok(self
                .resolutions
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&(url.to_string(), relation))
                .cloned()
                .unwrap_or(MusicBrainzUrlResolution {
                    resource_url: url.to_string(),
                    ..Default::default()
                }))
        })
    }

    fn get_release_for_verification<'a>(
        &'a self,
        release_mbid: &'a str,
        priority: RequestPriority,
        bypass_cache: bool,
    ) -> BoxFuture<'a, Result<Option<MusicBrainzVerifiedRelease>, ProviderFailure>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(ProviderCall {
                    method: "get_release_for_verification",
                    priority,
                    bypass_cache,
                });
            self.verifications
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(release_mbid)
                .cloned()
                .unwrap_or(Ok(None))
        })
    }

    fn search_duplicate_releases<'a>(
        &'a self,
        _facts: &'a DuplicateSearchFacts,
        limit: usize,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<MusicBrainzVerifiedRelease>, ContribError>> {
        Box::pin(async move {
            self.calls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(ProviderCall {
                    method: "search_duplicate_releases",
                    priority,
                    bypass_cache: false,
                });
            Ok(self
                .duplicates
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .iter()
                .take(limit)
                .cloned()
                .collect())
        })
    }
}

// ---------------------------------------------------------------------------
// Mocked submission endpoint (replaces the live MusicBrainz release editor)
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone)]
pub struct FakeSubmission {
    pub fields: Vec<(String, String)>,
    pub redirect_uri: String,
    pub callback_token: String,
    pub release_mbid: String,
}

#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct FakeReleaseEditor {
    submissions: Mutex<Vec<FakeSubmission>>,
    next_release_mbid: Mutex<Option<String>>,
}

#[cfg(any(test, feature = "test-support"))]
impl FakeReleaseEditor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Script the MBID the fake editor "creates" for the next submission.
    pub fn script_next_mbid(&self, mbid: &str) {
        *self
            .next_release_mbid
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(mbid.to_string());
    }

    /// Accept the seed form exactly as the release editor would (action, POST
    /// method, required fields) and mint a release MBID. No network involved.
    pub fn submit(&self, seed: &MusicBrainzSeed) -> Result<FakeSubmission, String> {
        if seed.action_url != MUSICBRAINZ_RELEASE_EDITOR {
            return Err(format!("unexpected action URL: {}", seed.action_url));
        }
        if seed.method != "POST" {
            return Err(format!("unexpected method: {}", seed.method));
        }
        let field = |name: &str| {
            seed.fields
                .iter()
                .find(|f| f.name == name)
                .map(|f| f.value.clone())
        };
        let name = field("name").ok_or_else(|| "missing name field".to_string())?;
        if name.trim().is_empty() {
            return Err("empty release name".to_string());
        }
        field("edit_note").ok_or_else(|| "missing edit_note field".to_string())?;
        let redirect_uri =
            field("redirect_uri").ok_or_else(|| "missing redirect_uri field".to_string())?;
        let token = redirect_uri
            .split("token=")
            .nth(1)
            .ok_or_else(|| "redirect_uri has no token".to_string())?
            .to_string();
        let release_mbid = self
            .next_release_mbid
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let submission = FakeSubmission {
            fields: seed
                .fields
                .iter()
                .map(|f| (f.name.clone(), f.value.clone()))
                .collect(),
            redirect_uri,
            callback_token: token,
            release_mbid,
        };
        self.submissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(submission.clone());
        Ok(submission)
    }

    pub fn submissions(&self) -> Vec<FakeSubmission> {
        self.submissions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

// ---------------------------------------------------------------------------
// Memory store
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct AlbumFreshness {
    active: bool,
    input_revision: String,
    album_row_revision: i64,
    artist_id: String,
}

#[derive(Debug, Clone)]
struct CallbackToken {
    contribution_id: String,
    requested_by_user_id: Option<String>,
    expires_at: f64,
    consumed_at: Option<f64>,
}

#[derive(Debug, Default)]
struct Inner {
    contributions: HashMap<String, ContributionRow>,
    tokens: HashMap<String, CallbackToken>,
    jobs: HashMap<String, VerificationJobRow>,
    attempts: Vec<ContributionVerificationAttempt>,
    freshness: HashMap<String, AlbumFreshness>,
    album_identities: HashMap<String, (String, String)>,
    artist_identities: HashMap<String, String>,
    merge_candidates: Vec<(String, String, String)>,
}

#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<Inner>,
}

impl MemoryStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register the live album row the freshness join reads.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_freshness(
        &self,
        album_id: &str,
        active: bool,
        input_revision: &str,
        album_row_revision: i64,
        artist_id: &str,
    ) {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .freshness
            .insert(
                album_id.to_string(),
                AlbumFreshness {
                    active,
                    input_revision: input_revision.to_string(),
                    album_row_revision,
                    artist_id: artist_id.to_string(),
                },
            );
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn raw(&self, contribution_id: &str) -> Option<ContributionRow> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .contributions
            .get(contribution_id)
            .cloned()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn jobs_for(&self, contribution_id: &str) -> Vec<VerificationJobRow> {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut jobs: Vec<VerificationJobRow> = inner
            .jobs
            .values()
            .filter(|j| j.contribution_id == contribution_id)
            .cloned()
            .collect();
        jobs.sort_by(|a, b| a.created_at.total_cmp(&b.created_at));
        jobs
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn attempts(&self) -> Vec<ContributionVerificationAttempt> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .attempts
            .clone()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn live_tokens(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .tokens
            .values()
            .filter(|t| t.consumed_at.is_none())
            .count()
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn album_identity(&self, album_id: &str) -> Option<(String, String)> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .album_identities
            .get(album_id)
            .cloned()
    }

    fn refresh_locked(inner: &Inner, row: &mut ContributionRow) {
        if let Some(fresh) = inner.freshness.get(&row.local_album_id) {
            row.album_active = fresh.active;
            row.current_input_revision = fresh.input_revision.clone();
            row.current_album_row_revision = fresh.album_row_revision;
        } else {
            // Unknown albums read as active + current so bare-bones tests
            // need no registration; staleness tests register explicitly.
            row.album_active = true;
            row.current_input_revision = row.input_revision.clone();
            row.current_album_row_revision = row.album_row_revision;
        }
    }

    fn cancel_jobs_locked(inner: &mut Inner, contribution_id: &str, now: f64) {
        for job in inner.jobs.values_mut() {
            if job.contribution_id == contribution_id
                && matches!(
                    job.state,
                    VerificationJobState::Queued | VerificationJobState::Running
                )
            {
                job.state = VerificationJobState::Cancelled;
                job.terminal_at = Some(now);
                job.worker_id = None;
                job.lease_expires_at = None;
                job.row_revision += 1;
            }
        }
    }

    fn consume_tokens_locked(inner: &mut Inner, contribution_id: &str, now: f64) {
        for token in inner.tokens.values_mut() {
            if token.contribution_id == contribution_id && token.consumed_at.is_none() {
                token.consumed_at = Some(now);
            }
        }
    }

    fn enqueue_job_locked(
        inner: &mut Inner,
        contribution_id: &str,
        requested_by: Option<String>,
        now: f64,
    ) -> String {
        let job_id = uuid::Uuid::new_v4().to_string();
        inner.jobs.insert(
            job_id.clone(),
            VerificationJobRow {
                id: job_id.clone(),
                contribution_id: contribution_id.to_string(),
                state: VerificationJobState::Queued,
                worker_id: None,
                attempt_count: 0,
                not_before: now,
                created_at: now,
                lease_expires_at: None,
                last_failure_code: None,
                requested_by_user_id: requested_by,
                terminal_at: None,
                row_revision: 1,
            },
        );
        job_id
    }
}

fn terminal(state: ContributionState) -> bool {
    matches!(
        state,
        ContributionState::Linked | ContributionState::Cancelled | ContributionState::Stale
    )
}

fn editable(state: ContributionState) -> bool {
    matches!(
        state,
        ContributionState::Draft | ContributionState::Ready | ContributionState::NeedsReview
    )
}

fn stale_revision(msg: &str) -> ContribError {
    ContribError::State(msg.to_string())
}

impl ContributionStore for MemoryStore {
    fn create_or_get<'a>(
        &'a self,
        local_album_id: &'a str,
        actor_user_id: &'a str,
        album_row_revision: i64,
        input_revision: &'a str,
        snapshot: &'a LocalReleaseSnapshot,
        draft: &'a ReleaseDraft,
        selection: &'a ContributionSourceSelection,
        now: f64,
    ) -> BoxFuture<'a, Result<ContributionRow, ContribError>> {
        Box::pin(async move {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(fresh) = inner.freshness.get(local_album_id) {
                if !fresh.active {
                    return Err(ContribError::AlbumNotFound);
                }
                if fresh.album_row_revision != album_row_revision
                    || fresh.input_revision != input_revision
                {
                    return Err(stale_revision(
                        "The album changed before the contribution could be created.",
                    ));
                }
            }
            let existing = inner
                .contributions
                .values()
                .filter(|r| r.local_album_id == local_album_id && !terminal(r.state))
                .max_by(|a, b| a.created_at.total_cmp(&b.created_at))
                .cloned();
            if let Some(mut row) = existing {
                MemoryStore::refresh_locked(&inner, &mut row);
                return Ok(row);
            }
            let mut row = ContributionRow {
                id: uuid::Uuid::new_v4().to_string(),
                local_album_id: local_album_id.to_string(),
                created_by_user_id: Some(actor_user_id.to_string()),
                updated_by_user_id: Some(actor_user_id.to_string()),
                state: ContributionState::Draft,
                album_row_revision,
                input_revision: input_revision.to_string(),
                local_snapshot: snapshot.clone(),
                draft: draft.clone(),
                source_selection: selection.clone(),
                provider_snapshot_expires_at: None,
                discogs_release_id: None,
                discogs_canonical_url: None,
                duplicate_result: None,
                duplicate_checked_at: None,
                duplicate_input_revision: None,
                result_release_mbid: None,
                result_source: None,
                result_received_at: None,
                seeded_at: None,
                seed_token_hash: None,
                seed_token_expires_at: None,
                seed_snapshot_json: None,
                seed_hash: None,
                terminal_at: None,
                created_at: now,
                updated_at: now,
                row_revision: 1,
                album_active: true,
                current_input_revision: input_revision.to_string(),
                current_album_row_revision: album_row_revision,
            };
            MemoryStore::refresh_locked(&inner, &mut row);
            inner.contributions.insert(row.id.clone(), row.clone());
            Ok(row)
        })
    }

    fn get<'a>(&'a self, contribution_id: &'a str) -> BoxFuture<'a, Option<ContributionRow>> {
        Box::pin(async move {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner
                .contributions
                .get(contribution_id)
                .cloned()
                .map(|mut row| {
                    MemoryStore::refresh_locked(&inner, &mut row);
                    row
                })
        })
    }

    fn get_active_for_album<'a>(
        &'a self,
        album_id: &'a str,
    ) -> BoxFuture<'a, Option<ContributionRow>> {
        Box::pin(async move {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut best: Option<ContributionRow> = inner
                .contributions
                .values()
                .filter(|r| r.local_album_id == album_id && !terminal(r.state))
                .max_by(|a, b| a.created_at.total_cmp(&b.created_at))
                .cloned();
            if let Some(row) = best.as_mut() {
                MemoryStore::refresh_locked(&inner, row);
                if !row.album_active {
                    return None;
                }
            }
            best
        })
    }

    fn compare_and_set<'a>(
        &'a self,
        contribution_id: &'a str,
        expected_row_revision: i64,
        actor_user_id: &'a str,
        now: f64,
        update: ContributionUpdate,
    ) -> BoxFuture<'a, Result<ContributionRow, ContribError>> {
        Box::pin(async move {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut row = inner
                .contributions
                .get(contribution_id)
                .cloned()
                .ok_or(ContribError::ContributionNotFound)?;
            MemoryStore::refresh_locked(&inner, &mut row);
            let check_revision = |row: &ContributionRow, msg: &str| {
                if row.row_revision != expected_row_revision {
                    return Err(stale_revision(msg));
                }
                Ok(())
            };
            match update {
                ContributionUpdate::Draft { draft, state } => {
                    if !row.album_active {
                        return Err(ContribError::ContributionNotFound);
                    }
                    if !editable(row.state) {
                        return Err(ContribError::state(
                            "This contribution can no longer be edited.",
                        ));
                    }
                    check_revision(&row, "The contribution changed before this edit was saved.")?;
                    if row.input_revision != row.current_input_revision
                        || row.album_row_revision != row.current_album_row_revision
                    {
                        row.state = ContributionState::Stale;
                        row.terminal_at = Some(now);
                        row.updated_at = now;
                        row.row_revision += 1;
                        inner.contributions.insert(row.id.clone(), row);
                        return Err(stale_revision(
                            "The local album changed. Rebuild the contribution before editing.",
                        ));
                    }
                    row.draft = draft;
                    row.state = state;
                    row.updated_by_user_id = Some(actor_user_id.to_string());
                    row.duplicate_result = None;
                    row.duplicate_checked_at = None;
                    row.duplicate_input_revision = None;
                    row.updated_at = now;
                    row.row_revision += 1;
                }
                ContributionUpdate::SelectDiscogs {
                    release,
                    selection,
                    expires_at,
                } => {
                    if !row.album_active {
                        return Err(ContribError::ContributionNotFound);
                    }
                    if !editable(row.state) {
                        return Err(ContribError::state(
                            "This contribution can no longer be edited.",
                        ));
                    }
                    check_revision(
                        &row,
                        "The contribution changed before the source was selected.",
                    )?;
                    if row.input_revision != row.current_input_revision {
                        return Err(stale_revision(
                            "The local album changed. Rebuild the contribution first.",
                        ));
                    }
                    row.discogs_release_id = Some(release.release_id.clone());
                    row.discogs_canonical_url = Some(release.canonical_release_url.clone());
                    row.source_selection = selection;
                    row.provider_snapshot_expires_at = Some(expires_at);
                    row.updated_by_user_id = Some(actor_user_id.to_string());
                    row.duplicate_result = None;
                    row.duplicate_checked_at = None;
                    row.duplicate_input_revision = None;
                    row.updated_at = now;
                    row.row_revision += 1;
                }
                ContributionUpdate::RemoveDiscogs { draft, state } => {
                    if !row.album_active {
                        return Err(ContribError::ContributionNotFound);
                    }
                    if !editable(row.state) {
                        return Err(ContribError::state(
                            "This contribution can no longer be edited.",
                        ));
                    }
                    check_revision(
                        &row,
                        "The contribution changed before the source was removed.",
                    )?;
                    row.discogs_release_id = None;
                    row.discogs_canonical_url = None;
                    row.source_selection = ContributionSourceSelection::default();
                    row.draft = draft;
                    row.provider_snapshot_expires_at = None;
                    row.state = state;
                    row.updated_by_user_id = Some(actor_user_id.to_string());
                    row.duplicate_result = None;
                    row.duplicate_checked_at = None;
                    row.duplicate_input_revision = None;
                    row.updated_at = now;
                    row.row_revision += 1;
                }
                ContributionUpdate::DuplicateResult { result, state } => {
                    if !row.album_active {
                        return Err(ContribError::ContributionNotFound);
                    }
                    if !matches!(
                        row.state,
                        ContributionState::Ready | ContributionState::NeedsReview
                    ) {
                        return Err(ContribError::state(
                            "Complete the contribution draft first.",
                        ));
                    }
                    check_revision(
                        &row,
                        "The contribution changed before duplicate results were saved.",
                    )?;
                    if row.input_revision != row.current_input_revision
                        || result.input_revision != row.input_revision
                    {
                        return Err(stale_revision(
                            "The local album changed before duplicate results were saved.",
                        ));
                    }
                    row.duplicate_input_revision = Some(result.input_revision.clone());
                    row.duplicate_result = Some(result);
                    row.duplicate_checked_at = Some(now);
                    row.state = state;
                    row.updated_by_user_id = Some(actor_user_id.to_string());
                    row.updated_at = now;
                    row.row_revision += 1;
                }
                ContributionUpdate::AttachExisting {
                    release_mbid,
                    release_group_mbid,
                    artist_mbid,
                    attempt,
                } => {
                    if !row.album_active {
                        return Err(ContribError::ContributionNotFound);
                    }
                    if !matches!(
                        row.state,
                        ContributionState::Ready | ContributionState::NeedsReview
                    ) {
                        return Err(ContribError::state(
                            "This contribution cannot attach a release now.",
                        ));
                    }
                    check_revision(
                        &row,
                        "The contribution changed before the release could be attached.",
                    )?;
                    if row.input_revision != row.current_input_revision
                        || row.duplicate_input_revision.as_deref()
                            != Some(row.input_revision.as_str())
                    {
                        return Err(stale_revision(
                            "The local album changed before the release could be attached.",
                        ));
                    }
                    let in_result = row.duplicate_result.as_ref().is_some_and(|d| {
                        d.candidates
                            .iter()
                            .any(|c| c.release_mbid.as_deref() == Some(release_mbid.as_str()))
                    });
                    if !in_result {
                        return Err(ContribError::state(
                            "The release is not in the current duplicate-check result.",
                        ));
                    }
                    let expected_key = format!("{release_group_mbid}:{release_mbid}");
                    if attempt.selected_candidate_key.as_deref() != Some(expected_key.as_str())
                        || attempt.local_album_id != row.local_album_id
                    {
                        return Err(ContribError::state(
                            "The verified release evidence does not match this contribution.",
                        ));
                    }
                    if let Some((existing_release, existing_group)) =
                        inner.album_identities.get(&row.local_album_id)
                        && (existing_release != &release_mbid
                            || existing_group != &release_group_mbid)
                    {
                        return Err(ContribError::state(
                            "The local album already has a different MusicBrainz identity.",
                        ));
                    }
                    let artist_id = inner
                        .freshness
                        .get(&row.local_album_id)
                        .map(|f| f.artist_id.clone());
                    if let (Some(wanted), Some(artist_id)) =
                        (artist_mbid.clone(), artist_id.clone())
                        && let Some(existing) = inner.artist_identities.get(&artist_id)
                        && existing != &wanted
                    {
                        return Err(ContribError::state(
                            "The local artist already has a different MusicBrainz identity.",
                        ));
                    }
                    let album_id = row.local_album_id.clone();
                    let is_new_identity = !inner.album_identities.contains_key(&album_id);
                    inner.album_identities.insert(
                        album_id.clone(),
                        (release_mbid.clone(), release_group_mbid.clone()),
                    );
                    let mut album_revision = row.current_album_row_revision;
                    if is_new_identity {
                        album_revision += 1;
                        if let Some(fresh) = inner.freshness.get_mut(&album_id) {
                            fresh.album_row_revision = album_revision;
                        }
                    }
                    if let (Some(wanted), Some(artist_id)) = (artist_mbid.clone(), artist_id) {
                        let owner = inner
                            .artist_identities
                            .iter()
                            .find(|(_, mbid)| *mbid == &wanted)
                            .map(|(id, _)| id.clone());
                        match owner {
                            Some(owner_id) if owner_id != artist_id => {
                                let (left, right) = if artist_id < owner_id {
                                    (artist_id.clone(), owner_id)
                                } else {
                                    (owner_id, artist_id.clone())
                                };
                                inner.merge_candidates.push((
                                    left,
                                    right,
                                    "SHARED_PROVIDER_IDENTITY".to_string(),
                                ));
                            }
                            _ => {
                                inner.artist_identities.entry(artist_id).or_insert(wanted);
                            }
                        }
                    }
                    inner.attempts.push(attempt);
                    MemoryStore::consume_tokens_locked(&mut inner, &row.id, now);
                    row.state = ContributionState::Linked;
                    row.album_row_revision = album_revision;
                    row.current_album_row_revision = album_revision;
                    row.result_release_mbid = Some(release_mbid);
                    row.result_source = Some("manual".to_string());
                    row.result_received_at = Some(now);
                    row.terminal_at = Some(now);
                    row.updated_by_user_id = Some(actor_user_id.to_string());
                    row.updated_at = now;
                    row.row_revision += 1;
                }
                ContributionUpdate::PrepareSeed {
                    token_hash,
                    token_expires_at,
                    seed_snapshot_json,
                    seed_hash,
                } => {
                    if !row.album_active {
                        return Err(ContribError::ContributionNotFound);
                    }
                    if !matches!(
                        row.state,
                        ContributionState::Ready | ContributionState::Seeded
                    ) || row.duplicate_result.is_none()
                    {
                        return Err(ContribError::state(
                            "Run the MusicBrainz duplicate check first.",
                        ));
                    }
                    check_revision(
                        &row,
                        "The contribution changed before the editor could be opened.",
                    )?;
                    if row.input_revision != row.current_input_revision
                        || row.duplicate_input_revision.as_deref()
                            != Some(row.input_revision.as_str())
                    {
                        return Err(stale_revision(
                            "The local album changed before the editor could be opened.",
                        ));
                    }
                    let has_exact = row
                        .duplicate_result
                        .as_ref()
                        .is_some_and(|d| d.candidates.iter().any(|c| c.exact));
                    if has_exact {
                        return Err(ContribError::state(
                            "An exact MusicBrainz release already exists for this Discogs source.",
                        ));
                    }
                    MemoryStore::consume_tokens_locked(&mut inner, &row.id, now);
                    inner.tokens.insert(
                        token_hash.clone(),
                        CallbackToken {
                            contribution_id: row.id.clone(),
                            requested_by_user_id: Some(actor_user_id.to_string()),
                            expires_at: token_expires_at,
                            consumed_at: None,
                        },
                    );
                    row.state = ContributionState::Seeded;
                    row.seed_token_hash = Some(token_hash);
                    row.seed_token_expires_at = Some(token_expires_at);
                    row.seed_snapshot_json = Some(seed_snapshot_json);
                    row.seed_hash = Some(seed_hash);
                    row.seeded_at = Some(now);
                    row.updated_by_user_id = Some(actor_user_id.to_string());
                    row.updated_at = now;
                    row.row_revision += 1;
                }
                ContributionUpdate::ManualResult {
                    release_mbid,
                    replace_existing,
                } => {
                    if !matches!(
                        row.state,
                        ContributionState::Seeded
                            | ContributionState::Verifying
                            | ContributionState::NeedsReview
                            | ContributionState::Stale
                    ) {
                        return Err(ContribError::state(
                            "This contribution is not waiting for a MusicBrainz result.",
                        ));
                    }
                    check_revision(
                        &row,
                        "The contribution changed before the result was recorded.",
                    )?;
                    if let Some(current) = row.result_release_mbid.clone()
                        && current != release_mbid
                        && (!replace_existing
                            || !matches!(
                                row.state,
                                ContributionState::NeedsReview | ContributionState::Stale
                            ))
                    {
                        return Err(ContribError::state(
                            "Confirm replacement of the existing MusicBrainz result.",
                        ));
                    }
                    MemoryStore::consume_tokens_locked(&mut inner, &row.id, now);
                    MemoryStore::cancel_jobs_locked(&mut inner, &row.id, now);
                    let next_state = if row.album_active && row.state != ContributionState::Stale {
                        ContributionState::Verifying
                    } else {
                        ContributionState::Stale
                    };
                    row.state = next_state;
                    row.result_release_mbid = Some(release_mbid);
                    row.result_source = Some("manual".to_string());
                    row.result_received_at = Some(now);
                    row.terminal_at = None;
                    row.updated_by_user_id = Some(actor_user_id.to_string());
                    row.updated_at = now;
                    row.row_revision += 1;
                    if next_state == ContributionState::Verifying {
                        let contribution_id = row.id.clone();
                        MemoryStore::enqueue_job_locked(
                            &mut inner,
                            &contribution_id,
                            Some(actor_user_id.to_string()),
                            now,
                        );
                    }
                }
                ContributionUpdate::RequeueVerification => {
                    if !row.album_active {
                        return Err(ContribError::ContributionNotFound);
                    }
                    if !matches!(
                        row.state,
                        ContributionState::Verifying | ContributionState::NeedsReview
                    ) {
                        return Err(ContribError::state(
                            "This contribution cannot be verified now.",
                        ));
                    }
                    if row.result_release_mbid.is_none() {
                        return Err(ContribError::state(
                            "No MusicBrainz result is ready to verify.",
                        ));
                    }
                    check_revision(
                        &row,
                        "The contribution changed before verification was retried.",
                    )?;
                    let has_active = inner.jobs.values().any(|j| {
                        j.contribution_id == row.id
                            && matches!(
                                j.state,
                                VerificationJobState::Queued | VerificationJobState::Running
                            )
                    });
                    if !has_active {
                        let contribution_id = row.id.clone();
                        MemoryStore::enqueue_job_locked(
                            &mut inner,
                            &contribution_id,
                            Some(actor_user_id.to_string()),
                            now,
                        );
                    }
                    row.state = ContributionState::Verifying;
                    row.terminal_at = None;
                    row.updated_by_user_id = Some(actor_user_id.to_string());
                    row.updated_at = now;
                    row.row_revision += 1;
                }
                ContributionUpdate::Cancel => {
                    if !row.album_active {
                        return Err(ContribError::ContributionNotFound);
                    }
                    if matches!(
                        row.state,
                        ContributionState::Linked
                            | ContributionState::Cancelled
                            | ContributionState::Stale
                    ) {
                        return Err(ContribError::state("This contribution is already closed."));
                    }
                    check_revision(
                        &row,
                        "The contribution changed before it could be cancelled.",
                    )?;
                    MemoryStore::consume_tokens_locked(&mut inner, &row.id, now);
                    MemoryStore::cancel_jobs_locked(&mut inner, &row.id, now);
                    row.state = ContributionState::Cancelled;
                    row.terminal_at = Some(now);
                    row.seed_snapshot_json = None;
                    row.updated_by_user_id = Some(actor_user_id.to_string());
                    row.updated_at = now;
                    row.row_revision += 1;
                }
                ContributionUpdate::PurgeProviderData { draft, selection } => {
                    if row.provider_snapshot_expires_at.is_none()
                        || row.row_revision != expected_row_revision
                    {
                        return Err(stale_revision("The contribution changed before cleanup."));
                    }
                    row.draft = draft;
                    row.source_selection = selection;
                    row.provider_snapshot_expires_at = None;
                    row.discogs_release_id = None;
                    row.discogs_canonical_url = None;
                    row.duplicate_result = None;
                    row.duplicate_checked_at = None;
                    row.duplicate_input_revision = None;
                    row.seed_snapshot_json = None;
                    row.updated_at = now;
                    row.row_revision += 1;
                }
            }
            inner.contributions.insert(row.id.clone(), row.clone());
            Ok(row)
        })
    }

    fn rebuild<'a>(
        &'a self,
        contribution_id: &'a str,
        expected_row_revision: i64,
        actor_user_id: &'a str,
        album_row_revision: i64,
        input_revision: &'a str,
        snapshot: &'a LocalReleaseSnapshot,
        draft: &'a ReleaseDraft,
        selection: &'a ContributionSourceSelection,
        now: f64,
    ) -> BoxFuture<'a, Result<ContributionRow, ContribError>> {
        Box::pin(async move {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut old = inner
                .contributions
                .get(contribution_id)
                .cloned()
                .ok_or(ContribError::ContributionNotFound)?;
            if old.row_revision != expected_row_revision {
                return Err(stale_revision(
                    "The contribution changed before it could be rebuilt.",
                ));
            }
            if let Some(fresh) = inner.freshness.get(&old.local_album_id) {
                if !fresh.active {
                    return Err(ContribError::AlbumNotFound);
                }
                if fresh.album_row_revision != album_row_revision
                    || fresh.input_revision != input_revision
                {
                    return Err(stale_revision(
                        "The album changed before the contribution could be rebuilt.",
                    ));
                }
            }
            old.state = ContributionState::Stale;
            old.terminal_at = Some(now);
            old.updated_at = now;
            old.row_revision += 1;
            inner.contributions.insert(old.id.clone(), old.clone());
            MemoryStore::consume_tokens_locked(&mut inner, &old.id, now);
            MemoryStore::cancel_jobs_locked(&mut inner, &old.id, now);
            let mut row = ContributionRow {
                id: uuid::Uuid::new_v4().to_string(),
                local_album_id: old.local_album_id.clone(),
                created_by_user_id: Some(actor_user_id.to_string()),
                updated_by_user_id: Some(actor_user_id.to_string()),
                state: ContributionState::Draft,
                album_row_revision,
                input_revision: input_revision.to_string(),
                local_snapshot: snapshot.clone(),
                draft: draft.clone(),
                source_selection: selection.clone(),
                provider_snapshot_expires_at: None,
                discogs_release_id: None,
                discogs_canonical_url: None,
                duplicate_result: None,
                duplicate_checked_at: None,
                duplicate_input_revision: None,
                result_release_mbid: None,
                result_source: None,
                result_received_at: None,
                seeded_at: None,
                seed_token_hash: None,
                seed_token_expires_at: None,
                seed_snapshot_json: None,
                seed_hash: None,
                terminal_at: None,
                created_at: now,
                updated_at: now,
                row_revision: 1,
                album_active: true,
                current_input_revision: input_revision.to_string(),
                current_album_row_revision: album_row_revision,
            };
            MemoryStore::refresh_locked(&inner, &mut row);
            inner.contributions.insert(row.id.clone(), row.clone());
            Ok(row)
        })
    }

    fn mark_stale<'a>(
        &'a self,
        contribution_id: &'a str,
        expected_row_revision: i64,
        now: f64,
    ) -> BoxFuture<'a, Result<ContributionRow, ContribError>> {
        Box::pin(async move {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut row = inner
                .contributions
                .get(contribution_id)
                .cloned()
                .ok_or(ContribError::ContributionNotFound)?;
            if !terminal(row.state) {
                if row.row_revision != expected_row_revision {
                    return Err(stale_revision(
                        "The contribution changed before it could be marked stale.",
                    ));
                }
                row.state = ContributionState::Stale;
                row.terminal_at = Some(now);
                row.seed_snapshot_json = None;
                row.updated_at = now;
                row.row_revision += 1;
                MemoryStore::cancel_jobs_locked(&mut inner, &row.id, now);
                inner.contributions.insert(row.id.clone(), row.clone());
            }
            MemoryStore::refresh_locked(&inner, &mut row);
            Ok(row)
        })
    }

    fn consume_callback_token<'a>(
        &'a self,
        token_hash: &'a str,
        release_mbid: &'a str,
        now: f64,
    ) -> BoxFuture<'a, Result<CallbackConsumption, ContribError>> {
        Box::pin(async move {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let token = match inner.tokens.get(token_hash) {
                Some(token) if token.consumed_at.is_none() && token.expires_at >= now => {
                    token.clone()
                }
                _ => return Ok(None),
            };
            let mut row = match inner.contributions.get(&token.contribution_id).cloned() {
                Some(row) => row,
                None => return Ok(None),
            };
            MemoryStore::refresh_locked(&inner, &mut row);
            if !matches!(
                row.state,
                ContributionState::Seeded | ContributionState::Verifying | ContributionState::Stale
            ) {
                return Err(ContribError::state(
                    "This contribution is not waiting for MusicBrainz.",
                ));
            }
            if let Some(current) = row.result_release_mbid.clone()
                && current != release_mbid
            {
                return Err(ContribError::state(
                    "This contribution already has a different MusicBrainz result.",
                ));
            }
            if let Some(stored) = inner.tokens.get_mut(token_hash) {
                stored.consumed_at = Some(now);
            }
            MemoryStore::consume_tokens_locked(&mut inner, &row.id, now);
            let next_state = if row.album_active && row.state != ContributionState::Stale {
                ContributionState::Verifying
            } else {
                ContributionState::Stale
            };
            row.state = next_state;
            row.result_release_mbid = Some(release_mbid.to_string());
            row.result_source = Some("callback".to_string());
            row.result_received_at = Some(now);
            row.updated_at = now;
            row.row_revision += 1;
            inner.contributions.insert(row.id.clone(), row.clone());
            if next_state == ContributionState::Stale {
                return Ok(Some((row.id, None)));
            }
            let active = inner
                .jobs
                .values()
                .find(|j| {
                    j.contribution_id == row.id
                        && matches!(
                            j.state,
                            VerificationJobState::Queued | VerificationJobState::Running
                        )
                })
                .map(|j| j.id.clone());
            let job_id = match active {
                Some(id) => id,
                None => {
                    let contribution_id = row.id.clone();
                    MemoryStore::enqueue_job_locked(
                        &mut inner,
                        &contribution_id,
                        token.requested_by_user_id.clone(),
                        now,
                    )
                }
            };
            Ok(Some((row.id, Some(job_id))))
        })
    }

    fn list_for_provider_purge<'a>(
        &'a self,
        now: f64,
        limit: usize,
    ) -> BoxFuture<'a, Vec<ContributionRow>> {
        Box::pin(async move {
            let inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let bounded = limit.clamp(1, 1_000);
            let mut rows: Vec<ContributionRow> = inner
                .contributions
                .values()
                .filter(|r| {
                    r.provider_snapshot_expires_at.is_some()
                        && (r.provider_snapshot_expires_at.is_some_and(|e| e <= now)
                            || terminal(r.state))
                })
                .cloned()
                .collect();
            rows.iter_mut()
                .for_each(|row| MemoryStore::refresh_locked(&inner, row));
            rows.sort_by(|a, b| {
                a.provider_snapshot_expires_at
                    .unwrap_or(f64::INFINITY)
                    .total_cmp(&b.provider_snapshot_expires_at.unwrap_or(f64::INFINITY))
                    .then_with(|| a.id.cmp(&b.id))
            });
            rows.truncate(bounded);
            rows
        })
    }

    fn claim_verification<'a>(
        &'a self,
        worker_id: &'a str,
        now: f64,
        lease_seconds: f64,
    ) -> BoxFuture<'a, Option<VerificationJobRow>> {
        Box::pin(async move {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let id = inner
                .jobs
                .values()
                .filter(|j| j.state == VerificationJobState::Queued && j.not_before <= now)
                .min_by(|a, b| {
                    a.not_before
                        .total_cmp(&b.not_before)
                        .then_with(|| a.created_at.total_cmp(&b.created_at))
                })
                .map(|j| j.id.clone())?;
            let job = inner.jobs.get_mut(&id)?;
            job.state = VerificationJobState::Running;
            job.attempt_count += 1;
            job.worker_id = Some(worker_id.to_string());
            job.lease_expires_at = Some(now + lease_seconds);
            job.row_revision += 1;
            Some(job.clone())
        })
    }

    fn heartbeat_verification<'a>(
        &'a self,
        job_id: &'a str,
        worker_id: &'a str,
        expected_row_revision: i64,
        now: f64,
        lease_seconds: f64,
    ) -> BoxFuture<'a, Result<i64, ContribError>> {
        Box::pin(async move {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let job = inner
                .jobs
                .get_mut(job_id)
                .ok_or_else(|| stale_revision("The contribution verification lease changed."))?;
            if job.state != VerificationJobState::Running
                || job.worker_id.as_deref() != Some(worker_id)
                || job.row_revision != expected_row_revision
            {
                return Err(stale_revision(
                    "The contribution verification lease changed.",
                ));
            }
            job.lease_expires_at = Some(now + lease_seconds);
            job.row_revision += 1;
            Ok(job.row_revision)
        })
    }

    fn retry_verification<'a>(
        &'a self,
        job_id: &'a str,
        worker_id: &'a str,
        expected_row_revision: i64,
        failure_code: &'a str,
        not_before: f64,
        now: f64,
    ) -> BoxFuture<'a, Result<(), ContribError>> {
        Box::pin(async move {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let _ = now;
            let job = inner.jobs.get_mut(job_id).ok_or_else(|| {
                stale_revision("The contribution verification job changed before retry.")
            })?;
            if job.state != VerificationJobState::Running
                || job.worker_id.as_deref() != Some(worker_id)
                || job.row_revision != expected_row_revision
            {
                return Err(stale_revision(
                    "The contribution verification job changed before retry.",
                ));
            }
            job.state = VerificationJobState::Queued;
            job.last_failure_code = Some(failure_code.to_string());
            job.not_before = not_before;
            job.worker_id = None;
            job.lease_expires_at = None;
            job.row_revision += 1;
            Ok(())
        })
    }

    fn finish_verification<'a>(
        &'a self,
        job_id: &'a str,
        worker_id: &'a str,
        expected_job_revision: i64,
        expected_contribution_revision: i64,
        expected_album_revision: i64,
        attempt: &'a ContributionVerificationAttempt,
        outcome: VerificationOutcome,
        failure_code: Option<&'a str>,
        identities: &'a FinishIdentities,
        now: f64,
    ) -> BoxFuture<'a, Result<VerificationOutcome, ContribError>> {
        Box::pin(async move {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let job = inner.jobs.get(job_id).cloned().ok_or_else(|| {
                stale_revision("The contribution verification job changed before completion.")
            })?;
            if job.state != VerificationJobState::Running
                || job.worker_id.as_deref() != Some(worker_id)
                || job.row_revision != expected_job_revision
            {
                return Err(stale_revision(
                    "The contribution verification job changed before completion.",
                ));
            }
            let mut row = inner
                .contributions
                .get(&job.contribution_id)
                .cloned()
                .ok_or(ContribError::ContributionNotFound)?;
            MemoryStore::refresh_locked(&inner, &mut row);
            if !row.album_active
                || row.input_revision != row.current_input_revision
                || row.album_row_revision != row.current_album_row_revision
            {
                row.state = ContributionState::Stale;
                row.terminal_at = Some(now);
                row.updated_at = now;
                row.row_revision += 1;
                inner.contributions.insert(row.id.clone(), row);
                if let Some(stored) = inner.jobs.get_mut(job_id) {
                    stored.state = VerificationJobState::Cancelled;
                    stored.last_failure_code = Some("LOCAL_INPUT_CHANGED".to_string());
                    stored.terminal_at = Some(now);
                    stored.worker_id = None;
                    stored.lease_expires_at = None;
                    stored.row_revision += 1;
                }
                return Ok(VerificationOutcome::Stale);
            }
            if row.row_revision != expected_contribution_revision {
                return Err(stale_revision(
                    "The contribution changed before verification completed.",
                ));
            }
            if row.state != ContributionState::Verifying {
                return Err(ContribError::state(
                    "This contribution is not being verified.",
                ));
            }
            if attempt.local_album_id != row.local_album_id {
                return Err(ContribError::state(
                    "The verification subject does not match.",
                ));
            }
            let live_album_revision = inner
                .freshness
                .get(&row.local_album_id)
                .map(|f| f.album_row_revision)
                .unwrap_or(expected_album_revision);
            if live_album_revision != expected_album_revision {
                return Err(stale_revision(
                    "The album changed before verification completed.",
                ));
            }
            let (final_outcome, final_failure) = match outcome {
                VerificationOutcome::Linked => {
                    let selected_ok = attempt.selected_candidate_key.is_some()
                        && identities.release_mbid.is_some()
                        && identities.release_group_mbid.is_some();
                    if !selected_ok {
                        (
                            VerificationOutcome::NeedsReview,
                            Some("VERIFICATION_EVIDENCE_MISSING".to_string()),
                        )
                    } else if identities.release_mbid.as_deref()
                        != row.result_release_mbid.as_deref()
                    {
                        (
                            VerificationOutcome::NeedsReview,
                            Some(FAILURE_RETURNED_RELEASE_MISMATCH.to_string()),
                        )
                    } else if let Some((existing_release, existing_group)) =
                        inner.album_identities.get(&row.local_album_id)
                    {
                        if Some(existing_release) != identities.release_mbid.as_ref()
                            || Some(existing_group) != identities.release_group_mbid.as_ref()
                        {
                            (
                                VerificationOutcome::NeedsReview,
                                Some("EXISTING_IDENTITY_CONFLICT".to_string()),
                            )
                        } else {
                            (VerificationOutcome::Linked, None)
                        }
                    } else if let Some(wanted) = identities.artist_mbid.as_deref() {
                        let artist_id = inner
                            .freshness
                            .get(&row.local_album_id)
                            .map(|f| f.artist_id.clone());
                        let conflict = artist_id
                            .as_ref()
                            .and_then(|id| inner.artist_identities.get(id))
                            .is_some_and(|existing| existing != wanted);
                        if conflict {
                            (
                                VerificationOutcome::NeedsReview,
                                Some("EXISTING_ARTIST_IDENTITY_CONFLICT".to_string()),
                            )
                        } else {
                            (VerificationOutcome::Linked, None)
                        }
                    } else {
                        (VerificationOutcome::Linked, None)
                    }
                }
                _ => (
                    VerificationOutcome::NeedsReview,
                    failure_code.map(str::to_string),
                ),
            };
            inner.attempts.push(attempt.clone());
            if final_outcome == VerificationOutcome::Linked {
                let album_id = row.local_album_id.clone();
                let release = identities.release_mbid.clone().unwrap_or_default();
                let group = identities.release_group_mbid.clone().unwrap_or_default();
                let is_new = !inner.album_identities.contains_key(&album_id);
                inner
                    .album_identities
                    .insert(album_id.clone(), (release, group));
                let mut album_revision = expected_album_revision;
                if is_new {
                    album_revision += 1;
                    if let Some(fresh) = inner.freshness.get_mut(&album_id) {
                        fresh.album_row_revision = album_revision;
                    }
                }
                if let Some(wanted) = identities.artist_mbid.clone() {
                    let artist_id = inner.freshness.get(&album_id).map(|f| f.artist_id.clone());
                    if let Some(artist_id) = artist_id {
                        let owner = inner
                            .artist_identities
                            .iter()
                            .find(|(_, mbid)| *mbid == &wanted)
                            .map(|(id, _)| id.clone());
                        match owner {
                            Some(owner_id) if owner_id != artist_id => {
                                let (left, right) = if artist_id < owner_id {
                                    (artist_id, owner_id)
                                } else {
                                    (owner_id, artist_id)
                                };
                                inner.merge_candidates.push((
                                    left,
                                    right,
                                    "SHARED_PROVIDER_IDENTITY".to_string(),
                                ));
                            }
                            _ => {
                                inner.artist_identities.entry(artist_id).or_insert(wanted);
                            }
                        }
                    }
                }
                row.state = ContributionState::Linked;
                row.album_row_revision = album_revision;
                row.current_album_row_revision = album_revision;
                row.terminal_at = Some(now);
                if let Some(stored) = inner.jobs.get_mut(job_id) {
                    stored.state = VerificationJobState::Succeeded;
                    stored.last_failure_code = None;
                    stored.terminal_at = Some(now);
                    stored.worker_id = None;
                    stored.lease_expires_at = None;
                    stored.row_revision += 1;
                }
            } else {
                row.state = ContributionState::NeedsReview;
                if let Some(stored) = inner.jobs.get_mut(job_id) {
                    stored.state = VerificationJobState::NeedsReview;
                    stored.last_failure_code = final_failure.clone();
                    stored.terminal_at = Some(now);
                    stored.worker_id = None;
                    stored.lease_expires_at = None;
                    stored.row_revision += 1;
                }
            }
            row.seed_snapshot_json = None;
            row.updated_at = now;
            row.row_revision += 1;
            inner.contributions.insert(row.id.clone(), row);
            Ok(final_outcome)
        })
    }

    fn recover_verification_leases<'a>(&'a self, now: f64) -> BoxFuture<'a, u64> {
        Box::pin(async move {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let mut recovered = 0;
            for job in inner.jobs.values_mut() {
                if job.state == VerificationJobState::Running
                    && job.lease_expires_at.is_some_and(|e| e < now)
                {
                    job.state = VerificationJobState::Queued;
                    job.worker_id = None;
                    job.lease_expires_at = None;
                    job.not_before = now;
                    job.row_revision += 1;
                    recovered += 1;
                }
            }
            recovered
        })
    }

    fn clean_records<'a>(&'a self, now: f64) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            inner.tokens.retain(|_, token| {
                if let Some(consumed) = token.consumed_at {
                    consumed >= now - 30.0 * 86_400.0
                } else {
                    token.expires_at >= now - 7.0 * 86_400.0
                }
            });
            let live: HashMap<String, bool> = inner
                .tokens
                .values()
                .filter(|t| t.consumed_at.is_none() && t.expires_at >= now)
                .map(|t| (t.contribution_id.clone(), true))
                .collect();
            for row in inner.contributions.values_mut() {
                let orphaned_seed = row.seed_snapshot_json.is_some()
                    && (terminal(row.state)
                        || (row.seeded_at.is_some() && !live.contains_key(&row.id)));
                if orphaned_seed {
                    row.seed_snapshot_json = None;
                    row.row_revision += 1;
                }
            }
            inner.contributions.retain(|_, row| {
                !(matches!(
                    row.state,
                    ContributionState::Cancelled | ContributionState::Stale
                ) && row.result_release_mbid.is_none()
                    && row.terminal_at.is_some_and(|t| t < now - 90.0 * 86_400.0))
            });
            let terminal_contribs: HashMap<String, bool> = inner
                .contributions
                .values()
                .filter(|r| terminal(r.state))
                .map(|r| (r.id.clone(), true))
                .collect();
            inner.jobs.retain(|_, job| {
                !(matches!(
                    job.state,
                    VerificationJobState::Succeeded
                        | VerificationJobState::NeedsReview
                        | VerificationJobState::Failed
                        | VerificationJobState::Cancelled
                ) && job.terminal_at.is_some_and(|t| t < now - 90.0 * 86_400.0)
                    && terminal_contribs.contains_key(&job.contribution_id))
            });
        })
    }
}
