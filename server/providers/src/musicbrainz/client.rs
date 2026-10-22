//! The MusicBrainz client: one paced, identified request per call, with
//! redirect proof and criticality routing. Every request carries
//! `fmt=json`, the smallest sorted include set the caller asked for (never
//! a broad default: the full verification include set decoded ~608 KB for
//! one 34-track release, live 2026-07-21), and the descriptive DroppedNeedle
//! User-Agent from the HTTP client factory.
//!
//! [`MusicBrainzClient`] holds one executor per kind of source, built once
//! with the providers. Callers read through a [`MusicBrainzView`], which
//! names the source and the priority every request of the view uses.

use std::time::Duration;

use droppedneedle_kernel::runtime_config::{Live, sections::MusicBrainzSettings};
use serde::de::DeserializeOwned;

use super::models::{
    ArtistSearchHit, MbArtist, MbRecording, MbRelease, MbReleaseGroup, RecordingSearchHit,
    ReleaseGroupBrowsePage, ReleaseGroupSearchHit, ReleaseSearchHit, SearchPage, UrlResolution,
    decode_search_page,
};
use super::redirects::{
    RedirectHop, brainzmash_redirect_path, lookup_redirect_pair, official_redirect_hop,
    validate_brainzmash_path,
};
use super::{
    BRAINZMASH_MAX_COOLDOWN, Criticality, LiveSource, MAX_BODY_BYTES, MAX_PAGE_LIMIT,
    MAX_REDIRECT_HOPS, MbSource, SOURCE, build_release_search_query, escape_lucene_phrase,
    is_valid_mbid, normalize_mb_id,
};
use crate::RequestPriority;
use crate::error::{PayloadKind, ProviderError};
use crate::executor::{
    Answer, Backoff, Executor, Redirects, Reply, Request, Spec, Url, standard_classify,
};

/// Deadline per attempt, the HTTP factory's own.
const TIMEOUT: Duration = Duration::from_secs(30);

/// The official service. Over its 1 request per second rule MusicBrainz
/// declines with HTTP 503 (MusicBrainz_API/Rate_Limiting, read
/// 2026-10-18), so a 503 is a rate limit here; one that names no wait
/// rests the bucket for a second.
pub static OFFICIAL: Spec = Spec {
    source: SOURCE,
    health_key: "musicbrainz",
    timeout: TIMEOUT,
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(1)),
    retry_after_cap: Duration::from_secs(60),
    classify: official_classify,
};

/// BrainzMash. A 429 rests the bucket for the wait it names, or backs off
/// from one second to a minute while 429s keep naming none.
/// A 503 is a dead mirror, not a rate signal, so
/// it sets no cooldown. Failures count toward the mirror's own health
/// entry.
pub static BRAINZMASH: Spec = Spec {
    source: SOURCE,
    health_key: "musicbrainz-brainzmash",
    timeout: TIMEOUT,
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Exponential {
        base: Duration::from_secs(1),
        max: BRAINZMASH_MAX_COOLDOWN,
    },
    retry_after_cap: BRAINZMASH_MAX_COOLDOWN,
    classify: brainzmash_classify,
};

/// A mirror the operator runs, paced at the rate they set.
pub static MIRROR: Spec = Spec {
    source: SOURCE,
    health_key: "musicbrainz",
    timeout: TIMEOUT,
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(1)),
    retry_after_cap: Duration::from_secs(60),
    classify: standard_classify,
};

fn official_classify(answer: &Answer<'_>) -> Result<(), ProviderError> {
    if answer.reply.status() == 503 {
        return Err(ProviderError::RateLimited {
            provider: SOURCE,
            retry_after: answer.retry_after(),
        });
    }
    standard_classify(answer)
}

fn brainzmash_classify(answer: &Answer<'_>) -> Result<(), ProviderError> {
    if answer.reply.status() == 503 {
        return Err(ProviderError::Unavailable {
            provider: SOURCE,
            retry_after: None,
        });
    }
    standard_classify(answer)
}

/// Lookup result: the entity plus the redirect proof behind it.
#[derive(Debug, Clone)]
pub struct Lookup<T> {
    /// Decoded entity (canonical id after any redirects).
    pub entity: T,
    /// Followed hops, oldest first, for the durable canonical map.
    pub redirects: Vec<RedirectHop>,
}

