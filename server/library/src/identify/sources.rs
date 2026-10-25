//! Where identification gets releases and fingerprints.
//!
//! [`ReleaseSource`] is the MusicBrainz side: release search, a release
//! with its full tracklist, and the canonical id of a merged recording or
//! artist.
//! Production binds it to the shared MusicBrainz client (which honors the
//! source setting, official or BrainzMash, and its rate limits); every
//! call runs identity-critical, so a dead provider defers the job rather
//! than letting it guess. [`FingerprintSource`] is the AcoustID side:
//! print the files on the blocking pool and look the prints up in
//! batches, with the API key read from the settings on every call.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::models::LocalTrackFacts;
use super::sqlite::SqliteIdentifyStore;
use crate::matching::model::{
    ArtistAlias, ArtistName, ArtistNames, RELATIONS_VERSION, RecordingRelations, ReleaseRelations,
    RoleCredit, WorkCredit,
};
use crate::matching::model::{VARIOUS_ARTISTS_MBID, credit_text};
use crate::matching::{CreditedArtist, Genre, Release, ReleaseMedium, ReleaseTrack};
use crate::scan::coordinator::{ResolverSource as _, SharedResolver};
use crate::scan::pool::BlockingPool;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary;
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::acoustid::{AcoustIdClient, BatchQuery};
use droppedneedle_providers::musicbrainz::models::Relation;
use droppedneedle_providers::musicbrainz::{
    ArtistCreditName, Criticality, LiveSource, MbRelease, MusicBrainzClient, MusicBrainzView,
    RecordingSearchHit, ReleaseSearchHit, build_recording_query, build_release_match_query,
    build_release_words_query, hit_score, is_valid_mbid,
};
use droppedneedle_providers::{
    CacheKey, Fetched, PayloadKind, Policy, ProviderCacheStore, ProviderError,
    Source as CacheSource,
};

/// The provider could not answer; the job waits and retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceError(pub String);

/// What MusicBrainz says about one artist id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtistAnswer {
    /// The id names an artist MusicBrainz still has.
    Current,
    /// MusicBrainz merged the id into this one (lowercased).
    MergedInto(String),
    /// No artist has the id.
    Missing,
    /// MusicBrainz refused the id, or answered in a shape the client
    /// refuses. The reason is for the log.
    Refused(String),
}

/// The source could not answer about an artist id, so nothing is
/// learned: it was down, rate limited or not set up, or it cannot tell.
#[derive(Debug, Clone, PartialEq)]
pub struct Unanswered {
    pub error: SourceError,
    /// How long the service asked to be left alone, when it said.
    pub retry_after_secs: Option<f64>,
}

impl Unanswered {
    /// A source that has no way to look artists up.
    pub fn cannot_tell() -> Self {
        Self {
            error: SourceError("this source cannot tell".to_owned()),
            retry_after_secs: None,
        }
    }
}

/// One release search hit, before its tracklist is fetched.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseHit {
    pub id: String,
    pub release_group_id: String,
    pub score: i64,
    /// Total tracks over all media, when the index knows every count.
    pub track_count: Option<u32>,
    /// Release title; blank when the index gave none.
    pub title: String,
}

/// A curator's release search: one page of editions for a title and
/// (unless blank) an artist, or every release of one release group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditionQuery {
    pub title: String,
    pub artist: String,
    /// List this release group's releases instead of searching by name.
    pub release_group_mbid: Option<String>,
    pub limit: u32,
    pub offset: u32,
}

/// One MusicBrainz release as the edition finder lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Edition {
    pub release_mbid: String,
    pub release_group_mbid: String,
    pub artist_name: String,
    pub title: String,
    pub date: Option<String>,
    pub country: Option<String>,
    pub status: Option<String>,
    pub packaging: Option<String>,
    pub media_formats: Vec<String>,
    pub disc_count: u32,
    pub track_count: u32,
    pub label: Option<String>,
    pub catalogue_number: Option<String>,
    pub barcode: Option<String>,
    pub disambiguation: Option<String>,
    pub score: i64,
}

/// One page of editions plus the index's total.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditionPage {
    pub items: Vec<Edition>,
    pub total: u64,
    pub offset: u64,
}

pub trait ReleaseSource: Send + Sync {
    /// Releases matching an album title and (unless blank) an artist.
    fn search<'a>(
        &'a self,
        title: &'a str,
        artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>>;

    /// Releases for identifying a local album. Sources with a search index
    /// rank releases with the local track count first, and Various Artists
    /// releases first for a compilation; the default is [`Self::search`].
    fn search_album<'a>(
        &'a self,
        query: AlbumQuery<'a>,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        if query.words {
            return Box::pin(async { Ok(Vec::new()) });
        }
        self.search(query.title, query.artist)
    }

    /// Releases that carry a recording of this title by this artist,
    /// for when no album search finds anything. A source without a
    /// recording index finds none.
    fn search_recording<'a>(
        &'a self,
        _title: &'a str,
        _artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// One release with its tracklist; `None` when MusicBrainz has no
    /// such release. A redirected (merged) id comes back as the release
    /// it now names, with the requested id in [`Release::old_ids`].
    fn release<'a>(&'a self, mbid: &'a str) -> BoxFuture<'a, Result<Option<Release>, SourceError>>;

    /// The id a recording MBID resolves to after merges.
    fn canonical_recording<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>>;

    /// One page of releases for a curator choosing an exact edition. A
    /// source without a search index answers with an empty page.
    fn search_editions<'a>(
        &'a self,
        _query: &'a EditionQuery,
    ) -> BoxFuture<'a, Result<EditionPage, SourceError>> {
        Box::pin(async { Ok(EditionPage::default()) })
    }

    /// [`group_editions`] for the group the files name; `None` when
    /// MusicBrainz will not list this id. Only an outage is an error.
    fn named_group_editions<'a>(
        &'a self,
        group: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<Edition>>, SourceError>> {
        Box::pin(async move { group_editions(self, group).await.map(Some) })
    }

    /// The release group id `mbid` is kept under after merges; `None`
    /// when MusicBrainz has no such group or will not answer for this id.
    /// Only an outage is an error. The default knows no merges.
    fn canonical_release_group<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        Box::pin(async move { Ok(Some(mbid.trim().to_ascii_lowercase())) })
    }

    /// What MusicBrainz says about artist id `mbid`: current, merged into
    /// another id, missing, or refused. The default cannot tell.
    fn canonical_artist<'a>(
        &'a self,
        _mbid: &'a str,
    ) -> BoxFuture<'a, Result<ArtistAnswer, Unanswered>> {
        Box::pin(async { Err(Unanswered::cannot_tell()) })
    }

    /// [`Self::release`], telling an answer that will never do (refused)
    /// from no answer this time. The default counts every failure as no
    /// answer.
    fn release_answer<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<ReleaseAnswer, Unanswered>> {
        Box::pin(async move {
            match self.release(mbid).await {
                Ok(Some(release)) => Ok(ReleaseAnswer::Found(Box::new(release))),
                Ok(None) => Ok(ReleaseAnswer::Missing),
                Err(error) => Err(Unanswered {
                    error,
                    retry_after_secs: None,
                }),
            }
        })
    }
}

