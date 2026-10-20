//! The scripted outside world for contribution tests: Discogs,
//! MusicBrainz and the release editor.
//!
//! `FakeReleaseEditor` is the mocked submission endpoint: the seed form is
//! POSTed to it instead of MusicBrainz, so tests never perform a live
//! provider write.

use std::collections::HashMap;
use std::sync::Mutex;

use droppedneedle_providers::RequestPriority;
use futures_util::future::BoxFuture;

use super::error::ContribError;
use super::models::*;
use super::seams::*;

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