/// The MusicBrainz executors, one per kind of source, and the roots of
/// the two built-in sources. Built once with the providers; cheap to clone.
#[derive(Debug, Clone)]
pub struct MusicBrainzClient {
    official: Executor,
    brainzmash: Executor,
    mirror: Executor,
    official_root: String,
    brainzmash_root: String,
}

impl MusicBrainzClient {
    /// The client over its three executors, with the official and
    /// BrainzMash roots.
    #[must_use]
    pub fn new(
        official: Executor,
        brainzmash: Executor,
        mirror: Executor,
        official_root: &str,
        brainzmash_root: &str,
    ) -> Self {
        Self {
            official,
            brainzmash,
            mirror,
            official_root: official_root.to_owned(),
            brainzmash_root: brainzmash_root.to_owned(),
        }
    }

    /// The source `settings` select.
    #[must_use]
    pub fn source_from(&self, settings: &MusicBrainzSettings) -> MbSource {
        MbSource::from_settings(settings, &self.official_root, &self.brainzmash_root)
    }

    /// The source the saved settings select, read at each request.
    #[must_use]
    pub fn live_source(&self, settings: Live<MusicBrainzSettings>) -> LiveSource {
        LiveSource::new(settings, &self.official_root, &self.brainzmash_root)
    }

    /// Reads against `source`, every request at `priority`.
    #[must_use]
    pub fn fixed(&self, source: MbSource, priority: RequestPriority) -> MusicBrainzView {
        MusicBrainzView {
            client: self.clone(),
            source: Choice::Fixed(source),
            priority,
        }
    }

    /// Reads against the source the settings name at each request, every
    /// request at `priority`.
    #[must_use]
    pub fn live(&self, source: LiveSource, priority: RequestPriority) -> MusicBrainzView {
        MusicBrainzView {
            client: self.clone(),
            source: Choice::Live(source),
            priority,
        }
    }
}

#[derive(Clone)]
enum Choice {
    Fixed(MbSource),
    Live(LiveSource),
}

/// MusicBrainz reads against one source at one priority. Owns status
/// semantics, redirect walking and criticality routing; owns no cache and
/// no persistence.
#[derive(Clone)]
pub struct MusicBrainzView {
    client: MusicBrainzClient,
    source: Choice,
    priority: RequestPriority,
}

impl MusicBrainzView {
    /// The source the next request goes to.
    pub fn source(&self) -> MbSource {
        match &self.source {
            Choice::Fixed(source) => source.clone(),
            Choice::Live(live) => live.current(),
        }
    }