/// What MusicBrainz says about one release id.
#[derive(Debug, Clone, PartialEq)]
pub enum ReleaseAnswer {
    /// The release, under the id it now has.
    Found(Box<Release>),
    /// MusicBrainz has no such release.
    Missing,
    /// An answer that asking again will not change: the id is refused,
    /// the redirect is not followed, or the document does not read.
    Refused(String),
}

/// Releases per page, and pages at most, when listing a group's editions.
const GROUP_PAGE: u32 = 100;
const GROUP_PAGES: u32 = 3;

/// Every edition of release group `group`, from the release index: pages
/// of [`GROUP_PAGE`] until an empty page or the index's total, at most
/// [`GROUP_PAGES`]. A larger group is listed in part, with one log line.
/// The source's own priority applies; nothing here throttles.
pub async fn group_editions(
    source: &(impl ReleaseSource + ?Sized),
    group: &str,
) -> Result<Vec<Edition>, SourceError> {
    list_pages(group, |offset| async move {
        source
            .search_editions(&EditionQuery {
                title: String::new(),
                artist: String::new(),
                release_group_mbid: Some(group.to_owned()),
                limit: GROUP_PAGE,
                offset,
            })
            .await
    })
    .await
}

/// [`group_editions`]'s paging over `page`, which answers the page at an
/// offset.
async fn list_pages<E, F, Fut>(group: &str, mut page: F) -> Result<Vec<Edition>, E>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<EditionPage, E>>,
{
    let mut editions = Vec::new();
    let mut total = 0;
    for number in 0..GROUP_PAGES {
        let found = page(number * GROUP_PAGE).await?;
        total = found.total;
        let got = found.items.len();
        editions.extend(found.items);
        if got == 0 || editions.len() as u64 >= total {
            break;
        }
    }
    if (editions.len() as u64) < total {
        tracing::info!(
            release_group = group,
            listed = editions.len(),
            total,
            "release group has more editions than are listed"
        );
    }
    Ok(editions)
}

/// Search hits that name their release group, with their track totals.
fn release_hits(items: Vec<ReleaseSearchHit>) -> Vec<ReleaseHit> {
    items
        .into_iter()
        .filter_map(|hit| {
            let group = hit.release_group.as_ref()?.id.clone();
            let counts: Option<Vec<u32>> =
                hit.media.iter().map(|medium| medium.track_count).collect();
            Some(ReleaseHit {
                score: hit_score(hit.score, hit.ext_score),
                track_count: counts
                    .filter(|counts| !counts.is_empty())
                    .map(|counts| counts.iter().sum()),
                id: hit.id.to_ascii_lowercase(),
                release_group_id: group.to_ascii_lowercase(),
                title: hit.title.unwrap_or_default(),
            })
        })
        .collect()
}

/// Each release a recording search lists, once, with the score of the
/// best recording that carries it and the release's total track count.
fn recording_release_hits(items: Vec<RecordingSearchHit>) -> Vec<ReleaseHit> {
    let mut hits: Vec<ReleaseHit> = Vec::new();
    for recording in items {
        let score = hit_score(recording.score, recording.ext_score);
        for release in recording.releases {
            let Some(group) = release.release_group else {
                continue;
            };
            let id = release.id.to_ascii_lowercase();
            if hits.iter().any(|hit| hit.id == id) {
                continue;
            }
            hits.push(ReleaseHit {
                id,
                release_group_id: group.id.to_ascii_lowercase(),
                score,
                track_count: release.track_count.filter(|count| *count > 0),
                title: String::new(),
            });
        }
    }
    hits
}

/// What identification searches for: the album title, the artist to
/// require (blank for compilations), and ranking hints.
#[derive(Debug, Clone, Copy)]
pub struct AlbumQuery<'a> {
    /// Album title.
    pub title: &'a str,
    /// Album artist to require; blank searches any artist.
    pub artist: &'a str,
    /// Number of local tracks.
    pub track_count: usize,
    /// The album is credited to Various Artists.
    pub various: bool,
    /// Search the title's words loosely rather than as a phrase. A
    /// source without a search index finds nothing this way.
    pub words: bool,
}

/// Search hits per query.
const SEARCH_LIMIT: u32 = 25;
/// Recordings per recording search; each lists the releases carrying it.
const RECORDING_LIMIT: u32 = 5;
/// Includes for a release fetched for matching and tagging.
const RELEASE_INCLUDES: [&str; 6] = [
    "artist-credits",
    "genres",
    "isrcs",
    "labels",
    "recordings",
    "release-groups",
];

