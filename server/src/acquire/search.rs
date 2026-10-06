//! Production candidate search and release polling for the flows loops.
//!
//! [`FanoutSearch`] is the single [`CandidateSearch`](super::flows::seams::CandidateSearch)
//! implementation: slskd plus the active Usenet side (native indexers or
//! Prowlarr, per the `usenet_search_backend` section), each isolated so one
//! erroring source never fails the fan-out. [`MusicBrainzReleasePoll`] reads a
//! followed artist's release groups for the follow poll.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures_util::future::BoxFuture;

use super::flows::seams::{Candidate, CandidateSearch, ObservedRelease, ReleasePoll};
use super::slskd::{ReqwestSlskdHttp, SlskdRepository};
use super::usenet::newznab::NewznabIndexer;
use super::usenet::prowlarr::ProwlarrIndexer;
use crate::providers::RequestPriority;
use crate::providers::adapters::HealthSink;
use crate::providers::degradation::DegradationSink;
use crate::providers::musicbrainz::{
    Criticality, MbPacing, MbTransport, MusicBrainzClient, ReqwestMbTransport,
};
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
    /// clients are skipped, so an empty deployment finds nothing.
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

    /// Whether any side could answer. Used for health detail.
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

/// Release groups read per page (the MusicBrainz browse maximum).
const RELEASE_PAGE: u32 = 100;
/// Pages read per artist at most: 1,000 release groups covers all but a
/// handful of artists, and bounds one poll's provider work.
const MAX_RELEASE_PAGES: u32 = 10;

/// Slot boot puts the live release poll into (it needs the provider
/// limiter, which exists only after acquisition is built).
pub type ReleasePollSlot = Arc<OnceLock<Arc<dyn ReleasePoll>>>;

/// The follow poll's provider page, read through [`ReleasePollSlot`].
/// Until boot attaches the live poll every poll fails, so no artist is
/// baselined against an empty page.
#[derive(Default)]
pub struct SlotPoll {
    slot: ReleasePollSlot,
}

impl SlotPoll {
    /// Poll over an empty slot.
    pub fn new() -> Self {
        Self::default()
    }

    /// The slot boot fills.
    pub fn slot(&self) -> &ReleasePollSlot {
        &self.slot
    }
}

impl ReleasePoll for SlotPoll {
    fn poll_releases<'a>(
        &'a self,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ObservedRelease>, String>> {
        match self.slot.get() {
            Some(poll) => poll.poll_releases(artist_mbid),
            None => Box::pin(async { Err("MusicBrainz is not attached yet".to_owned()) }),
        }
    }
}

/// The artist's release groups from the MusicBrainz browse endpoint,
/// every page, at background priority on the shared limiter (v2
/// `get_artist_release_groups_with_context`, `BACKGROUND_SYNC`). The
/// browse list is complete, so a release cannot hide behind search
/// ranking. A provider outage is an error, never an empty page.
pub struct MusicBrainzReleasePoll<T: MbTransport, S: DegradationSink> {
    client: MusicBrainzClient<T, S>,
}

impl<T: MbTransport, S: DegradationSink> MusicBrainzReleasePoll<T, S> {
    /// Poll over one client (callers set background priority on it).
    pub fn new(client: MusicBrainzClient<T, S>) -> Self {
        Self { client }
    }
}

impl<T: MbTransport, S: DegradationSink> ReleasePoll for MusicBrainzReleasePoll<T, S> {
    fn poll_releases<'a>(
        &'a self,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ObservedRelease>, String>> {
        Box::pin(async move {
            let mut out = Vec::new();
            for page in 0..MAX_RELEASE_PAGES {
                let offset = page * RELEASE_PAGE;
                let found = self
                    .client
                    .browse_artist_release_groups(
                        artist_mbid,
                        RELEASE_PAGE,
                        offset,
                        Criticality::IdentityCritical,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                let read = found.items.len();
                out.extend(found.items.into_iter().map(|group| ObservedRelease {
                    rg_mbid: group.id,
                    title: group.title.unwrap_or_default(),
                    first_release_date: group.first_release_date,
                    primary_type: group.primary_type,
                    secondary_types: group.secondary_types,
                }));
                if read == 0 || u64::from(offset) + read as u64 >= found.count {
                    return Ok(out);
                }
            }
            tracing::debug!(artist_mbid, "follow poll read the page cap for this artist");
            Ok(out)
        })
    }
}

/// The production poll: background priority on the shared limiter, the
/// configured MusicBrainz source read per request, failures counted
/// toward system health.
pub fn live_release_poll(
    http: &crate::http_client::HttpClientFactory,
    providers: Arc<crate::providers::Providers>,
    source: crate::providers::musicbrainz::SourceFn,
) -> MusicBrainzReleasePoll<ReqwestMbTransport, HealthSink> {
    let health = HealthSink::new(&providers);
    MusicBrainzReleasePoll::new(
        MusicBrainzClient::official(
            ReqwestMbTransport::new(http.no_redirect().clone()),
            MbPacing::new(providers),
        )
        .with_source_fn(source)
        .with_priority(RequestPriority::BackgroundSync)
        .with_sink(health),
    )
}