    /// Search artists by free text, the way v2's artist search asked:
    /// name and accent-folded name boosted, aliases next, then any field.
    /// The text is escaped, so quotes and Lucene operators match literally.
    pub async fn search_artists_text(
        &self,
        text: &str,
        limit: u32,
        offset: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ArtistSearchHit>, ProviderError> {
        let escaped = escape_lucene_phrase(text.trim());
        let query = format!(
            r#"artist:"{escaped}"^3 OR artistaccent:"{escaped}"^3 OR alias:"{escaped}"^2 OR {escaped}"#
        );
        self.search_page("artist", &query, limit, offset, "artists", criticality)
            .await
    }

    /// Search release groups by free text, the way v2's album search
    /// asked: group title boosted, release title next, then any field.
    pub async fn search_release_groups_text(
        &self,
        text: &str,
        limit: u32,
        offset: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ReleaseGroupSearchHit>, ProviderError> {
        let escaped = escape_lucene_phrase(text.trim());
        let query = format!(r#"releasegroup:"{escaped}"^3 OR release:"{escaped}"^2 OR {escaped}"#);
        self.search_page(
            "release-group",
            &query,
            limit,
            offset,
            "release-groups",
            criticality,
        )
        .await
    }

    /// One page of an artist's release groups (browse, not search: the
    /// list is complete and stable), with artist credits so "more by"
    /// rows can name the artist. Missing artists browse as empty.
    pub async fn browse_artist_release_groups(
        &self,
        artist_mbid: &str,
        limit: u32,
        offset: u32,
        criticality: Criticality,
    ) -> Result<ReleaseGroupBrowsePage, ProviderError> {
        let params = vec![
            ("artist".to_owned(), normalize_mb_id(artist_mbid)),
            ("inc".to_owned(), "artist-credits".to_owned()),
            ("limit".to_owned(), limit.min(MAX_PAGE_LIMIT).to_string()),
            ("offset".to_owned(), offset.to_string()),
        ];
        match self
            .get(vec!["release-group".to_owned()], params, criticality)
            .await?
        {
            WireOutcome::Found(reply) => reply.json(),
            WireOutcome::Missing | WireOutcome::Degraded => Ok(ReleaseGroupBrowsePage {
                count: 0,
                offset: u64::from(offset),
                items: Vec::new(),
            }),
            WireOutcome::Redirect { status, .. } => Err(refused(status)),
        }
    }

    /// Search releases by title and artist (verified Lucene shape).
    pub async fn search_releases(
        &self,
        title: &str,
        artist: &str,
        limit: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ReleaseSearchHit>, ProviderError> {
        let query = build_release_search_query(title, artist);
        self.search_page("release", &query, limit, 0, "releases", criticality)
            .await
    }

    /// Search releases with a query the caller built and escaped. Library
    /// contributions use it to OR a barcode clause onto title and artist,
    /// as v2's duplicate search did.
    pub async fn search_releases_query(
        &self,
        query: &str,
        limit: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ReleaseSearchHit>, ProviderError> {
        self.search_page("release", query, limit, 0, "releases", criticality)
            .await
    }

    /// Search recordings with a query the caller built and escaped.
    /// Identification uses it to find an album by one of its tracks.
    pub async fn search_recordings_query(
        &self,
        query: &str,
        limit: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<RecordingSearchHit>, ProviderError> {
        self.search_page("recording", query, limit, 0, "recordings", criticality)
            .await
    }

    /// One page of releases by title and artist, for a curator choosing
    /// an exact edition.
    pub async fn search_release_editions(
        &self,
        title: &str,
        artist: &str,
        limit: u32,
        offset: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ReleaseSearchHit>, ProviderError> {
        let query = build_release_search_query(title, artist);
        self.search_page("release", &query, limit, offset, "releases", criticality)
            .await
    }

    /// One page of every release in one release group, for a curator
    /// choosing any edition of an album. `rgid` is MusicBrainz's release
    /// group field in the release index.
    pub async fn search_release_group_editions(
        &self,
        release_group_mbid: &str,
        limit: u32,
        offset: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ReleaseSearchHit>, ProviderError> {
        let query = format!("rgid:{}", normalize_mb_id(release_group_mbid));
        self.search_page("release", &query, limit, offset, "releases", criticality)
            .await
    }

    /// Search release groups carrying one tag,
    /// spelling variants included.
    pub async fn search_release_groups_by_tag(
        &self,
        tag: &str,
        limit: u32,
        offset: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ReleaseGroupSearchHit>, ProviderError> {
        let query = super::build_tag_query(tag);
        self.search_page(
            "release-group",
            &query,
            limit,
            offset,
            "release-groups",
            criticality,
        )
        .await
    }

    /// Search artists carrying one tag,
    /// spelling variants included.
    pub async fn search_artists_by_tag(
        &self,
        tag: &str,
        limit: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ArtistSearchHit>, ProviderError> {
        self.search_artists_by_tag_from(tag, limit, 0, criticality)
            .await
    }

    /// One page of artists carrying one tag, starting at `offset` (the
    /// genre page's "more artists").
    pub async fn search_artists_by_tag_from(
        &self,
        tag: &str,
        limit: u32,
        offset: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ArtistSearchHit>, ProviderError> {
        let query = super::build_tag_query(tag);
        self.search_page("artist", &query, limit, offset, "artists", criticality)
            .await
    }

    /// Check that the source answers an artist search, for the Settings
    /// connection test. Only a success status passes.
    ///
    /// # Errors
    ///
    /// The typed failure of the call; a 404 is [`ProviderError::NotFound`]
    /// and a redirect [`ProviderError::Redirect`].
    pub async fn check_reachable(&self) -> Result<(), ProviderError> {
        let params = vec![
            ("query".to_owned(), "test".to_owned()),
            ("limit".to_owned(), "1".to_owned()),
        ];
        match self
            .get(
                vec!["artist".to_owned()],
                params,
                Criticality::IdentityCritical,
            )
            .await?
        {
            WireOutcome::Found(_) => Ok(()),
            WireOutcome::Missing => Err(ProviderError::NotFound { provider: SOURCE }),
            WireOutcome::Redirect { status, .. } => Err(refused(status)),
            WireOutcome::Degraded => Err(ProviderError::Unavailable {
                provider: SOURCE,
                retry_after: None,
            }),
        }
    }

    /// Search artists by name.
    pub async fn search_artists(
        &self,
        name: &str,
        limit: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ArtistSearchHit>, ProviderError> {
        let query = format!(r#"artist:"{}""#, escape_lucene_phrase(name));
        self.search_page("artist", &query, limit, 0, "artists", criticality)
            .await
    }

    /// Look up one release with the caller's include set.
    pub async fn lookup_release(
        &self,
        mbid: &str,
        includes: &[&str],
        criticality: Criticality,
    ) -> Result<Option<Lookup<MbRelease>>, ProviderError> {
        self.lookup("release", mbid, includes, criticality).await
    }

    /// Look up one release with proof it belongs to a provider-returned
    /// group. Exact-release identification must include `release-groups`
    /// (Clairo _Immunity_ omits the member without it, live 2026-08-10);
    /// absence after that request is a fail-closed contract break, never
    /// an assumed group.
    pub async fn lookup_exact_release(
        &self,
        mbid: &str,
        criticality: Criticality,
    ) -> Result<Option<Lookup<MbRelease>>, ProviderError> {
        let found = self
            .lookup_release(
                mbid,
                &["artist-credits", "recordings", "release-groups"],
                criticality,
            )
            .await?;
        match found {
            Some(lookup) if lookup.entity.release_group.is_none() => Err(ProviderError::Payload {
                provider: SOURCE,
                kind: PayloadKind::Contract,
                message: format!(
                    "exact release {} arrived without its provider release group",
                    lookup.entity.id
                ),
            }),
            other => Ok(other),
        }
    }

    /// Resolve a release MBID to its release-group MBID (tags carry the release
    /// id, the library keys on the group id).
    pub async fn resolve_release_to_release_group(
        &self,
        release_mbid: &str,
        criticality: Criticality,
    ) -> Result<Option<String>, ProviderError> {
        let found = self
            .lookup_release(release_mbid, &["release-groups"], criticality)
            .await?;
        Ok(found.and_then(|lookup| lookup.entity.release_group.map(|group| group.id)))
    }

    /// Look up one release group with the caller's include set.
    pub async fn lookup_release_group(
        &self,
        mbid: &str,
        includes: &[&str],
        criticality: Criticality,
    ) -> Result<Option<Lookup<MbReleaseGroup>>, ProviderError> {
        self.lookup("release-group", mbid, includes, criticality)
            .await
    }

    /// Look up one artist. The detail surface uses
    /// `tags+aliases+url-rels` (v2 artist mixin).
    pub async fn lookup_artist(
        &self,
        mbid: &str,
        includes: &[&str],
        criticality: Criticality,
    ) -> Result<Option<Lookup<MbArtist>>, ProviderError> {
        self.lookup("artist", mbid, includes, criticality).await
    }

    /// Look up one recording. Identity callers pass
    /// `inc=releases+release-groups`: each `releases` item carries
    /// `id`/`status`/`date` and each `release-group` carries
    /// `id`/`title`/`primary-type`/`secondary-types`/`first-release-date`
    /// (live 2026-07-20).
    pub async fn lookup_recording(
        &self,
        mbid: &str,
        includes: &[&str],
        criticality: Criticality,
    ) -> Result<Option<Lookup<MbRecording>>, ProviderError> {
        self.lookup("recording", mbid, includes, criticality).await
    }

    /// Resolve a recording MBID through merge redirects to its canonical
    /// id. A retired MBID counts as equivalent only after this lookup
    /// proves the redirect target; any other outcome keeps the normal
    /// conflict gate (live 2026-08-10, merged recording identifiers).
    pub async fn resolve_recording_mbid(
        &self,
        recording_mbid: &str,
        criticality: Criticality,
    ) -> Result<Option<String>, ProviderError> {
        let normalized = normalize_mb_id(recording_mbid);
        if !is_valid_mbid(&normalized) {
            return Err(ProviderError::Invalid {
                provider: SOURCE,
                reason: "not a MusicBrainz id",
            });
        }
        let found = self.lookup_recording(&normalized, &[], criticality).await?;
        Ok(found.map(|lookup| lookup.entity.id))
    }

    /// Resolve a resource URL to its relations. Callers pass the fixed
    /// numeric URL form, never a pasted or provider slug (a slugged Discogs
    /// URL 404s, live 2026-07-21). A missing URL resolves to an empty
    /// relation list, and multi-target responses stay ambiguous.
    pub async fn resolve_url(
        &self,
        resource: &str,
        includes: &[&str],
        criticality: Criticality,
    ) -> Result<UrlResolution, ProviderError> {
        let mut params = vec![("resource".to_owned(), resource.to_owned())];
        let inc = sorted_includes(includes);
        if !inc.is_empty() {
            params.push(("inc".to_owned(), inc));
        }
        match self
            .get(vec!["url".to_owned()], params, criticality)
            .await?
        {
            WireOutcome::Found(reply) => reply.json(),
            WireOutcome::Missing | WireOutcome::Degraded => Ok(UrlResolution::empty(resource)),
            WireOutcome::Redirect { status, .. } => Err(refused(status)),
        }
    }

    /// One entity search with bucket isolation: provider death on a
    /// non-critical search records a degradation and yields an empty page
    /// (v2 grouped search returns `[]` plus a failed bucket).
    async fn search_page<E>(
        &self,
        entity: &str,
        query: &str,
        limit: u32,
        offset: u32,
        array_key: &str,
        criticality: Criticality,
    ) -> Result<SearchPage<E>, ProviderError>
    where
        E: DeserializeOwned,
    {
        let mut params = vec![
            ("query".to_owned(), query.to_owned()),
            ("limit".to_owned(), limit.min(MAX_PAGE_LIMIT).to_string()),
        ];
        if offset > 0 {
            params.push(("offset".to_owned(), offset.to_string()));
        }
        match self
            .get(vec![entity.to_owned()], params, criticality)
            .await?
        {
            WireOutcome::Found(reply) => decode_search_page(reply.bytes(), array_key),
            WireOutcome::Missing | WireOutcome::Degraded => Ok(SearchPage {
                count: 0,
                offset: 0,
                items: Vec::new(),
            }),
            WireOutcome::Redirect { status, .. } => Err(refused(status)),
        }
    }

    /// One entity lookup with redirect proof and criticality routing.
    async fn lookup<E>(
        &self,
        entity: &str,
        mbid: &str,
        includes: &[&str],
        criticality: Criticality,
    ) -> Result<Option<Lookup<E>>, ProviderError>
    where
        E: DeserializeOwned,
    {
        let mut params = Vec::new();
        let inc = sorted_includes(includes);
        if !inc.is_empty() {
            params.push(("inc".to_owned(), inc));
        }
        let mut segments = vec![entity.to_owned(), normalize_mb_id(mbid)];
        let mut redirects = Vec::new();
        let mut last_status = 0;
        for _ in 0..=MAX_REDIRECT_HOPS {
            match self.get(segments, params.clone(), criticality).await? {
                WireOutcome::Found(reply) => {
                    let entity = reply.json()?;
                    return Ok(Some(Lookup { entity, redirects }));
                }
                WireOutcome::Missing | WireOutcome::Degraded => return Ok(None),
                WireOutcome::Redirect {
                    hop,
                    next_path,
                    status,
                } => {
                    redirects.push(hop);
                    segments = path_segments(&next_path);
                    last_status = status;
                }
            }
        }
        Err(refused(last_status))
    }

    /// One request with status semantics. Returns the redirect hop for the
    /// caller to follow (lookups) or reject (searches).
    async fn get(
        &self,
        segments: Vec<String>,
        mut params: Vec<(String, String)>,
        criticality: Criticality,
    ) -> Result<WireOutcome, ProviderError> {
        let source = self.source();
        let client = &self.client;
        let (executor, base_text) = match &source {
            MbSource::Unreadable => {
                return Err(ProviderError::NotConfigured {
                    provider: SOURCE,
                    message: "the musicbrainz settings cannot be read".to_owned(),
                });
            }
            MbSource::BrainzMash {
                binding_valid: false,
                ..
            } => {
                return Err(ProviderError::NotConfigured {
                    provider: SOURCE,
                    message: "brainzmash active binding is not valid".to_owned(),
                });
            }
            MbSource::BrainzMash { base_url, .. } => (&client.brainzmash, base_url),
            MbSource::Official { base_url } => (&client.official, base_url),
            MbSource::Mirror {
                base_url,
                rate_per_sec,
            } => {
                // Zero or less is the owner's "unlimited" for their mirror.
                let rate = if rate_per_sec.is_finite() {
                    *rate_per_sec
                } else {
                    0.0
                };
                client.mirror.limiter().retune(rate);
                (&client.mirror, base_url)
            }
        };
        let joined = format!("/{}", segments.join("/"));
        let request_path = if source.is_brainzmash() {
            validate_brainzmash_path(&joined)?
        } else {
            joined
        };
        let base = parse_base(base_text).ok_or(ProviderError::Invalid {
            provider: SOURCE,
            reason: "the MusicBrainz address is not a valid URL",
        })?;
        params.push(("fmt".to_owned(), "json".to_owned()));
        let query: Vec<(&str, &str)> = params
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
        let request = Request::get(&base, &segments, &query)
            .map_err(|_| ProviderError::Invalid {
                provider: SOURCE,
                reason: "the MusicBrainz address is not a valid URL",
            })?
            .redirects(Redirects::Return);
        match executor.send(request, self.priority).await {
            Ok(reply) if (300..400).contains(&reply.status()) => {
                let request_url = format!("{}{request_path}", base_text.trim_end_matches('/'));
                let hop = redirect_hop(&source, &request_url, &request_path, &reply)?;
                Ok(WireOutcome::Redirect {
                    hop: hop.hop,
                    next_path: hop.next_path,
                    status: reply.status(),
                })
            }
            Ok(reply) => Ok(WireOutcome::Found(reply)),
            Err(ProviderError::NotFound { .. }) => Ok(WireOutcome::Missing),
            // A dead or busy provider on a best-effort read is a recorded
            // absence; the executor has recorded it.
            Err(error) if criticality == Criticality::BestEffort && error.is_retriable() => {
                tracing::debug!(%error, "musicbrainz read degraded to absence");
                Ok(WireOutcome::Degraded)
            }
            Err(error) => Err(error),
        }
    }
}

/// Validate one 3xx into a followable hop, or refuse it.
fn redirect_hop(
    source: &MbSource,
    request_url: &str,
    request_path: &str,
    reply: &Reply,
) -> Result<FollowHop, ProviderError> {
    let location = reply.header("location").unwrap_or("");
    if source.is_brainzmash() {
        if let Some(hop_path) =
            brainzmash_redirect_path(request_url, reply.status(), reply.header("location"))
            && let Some(hop) = lookup_redirect_pair(request_path, &hop_path)
        {
            return Ok(FollowHop {
                hop,
                next_path: hop_path,
            });
        }
    } else if let Some(hop) =
        official_redirect_hop(request_url, source.base_url(), request_path, location)
    {
        let next_path = format!("/{}/{}", hop.entity, hop.to_mbid);
        return Ok(FollowHop { hop, next_path });
    }
    tracing::debug!(
        status = reply.status(),
        path = request_path,
        "musicbrainz redirect refused"
    );
    Err(refused(reply.status()))
}

fn refused(status: u16) -> ProviderError {
    ProviderError::Redirect {
        provider: SOURCE,
        status,
    }
}

/// An http or https base that can carry a path.
fn parse_base(text: &str) -> Option<Url> {
    let url = Url::parse(text.trim()).ok()?;
    (matches!(url.scheme(), "http" | "https") && !url.cannot_be_a_base() && url.has_host())
        .then_some(url)
}

/// `/entity/mbid` as its segments.
fn path_segments(path: &str) -> Vec<String> {
    path.trim_matches('/')
        .split('/')
        .map(str::to_owned)
        .collect()
}

/// One request's classified result.
enum WireOutcome {
    /// 200 with a body to decode.
    Found(Reply),
    /// 404: definitive absence (a 404 today may still resolve after later
    /// edits, so misses are never cached as permanent elsewhere).
    Missing,
    /// Provider dead on a non-critical call: recorded, resolving to None.
    Degraded,
    /// Followable same-origin lookup redirect.
    Redirect {
        /// Validated hop for the durable map.
        hop: RedirectHop,
        /// Next lookup path.
        next_path: String,
        /// The 3xx status.
        status: u16,
    },
}

/// Validated redirect hop plus the path to request next.
struct FollowHop {
    hop: RedirectHop,
    next_path: String,
}

/// Smallest sorted include set for the request (v2 sorts and dedupes the
/// caller includes before sending).
fn sorted_includes(includes: &[&str]) -> String {
    let mut selected: Vec<&str> = includes.to_vec();
    selected.sort_unstable();
    selected.dedup();
    selected.join("+")
}