impl ReleaseSource for MusicBrainzView {
    fn search<'a>(
        &'a self,
        title: &'a str,
        artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        Box::pin(async move {
            let page = self
                .search_releases(title, artist, SEARCH_LIMIT, Criticality::IdentityCritical)
                .await
                .map_err(|error| SourceError(error.to_string()))?;
            Ok(release_hits(page.items))
        })
    }

    fn search_album<'a>(
        &'a self,
        query: AlbumQuery<'a>,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        Box::pin(async move {
            let tracks = Some(query.track_count);
            let various = query.various.then_some(VARIOUS_ARTISTS_MBID);
            let lucene = if query.words {
                match build_release_words_query(query.title, query.artist, tracks, various) {
                    Some(lucene) => lucene,
                    None => return Ok(Vec::new()),
                }
            } else {
                build_release_match_query(query.title, query.artist, tracks, various)
            };
            let page = self
                .search_releases_query(&lucene, SEARCH_LIMIT, Criticality::IdentityCritical)
                .await
                .map_err(|error| SourceError(error.to_string()))?;
            Ok(release_hits(page.items))
        })
    }

    fn search_recording<'a>(
        &'a self,
        title: &'a str,
        artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        Box::pin(async move {
            let query = build_recording_query(title, artist);
            let page = self
                .search_recordings_query(&query, RECORDING_LIMIT, Criticality::IdentityCritical)
                .await
                .map_err(|error| SourceError(error.to_string()))?;
            Ok(recording_release_hits(page.items))
        })
    }

    fn release<'a>(&'a self, mbid: &'a str) -> BoxFuture<'a, Result<Option<Release>, SourceError>> {
        Box::pin(async move {
            fetch_release(self, mbid)
                .await
                .map_err(|failure| match failure {
                    ReleaseFailure::Provider(error) => SourceError(error.to_string()),
                    ReleaseFailure::Wire(error) => error,
                })
        })
    }

    /// A 4xx other than 404, a redirect that is not followed, an answer
    /// that does not decode or is too large, and a document that does not
    /// convert are refused: the same id fails the same way next time.
    fn release_answer<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<ReleaseAnswer, Unanswered>> {
        Box::pin(async move {
            match fetch_release(self, mbid).await {
                Ok(Some(release)) => Ok(ReleaseAnswer::Found(Box::new(release))),
                Ok(None) => Ok(ReleaseAnswer::Missing),
                Err(ReleaseFailure::Wire(error)) => Ok(ReleaseAnswer::Refused(error.0)),
                Err(ReleaseFailure::Provider(
                    error @ (ProviderError::BadRequest { .. }
                    | ProviderError::Redirect { .. }
                    | ProviderError::Payload { .. }),
                )) => Ok(ReleaseAnswer::Refused(error.to_string())),
                Err(ReleaseFailure::Provider(error)) => {
                    let retry_after_secs = match &error {
                        ProviderError::RateLimited { retry_after, .. }
                        | ProviderError::Unavailable { retry_after, .. } => {
                            retry_after.map(|wait| wait.as_secs_f64())
                        }
                        _ => None,
                    };
                    Err(Unanswered {
                        error: SourceError(error.to_string()),
                        retry_after_secs,
                    })
                }
            }
        })
    }

    fn canonical_recording<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        Box::pin(async move {
            self.resolve_recording_mbid(mbid, Criticality::IdentityCritical)
                .await
                .map(|found| found.map(|id| id.to_ascii_lowercase()))
                .map_err(|error| SourceError(error.to_string()))
        })
    }

    fn named_group_editions<'a>(
        &'a self,
        group: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<Edition>>, SourceError>> {
        Box::pin(async move {
            let listed = list_pages(group, |offset| async move {
                let page = self
                    .search_release_group_editions(
                        group,
                        GROUP_PAGE,
                        offset,
                        Criticality::IdentityCritical,
                    )
                    .await?;
                Ok::<_, ProviderError>(EditionPage {
                    total: page.count,
                    offset: page.offset,
                    items: page
                        .items
                        .into_iter()
                        .filter_map(edition_from_hit)
                        .collect(),
                })
            })
            .await;
            match listed {
                Ok(editions) => Ok(Some(editions)),
                Err(error) if error.is_retriable() => Err(SourceError(error.to_string())),
                Err(error) => {
                    tracing::info!(
                        release_group = group,
                        %error,
                        "MusicBrainz refused to list a release group; the files' group is set aside"
                    );
                    Ok(None)
                }
            }
        })
    }

    /// Not probed for release groups: no merged release group id was at
    /// hand on 2026-10-22. The lookup follows redirects the same way as
    /// the artist one verified below, which this assumes.
    fn canonical_release_group<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        Box::pin(async move {
            let asked = mbid.trim().to_ascii_lowercase();
            match self
                .lookup_release_group(&asked, &[], Criticality::IdentityCritical)
                .await
            {
                Ok(found) => Ok(found
                    .map(|lookup| lookup.entity.id.trim().to_ascii_lowercase())
                    .filter(|id| is_valid_mbid(id))),
                Err(error) if error.is_retriable() => Err(SourceError(error.to_string())),
                Err(error) => {
                    tracing::info!(
                        release_group = asked,
                        %error,
                        "MusicBrainz refused a release group lookup; the files' group is set aside"
                    );
                    Ok(None)
                }
            }
        })
    }

    /// Verified 2026-10-17 against BrainzMash (`api.brainzmash.cc/ws/2`):
    /// a merged artist id answers 301 with an absolute same-origin
    /// `Location` ending `?fmt=json`, whose 200 names the kept id; an
    /// unknown id answers 404. The official service refused this host.
    fn canonical_artist<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<ArtistAnswer, Unanswered>> {
        Box::pin(async move {
            let asked = mbid.trim().to_ascii_lowercase();
            let unanswered = |error: ProviderError, retry_after_secs| Unanswered {
                error: SourceError(error.to_string()),
                retry_after_secs,
            };
            match self
                .lookup_artist(&asked, &[], Criticality::IdentityCritical)
                .await
            {
                Ok(Some(lookup)) => {
                    let id = lookup.entity.id.trim().to_ascii_lowercase();
                    Ok(if id == asked {
                        ArtistAnswer::Current
                    } else if is_valid_mbid(&id) {
                        ArtistAnswer::MergedInto(id)
                    } else {
                        ArtistAnswer::Refused(format!("the answer names {id:?}"))
                    })
                }
                Ok(None) => Ok(ArtistAnswer::Missing),
                // A 400 is MusicBrainz's "Invalid mbid." answer. A request
                // refused before sending or an answer too large to read
                // says nothing about the id, so the check stays pending.
                Err(
                    error @ (ProviderError::BadRequest { status: 400, .. }
                    | ProviderError::Redirect { .. }
                    | ProviderError::Payload {
                        kind: PayloadKind::Decode | PayloadKind::Contract,
                        ..
                    }),
                ) => Ok(ArtistAnswer::Refused(error.to_string())),
                Err(error) => {
                    let retry_after = match &error {
                        ProviderError::RateLimited { retry_after, .. }
                        | ProviderError::Unavailable { retry_after, .. } => {
                            retry_after.map(|wait| wait.as_secs_f64())
                        }
                        _ => None,
                    };
                    Err(unanswered(error, retry_after))
                }
            }
        })
    }

    fn search_editions<'a>(
        &'a self,
        query: &'a EditionQuery,
    ) -> BoxFuture<'a, Result<EditionPage, SourceError>> {
        Box::pin(async move {
            // A dead index is an error the curator sees, not an empty list.
            let page = match query.release_group_mbid.as_deref() {
                Some(group) => {
                    self.search_release_group_editions(
                        group,
                        query.limit,
                        query.offset,
                        Criticality::IdentityCritical,
                    )
                    .await
                }
                None => {
                    self.search_release_editions(
                        &query.title,
                        &query.artist,
                        query.limit,
                        query.offset,
                        Criticality::IdentityCritical,
                    )
                    .await
                }
            }
            .map_err(|error| SourceError(error.to_string()))?;
            Ok(EditionPage {
                total: page.count,
                offset: page.offset,
                items: page
                    .items
                    .into_iter()
                    .filter_map(edition_from_hit)
                    .collect(),
            })
        })
    }
}

