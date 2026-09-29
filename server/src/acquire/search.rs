//! Production candidate search and release polling for the flows loops.
//!
//! [`FanoutSearch`] is the single [`CandidateSearch`](super::flows::seams::CandidateSearch)
//! implementation: slskd plus the active Usenet side (native indexers or
//! Prowlarr, per the `usenet_search_backend` section), each isolated so one
//! erroring source never fails the fan-out. [`EmptyPoll`] stands in for the
//! follow-poll provider page until the provider-backed poll lands.

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;

use super::flows::seams::{Candidate, CandidateSearch, ObservedRelease, ReleasePoll};
use super::slskd::{ReqwestSlskdHttp, SlskdRepository};
use super::usenet::newznab::NewznabIndexer;
use super::usenet::prowlarr::ProwlarrIndexer;
use crate::runtime_config::sections::{UsenetBackend, UsenetBackendSetting};

/// Candidates returned per search, at most. The wanted loop only needs to
/// know a candidate exists; the worker re-searches at enqueue time.
const MAX_CANDIDATES: usize = 50;

/// slskd plus Usenet candidate fan-out.
pub struct FanoutSearch {
    slskd: Option<Arc<SlskdRepository<ReqwestSlskdHttp>>>,
    newznab: Arc<NewznabIndexer>,
    prowlarr: Arc<ProwlarrIndexer>,
    backend: UsenetBackend,
    timeout: Duration,
}

impl FanoutSearch {
    /// Wire the fan-out. `backend` selects the Usenet side; unconfigured
    /// clients are skipped, so an empty deployment honestly finds nothing.
    pub fn new(
        slskd: Option<Arc<SlskdRepository<ReqwestSlskdHttp>>>,
        newznab: Arc<NewznabIndexer>,
        prowlarr: Arc<ProwlarrIndexer>,
        backend: UsenetBackendSetting,
        timeout: Duration,
    ) -> Self {
        Self {
            slskd,
            newznab,
            prowlarr,
            backend: backend.0,
            timeout,
        }
    }

    /// Whether any side could answer. Used for honest health detail.
    pub fn any_configured(&self) -> bool {
        let slskd = self.slskd.as_ref().is_some_and(|repo| repo.is_configured());
        let usenet = match self.backend {
            UsenetBackend::Indexers => self.newznab.is_configured(),
            UsenetBackend::Prowlarr => self.prowlarr.is_configured(),
        };
        slskd || usenet
    }
}

impl CandidateSearch for FanoutSearch {
    fn search_album<'a>(
        &'a self,
        artist: &'a str,
        title: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>> {
        Box::pin(async move {
            let mut out = Vec::new();
            if let Some(repo) = &self.slskd
                && repo.is_configured()
            {
                match repo.search_album(artist, title, None).await {
                    Ok(hits) => out.extend(hits.iter().take(MAX_CANDIDATES).map(|hit| {
                        let title = if hit.parent_directory.is_empty() {
                            hit.filename.clone()
                        } else {
                            hit.parent_directory.clone()
                        };
                        Candidate {
                            title,
                            source: "slskd".to_owned(),
                        }
                    })),
                    Err(error) => {
                        tracing::warn!(%error, "acquire candidate search: slskd failed");
                    }
                }
            }
            let usenet = match self.backend {
                UsenetBackend::Indexers => {
                    if self.newznab.is_configured() {
                        self.newznab
                            .search_album(artist, title, None, self.timeout)
                            .await
                    } else {
                        Vec::new()
                    }
                }
                UsenetBackend::Prowlarr => {
                    if self.prowlarr.is_configured() {
                        self.prowlarr
                            .search_album(artist, title, self.timeout)
                            .await
                    } else {
                        Vec::new()
                    }
                }
            };
            out.extend(
                usenet
                    .into_iter()
                    .take(MAX_CANDIDATES)
                    .map(|hit| Candidate {
                        title: hit.usenet.title,
                        source: "usenet".to_owned(),
                    }),
            );
            out.truncate(MAX_CANDIDATES);
            Ok(out)
        })
    }
}

/// Follow-poll provider page that answers empty everywhere. The follow loop
/// still runs its cadence (baselines record, cursors advance) but never
/// observes a release until the MusicBrainz-backed poll lands.
#[derive(Debug, Default)]
pub struct EmptyPoll;

impl EmptyPoll {
    /// Empty poller.
    pub fn new() -> Self {
        Self
    }
}

impl ReleasePoll for EmptyPoll {
    fn poll_releases<'a>(
        &'a self,
        _artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ObservedRelease>, String>> {
        Box::pin(async move { Ok(Vec::new()) })
    }
}