/// A release source for each kind of caller: background work, and a
/// person waiting on the answer, who goes ahead of it at the limiter.
#[derive(Clone)]
pub struct Prioritised<R> {
    background: R,
    waiting: R,
}

impl<R> Prioritised<R> {
    pub fn new(background: R, waiting: R) -> Self {
        Self {
            background,
            waiting,
        }
    }

    /// The source for a caller at `priority`: a person waiting
    /// (`UserInitiated`) gets the waiting source, every other priority the
    /// background one.
    pub fn at(&self, priority: RequestPriority) -> &R {
        if priority == RequestPriority::UserInitiated {
            &self.waiting
        } else {
            &self.background
        }
    }
}

/// MusicBrainz for identification and imports: the live source at one
/// priority, with the id each retired recording id was merged into kept
/// in the provider cache. An album's files can carry one retired id per
/// track, and every pass over the album asks the same ones again. Sources
/// at different priorities share one cache: priority does not change the
/// answer.
#[derive(Clone)]
pub struct CachedMusicBrainz {
    view: MusicBrainzView,
    source: LiveSource,
    cache: ProviderCacheStore,
    priority: RequestPriority,
}

impl CachedMusicBrainz {
    /// Reads against the source the settings name, every request at
    /// `priority`, merges kept in `cache`.
    pub fn new(
        client: &MusicBrainzClient,
        source: LiveSource,
        cache: ProviderCacheStore,
        priority: RequestPriority,
    ) -> Self {
        Self {
            view: client.live(source.clone(), priority),
            source,
            cache,
            priority,
        }
    }
}

impl ReleaseSource for CachedMusicBrainz {
    fn search<'a>(
        &'a self,
        title: &'a str,
        artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        ReleaseSource::search(&self.view, title, artist)
    }

    fn search_album<'a>(
        &'a self,
        query: AlbumQuery<'a>,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        self.view.search_album(query)
    }

    fn search_recording<'a>(
        &'a self,
        title: &'a str,
        artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        self.view.search_recording(title, artist)
    }

    fn release<'a>(&'a self, mbid: &'a str) -> BoxFuture<'a, Result<Option<Release>, SourceError>> {
        ReleaseSource::release(&self.view, mbid)
    }

    /// The key carries the source, its generation and the asked id, so a
    /// source switch asks again. A lookup that failed stores nothing; "no
    /// such recording" is kept briefly.
    fn canonical_recording<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        Box::pin(async move {
            let asked = mbid.trim().to_ascii_lowercase();
            let namespace = self.source.cache_namespace();
            let Some(namespace) = namespace.filter(|_| is_valid_mbid(&asked)) else {
                return self.view.canonical_recording(&asked).await;
            };
            let key = CacheKey::new(
                CacheSource::MusicBrainz,
                &[&namespace, "recording_merge", &asked],
            );
            let view = self.view.clone();
            self.cache
                .get_or_fetch(
                    &key,
                    Policy::MERGED_ID,
                    self.priority,
                    move |_| async move {
                        Ok(match view.canonical_recording(&asked).await? {
                            Some(canonical) => Fetched::Found(canonical),
                            None => Fetched::Missing,
                        })
                    },
                )
                .await
        })
    }

    fn search_editions<'a>(
        &'a self,
        query: &'a EditionQuery,
    ) -> BoxFuture<'a, Result<EditionPage, SourceError>> {
        ReleaseSource::search_editions(&self.view, query)
    }

    fn named_group_editions<'a>(
        &'a self,
        group: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<Edition>>, SourceError>> {
        self.view.named_group_editions(group)
    }

    fn canonical_release_group<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>> {
        self.view.canonical_release_group(mbid)
    }

    fn canonical_artist<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<ArtistAnswer, Unanswered>> {
        self.view.canonical_artist(mbid)
    }

    fn release_answer<'a>(
        &'a self,
        mbid: &'a str,
    ) -> BoxFuture<'a, Result<ReleaseAnswer, Unanswered>> {
        self.view.release_answer(mbid)
    }
}

/// An edition-finder row from a release search hit; hits without a
/// release group are skipped, as v2 did.
pub fn edition_from_hit(hit: ReleaseSearchHit) -> Option<Edition> {
    let release_group_mbid = hit.release_group.as_ref()?.id.to_ascii_lowercase();
    let label = hit.label_info.first();
    let mut media_formats: Vec<String> = Vec::new();
    for format in hit.media.iter().filter_map(|medium| medium.format.clone()) {
        if !media_formats.contains(&format) {
            media_formats.push(format);
        }
    }
    let blank_none = |value: Option<String>| value.filter(|value| !value.trim().is_empty());
    Some(Edition {
        score: hit_score(hit.score, hit.ext_score),
        artist_name: credit_text(&credits(&hit.artist_credit)),
        title: hit.title.clone().unwrap_or_default(),
        date: blank_none(hit.date.clone()),
        country: blank_none(hit.country.clone()),
        status: blank_none(hit.status.clone()),
        packaging: blank_none(hit.packaging.clone()),
        disc_count: hit.media.len() as u32,
        track_count: hit
            .media
            .iter()
            .filter_map(|medium| medium.track_count)
            .sum(),
        media_formats,
        label: blank_none(label.and_then(|info| info.label.as_ref()?.name.clone())),
        catalogue_number: blank_none(label.and_then(|info| info.catalog_number.clone())),
        barcode: blank_none(hit.barcode.clone()),
        disambiguation: blank_none(hit.disambiguation.clone()),
        release_mbid: hit.id.to_ascii_lowercase(),
        release_group_mbid,
    })
}

/// The matcher's release from the MusicBrainz wire shape. A release
/// without its group, or a track without its recording, breaks the
/// provider contract for this include set.
/// Why one release lookup gave no document.
enum ReleaseFailure {
    /// MusicBrainz did not answer, or refused.
    Provider(ProviderError),
    /// The answer did not convert to a release.
    Wire(SourceError),
}

/// Release `mbid` with its tracklist; `None` when MusicBrainz has no such
/// release. A redirected (merged) id comes back as the release it now
/// names, with the requested id among its old ids.
async fn fetch_release(
    view: &MusicBrainzView,
    mbid: &str,
) -> Result<Option<Release>, ReleaseFailure> {
    let found = view
        .lookup_release(mbid, &RELEASE_INCLUDES, Criticality::IdentityCritical)
        .await
        .map_err(ReleaseFailure::Provider)?;
    let Some(lookup) = found else {
        return Ok(None);
    };
    let mut old_ids: Vec<String> = lookup
        .redirects
        .iter()
        .filter(|hop| hop.entity == "release")
        .map(|hop| hop.from_mbid.to_ascii_lowercase())
        .collect();
    let requested = mbid.trim().to_ascii_lowercase();
    if !requested.eq_ignore_ascii_case(&lookup.entity.id) && !old_ids.contains(&requested) {
        old_ids.push(requested);
    }
    release_from_wire(lookup.entity, old_ids)
        .map(Some)
        .map_err(ReleaseFailure::Wire)
}

pub fn release_from_wire(wire: MbRelease, old_ids: Vec<String>) -> Result<Release, SourceError> {
    let group = wire.release_group.ok_or_else(|| {
        SourceError(format!(
            "release {} arrived without its release group",
            wire.id
        ))
    })?;
    let mut media = wire.media;
    media.sort_by_key(|medium| medium.position.unwrap_or(u32::MAX));
    let mut tracks = Vec::new();
    let mut summary = Vec::with_capacity(media.len());
    let mut absolute = 0;
    for (medium_index, medium) in media.into_iter().enumerate() {
        let disc = medium.position.unwrap_or(medium_index as u32 + 1);
        summary.push(ReleaseMedium {
            position: disc,
            format: medium.format.clone(),
            title: medium.title.clone().filter(|title| !title.is_empty()),
            track_count: medium.track_count.unwrap_or(medium.tracks.len() as u32),
        });
        for (track_index, track) in medium.tracks.iter().enumerate() {
            let recording = track.recording.as_ref().ok_or_else(|| {
                SourceError(format!("track {} arrived without its recording", track.id))
            })?;
            absolute += 1;
            tracks.push(ReleaseTrack {
                id: track.id.to_ascii_lowercase(),
                recording_id: recording.id.to_ascii_lowercase(),
                title: track.display_title().unwrap_or_default().to_owned(),
                artists: credits(track.credit()),
                disc,
                position: track.position.unwrap_or(track_index as u32 + 1),
                absolute_position: absolute,
                length_ms: track.length_ms(),
                isrcs: recording
                    .isrcs
                    .iter()
                    .map(|isrc| isrc.trim().to_ascii_uppercase())
                    .filter(|isrc| !isrc.is_empty())
                    .collect(),
                genres: wire_genres(&recording.genres),
            });
        }
    }
    let labels: Vec<String> = wire
        .label_info
        .iter()
        .filter_map(|info| info.label.as_ref()?.name.clone())
        .filter(|name| !name.is_empty())
        .collect();
    let catalog_numbers: Vec<String> = wire
        .label_info
        .iter()
        .filter_map(|info| info.catalog_number.clone())
        .filter(|number| !number.is_empty())
        .collect();
    Ok(Release {
        id: wire.id.to_ascii_lowercase(),
        release_group_id: group.id.to_ascii_lowercase(),
        title: wire.title.unwrap_or_default(),
        artists: credits(&wire.artist_credit),
        date: wire.date.filter(|date| !date.is_empty()),
        original_date: group.first_release_date.filter(|date| !date.is_empty()),
        country: wire.country.filter(|country| !country.is_empty()),
        status: wire.status,
        barcode: wire.barcode.filter(|barcode| !barcode.is_empty()),
        asin: wire.asin.filter(|asin| !asin.is_empty()),
        primary_type: group.primary_type,
        secondary_types: group.secondary_types,
        labels: dedup(labels),
        catalog_numbers: dedup(catalog_numbers),
        media: summary,
        tracks,
        old_ids,
        disambiguation: wire.disambiguation.filter(|note| !note.trim().is_empty()),
        genres: {
            let mut genres = wire_genres(&wire.genres);
            for genre in wire_genres(&group.genres) {
                if !genres.iter().any(|known| known.name == genre.name) {
                    genres.push(genre);
                }
            }
            genres
        },
        recording_relations: None,
        artist_names: None,
    })
}

/// Includes for the credited artists' own names and aliases, asked for
/// on their own once a profile writes MusicBrainz or translated names.
pub const NAME_INCLUDES: [&str; 3] = ["aliases", "artist-credits", "recordings"];

/// The credited artists' own names and aliases a names lookup gave: the
/// release's credit and each release track's. An artist named twice
/// keeps the entry that came with its aliases.
pub fn names_from_wire(wire: &MbRelease, fetched_at_ms: i64) -> ArtistNames {
    let mut artists: std::collections::BTreeMap<String, ArtistName> =
        std::collections::BTreeMap::new();
    let credits = wire.artist_credit.iter().chain(
        wire.media
            .iter()
            .flat_map(|medium| &medium.tracks)
            .flat_map(|track| track.credit()),
    );
    for entry in credits {
        let artist = &entry.artist;
        let id = artist.id.to_ascii_lowercase();
        if artists
            .get(&id)
            .is_some_and(|known| known.aliases.is_some() || artist.aliases.is_none())
        {
            continue;
        }
        let aliases = artist.aliases.as_ref().map(|aliases| {
            aliases
                .iter()
                .filter(|alias| !alias.name.trim().is_empty())
                .map(|alias| ArtistAlias {
                    name: alias.name.clone(),
                    sort_name: alias
                        .sort_name
                        .clone()
                        .filter(|sort| !sort.trim().is_empty()),
                    locale: alias
                        .locale
                        .clone()
                        .filter(|locale| !locale.trim().is_empty()),
                    primary: alias.primary.unwrap_or(false),
                    type_id: alias.type_id.clone(),
                })
                .collect()
        });
        artists.insert(
            id,
            ArtistName {
                name: artist.name.clone(),
                sort_name: artist.sort_name.clone().filter(|sort| !sort.is_empty()),
                aliases,
            },
        );
    }
    ArtistNames {
        fetched_at_ms,
        artists,
    }
}

/// Includes for a release's recording and work credits, asked for on
/// their own once a profile writes them.
pub const RELATION_INCLUDES: [&str; 5] = [
    "artist-rels",
    "recording-level-rels",
    "recordings",
    "work-level-rels",
    "work-rels",
];

/// The tag role a MusicBrainz artist relationship credits: Picard
/// 2.13.3's `_ARTIST_REL_TYPES` (`picard/mbjson.py:48-73`) plus v2's
/// `performer` and `vocal`. `mastering` and `recording` stay unmapped, as
/// in Picard; other types write no tag. A folded type (librettist,
/// orchestrator, chorus master and the rest) takes the main role it is
/// written under and keeps its own type in [`RoleCredit::relation_type`].
fn credit_role(relation_type: &str) -> Option<&'static str> {
    Some(match relation_type {
        "arranger" | "orchestrator" | "instrument arranger" | "vocal arranger" => "arranger",
        "composer" => "composer",
        "conductor" => "conductor",
        "instrument"
        | "performer"
        | "vocal"
        | "chorus master"
        | "concertmaster"
        | "performing orchestra" => "performer",
        "lyricist" | "librettist" => "lyricist",
        "producer" => "producer",
        "remixer" => "remixer",
        "audio" | "engineer" | "live sound" | "sound" => "engineer",
        "mix" => "mixer",
        "mix-DJ" => "dj_mixer",
        "writer" => "writer",
        "audio director" | "video director" => "director",
        _ => return None,
    })
}

/// The credits on one recording's (or work's) relationships.
fn role_credits(relations: &[Relation]) -> Vec<RoleCredit> {
    relations
        .iter()
        .filter_map(|relation| {
            let role = credit_role(&relation.rel_type)?;
            // A DJ-mix with attributes credits one disc, which Picard
            // reads from release relationships; those are not fetched.
            if role == "dj_mixer" && !relation.attributes.is_empty() {
                return None;
            }
            let artist = relation.artist.as_ref()?;
            let credited = relation
                .target_credit
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty());
            let name = Some(artist.name.trim())
                .filter(|name| !name.is_empty())
                .or(credited)?
                .to_owned();
            Some(RoleCredit {
                role: role.to_owned(),
                relation_type: relation.rel_type.clone(),
                sort_name: artist
                    .sort_name
                    .clone()
                    .filter(|sort| !sort.trim().is_empty()),
                artist_id: artist.id.to_ascii_lowercase(),
                attributes: relation.attributes.clone(),
                credited: credited
                    .filter(|credited| *credited != name)
                    .map(str::to_owned),
                name,
            })
        })
        .collect()
}

/// The credits a relation lookup gave, by release-track id. A recording,
/// or a work it performs, that came without its `relations` list is left
/// out: not fetched, never "no credits".
pub fn relations_from_wire(wire: &MbRelease, fetched_at_ms: i64) -> ReleaseRelations {
    let mut tracks = std::collections::BTreeMap::new();
    for track in wire.media.iter().flat_map(|medium| &medium.tracks) {
        let Some(recording) = track.recording.as_ref() else {
            continue;
        };
        let Some(relations) = recording.relations.as_ref() else {
            continue;
        };
        let mut credits = role_credits(relations);
        let mut works: Vec<WorkCredit> = Vec::new();
        let mut complete = true;
        for relation in relations
            .iter()
            .filter(|relation| relation.rel_type == "performance")
        {
            let Some(work) = relation.work.as_ref() else {
                continue;
            };
            match work.relations.as_ref() {
                Some(work_relations) => credits.extend(role_credits(work_relations)),
                None => complete = false,
            }
            let id = work.id.to_ascii_lowercase();
            if !works.iter().any(|known| known.id == id) {
                works.push(WorkCredit {
                    id,
                    title: work.title.clone().filter(|title| !title.trim().is_empty()),
                });
            }
        }
        if !complete {
            continue;
        }
        tracks.insert(
            track.id.to_ascii_lowercase(),
            RecordingRelations {
                recording_id: recording.id.to_ascii_lowercase(),
                credits,
                works,
            },
        );
    }
    ReleaseRelations {
        fetched_at_ms,
        version: RELATIONS_VERSION,
        tracks,
    }
}

fn wire_genres(genres: &[droppedneedle_providers::musicbrainz::models::MbGenre]) -> Vec<Genre> {
    genres
        .iter()
        .filter(|genre| !genre.name.trim().is_empty())
        .map(|genre| Genre {
            name: genre.name.clone(),
            count: genre.count,
        })
        .collect()
}

fn credits(credit: &[ArtistCreditName]) -> Vec<CreditedArtist> {
    credit
        .iter()
        .map(|entry| CreditedArtist {
            id: entry.artist.id.to_ascii_lowercase(),
            name: if entry.name.is_empty() {
                entry.artist.name.clone()
            } else {
                entry.name.clone()
            },
            sort_name: entry
                .artist
                .sort_name
                .clone()
                .filter(|name| !name.is_empty()),
            join: entry.joinphrase.clone(),
        })
        .collect()
}

fn dedup(values: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(values.len());
    for value in values {
        if !out.contains(&value) {
            out.push(value);
        }
    }
    out
}

/// What AcoustID heard, per local track id.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioMatches {
    pub recordings: HashMap<String, Vec<String>>,
    pub releases: HashMap<String, Vec<String>>,
}

pub trait FingerprintSource: Send + Sync {
    /// Print and look up the given tracks. Fails soft: tracks it could
    /// not print or look up are simply absent.
    fn identify<'a>(&'a self, tracks: &'a [LocalTrackFacts]) -> BoxFuture<'a, AudioMatches>;

    /// Print and look up files outside the library (a download being
    /// imported), keyed by the caller's ids: the recordings heard in each.
    /// Nothing is kept. Fails soft like [`Self::identify`].
    fn identify_files<'a>(
        &'a self,
        _files: &'a [(String, PathBuf)],
    ) -> BoxFuture<'a, HashMap<String, Vec<String>>> {
        Box::pin(async { HashMap::new() })
    }

    /// Like [`Self::identify_files`], but the releases the heard
    /// recordings appear on, for identifying an album nobody named.
    fn identify_file_releases<'a>(
        &'a self,
        _files: &'a [(String, PathBuf)],
    ) -> BoxFuture<'a, HashMap<String, Vec<String>>> {
        Box::pin(async { HashMap::new() })
    }
}

/// No fingerprinting (tests, and wiring without a pool).
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFingerprints;

impl FingerprintSource for NoFingerprints {
    fn identify<'a>(&'a self, _tracks: &'a [LocalTrackFacts]) -> BoxFuture<'a, AudioMatches> {
        Box::pin(async { AudioMatches::default() })
    }
}

/// Chromaprint on the blocking pool plus batched AcoustID lookups, at
/// background priority: identification and imports are background work.
/// Prints are kept per track and file revision, so a file is decoded once.
pub struct AcoustIdFingerprints {
    client: AcoustIdClient,
    library: Live<TypedLibrary>,
    roots: SharedResolver,
    pool: BlockingPool,
    store: Arc<SqliteIdentifyStore>,
}

impl AcoustIdFingerprints {
    /// Fingerprints with the AcoustID key in `library` and the files under
    /// the live `roots`, both read when a job needs them.
    pub fn new(
        client: AcoustIdClient,
        library: Live<TypedLibrary>,
        roots: SharedResolver,
        pool: BlockingPool,
        store: Arc<SqliteIdentifyStore>,
    ) -> Self {
        Self {
            client,
            library,
            roots,
            pool,
            store,
        }
    }

    /// The AcoustID key from the settings, read per job so a change
    /// applies without a restart. Blank means fingerprinting is off.
    fn api_key(&self) -> String {
        match self.library.get() {
            Ok(settings) => settings.acoustid_api_key.expose().trim().to_owned(),
            Err(error) => {
                tracing::warn!(%error, "cannot read the AcoustID key; fingerprints skipped");
                String::new()
            }
        }
    }

    async fn identify_tracks(&self, tracks: &[LocalTrackFacts]) -> AudioMatches {
        let key = self.api_key();
        if key.is_empty() {
            return AudioMatches::default();
        }
        // Prints already taken for these exact files come from the store.
        let wanted: Vec<(String, String)> = tracks
            .iter()
            .map(|track| (track.local_track_id.clone(), track.stat_revision.clone()))
            .collect();
        let store = self.store.clone();
        // A print that cannot be read is logged and the file decoded
        // again: that gives the same print.
        let stored: HashMap<String, (String, u32)> = tokio::task::spawn_blocking(move || {
            wanted
                .into_iter()
                .filter_map(|(id, revision)| match store.fingerprint(&id, &revision) {
                    Ok(print) => Some((id, print?)),
                    Err(error) => {
                        tracing::warn!(%error, track_id = id, "stored fingerprint unreadable");
                        None
                    }
                })
                .collect()
        })
        .await
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "reading stored fingerprints failed");
            HashMap::new()
        });
        let registry = self.roots.resolver().registry().clone();
        let jobs: Vec<(String, PathBuf)> = tracks
            .iter()
            .filter(|track| !stored.contains_key(&track.local_track_id))
            .filter_map(|track| {
                let root = registry.resolve(&track.root_id)?;
                Some((
                    track.local_track_id.clone(),
                    root.path.join(&track.relative_path),
                ))
            })
            .collect();
        let mut prints: Vec<(String, String, u32)> = stored
            .into_iter()
            .map(|(id, (print, seconds))| (id, print, seconds))
            .collect();
        prints.extend(self.print(jobs).await);
        let queries: Vec<BatchQuery<'_>> = prints
            .iter()
            .map(|(_, print, seconds)| BatchQuery {
                fingerprint: print,
                duration_secs: u64::from(*seconds),
            })
            .collect();
        let found = self
            .client
            .lookup_batch(&key, &queries, RequestPriority::BackgroundSync)
            .await;
        let mut matches = AudioMatches::default();
        for (index, heard) in &found {
            let Some((id, _, _)) = prints.get(*index) else {
                continue;
            };
            if !heard.recording_ids.is_empty() {
                matches
                    .recordings
                    .insert(id.clone(), heard.recording_ids.clone());
            }
            if !heard.release_ids.is_empty() {
                matches
                    .releases
                    .insert(id.clone(), heard.release_ids.clone());
            }
        }
        // Keep every print the lookup answered for, matched or not.
        let revisions: HashMap<String, String> = tracks
            .iter()
            .map(|track| (track.local_track_id.clone(), track.stat_revision.clone()))
            .collect();
        let keep: Vec<(String, String, String, u32, bool)> = found
            .keys()
            .filter_map(|index| {
                let (id, print, seconds) = prints.get(*index)?;
                let revision = revisions.get(id)?.clone();
                let matched = matches.recordings.contains_key(id);
                Some((id.clone(), revision, print.clone(), *seconds, matched))
            })
            .collect();
        let store = self.store.clone();
        let kept = tokio::task::spawn_blocking(move || {
            for (id, revision, print, seconds, matched) in keep {
                if let Err(error) = store.save_fingerprint(&id, &revision, &print, seconds, matched)
                {
                    tracing::warn!(%error, track_id = id, "fingerprint not kept");
                }
            }
        })
        .await;
        if let Err(error) = kept {
            tracing::warn!(%error, "keeping fingerprints failed");
        }
        matches
    }
}

impl AcoustIdFingerprints {
    /// Chromaprint each file on the blocking pool: (id, print, seconds).
    async fn print(&self, jobs: Vec<(String, PathBuf)>) -> Vec<(String, String, u32)> {
        let printed = futures_util::future::join_all(jobs.into_iter().map(|(id, path)| {
            let pool = self.pool.clone();
            async move {
                let result = pool
                    .run(move || crate::tags::generate_fingerprint(&path))
                    .await;
                (id, result)
            }
        }))
        .await;
        let mut prints = Vec::new();
        for (id, result) in printed {
            match result {
                Ok(Ok(print)) if print.duration_seconds > 0 => {
                    prints.push((id, print.fingerprint, print.duration_seconds));
                }
                Ok(Ok(_)) => tracing::debug!(track = id, "zero-length audio not fingerprinted"),
                Ok(Err(error)) => tracing::info!(track = id, %error, "fingerprint failed"),
                Err(error) => tracing::warn!(track = id, %error, "fingerprint job failed"),
            }
        }
        prints
    }

    async fn identify_paths(&self, files: &[(String, PathBuf)]) -> HashMap<String, Vec<String>> {
        self.heard_in_paths(files)
            .await
            .into_iter()
            .filter_map(|(id, heard)| {
                (!heard.recording_ids.is_empty()).then_some((id, heard.recording_ids))
            })
            .collect()
    }

    /// Print files outside the library and look them up, by caller id.
    async fn heard_in_paths(
        &self,
        files: &[(String, PathBuf)],
    ) -> HashMap<String, droppedneedle_providers::acoustid::BatchMatch> {
        let key = self.api_key();
        if key.is_empty() || files.is_empty() {
            return HashMap::new();
        }
        let prints = self.print(files.to_vec()).await;
        let queries: Vec<BatchQuery<'_>> = prints
            .iter()
            .map(|(_, print, seconds)| BatchQuery {
                fingerprint: print,
                duration_secs: u64::from(*seconds),
            })
            .collect();
        let found = self
            .client
            .lookup_batch(&key, &queries, RequestPriority::BackgroundSync)
            .await;
        found
            .into_iter()
            .filter_map(|(index, heard)| {
                let (id, _, _) = prints.get(index)?;
                Some((id.clone(), heard))
            })
            .collect()
    }
}

impl FingerprintSource for AcoustIdFingerprints {
    fn identify<'a>(&'a self, tracks: &'a [LocalTrackFacts]) -> BoxFuture<'a, AudioMatches> {
        Box::pin(self.identify_tracks(tracks))
    }

    fn identify_files<'a>(
        &'a self,
        files: &'a [(String, PathBuf)],
    ) -> BoxFuture<'a, HashMap<String, Vec<String>>> {
        Box::pin(self.identify_paths(files))
    }

    fn identify_file_releases<'a>(
        &'a self,
        files: &'a [(String, PathBuf)],
    ) -> BoxFuture<'a, HashMap<String, Vec<String>>> {
        Box::pin(async move {
            self.heard_in_paths(files)
                .await
                .into_iter()
                .filter_map(|(id, heard)| {
                    (!heard.release_ids.is_empty()).then_some((id, heard.release_ids))
                })
                .collect()
        })
    }
}
