//! The MusicBrainz client: one paced, identified request per call, with
//! redirect proof and criticality routing. Every request carries
//! `fmt=json`, the smallest sorted include set the caller asked for (never
//! a broad default: the full verification include set decoded ~608 KB for
//! one 34-track release, live 2026-07-21), and the descriptive DroppedNeedle
//! User-Agent from the HTTP client factory.

use serde::Deserialize;

use super::models::{
    ArtistSearchHit, MbArtist, MbRecording, MbRelease, MbReleaseGroup, RecordingSearchHit,
    ReleaseGroupBrowsePage, ReleaseGroupSearchHit, ReleaseSearchHit, SearchPage, UrlResolution,
    decode_search_page,
};
use super::pacing::MbPacing;
use super::redirects::{
    RedirectHop, brainzmash_redirect_path, lookup_redirect_pair, official_redirect_hop,
    validate_brainzmash_path,
};
use super::transport::{MbRequest, MbTransport, RawResponse};
use super::{
    Criticality, MAX_PAGE_LIMIT, MAX_REDIRECT_HOPS, MbError, MbSource, SourceFn,
    build_recording_search_query, build_release_group_search_query, build_release_search_query,
    escape_lucene_phrase, is_valid_mbid, normalize_mb_id, parse_retry_after_secs,
};
use crate::providers::RequestPriority;
use crate::providers::degradation::{DegradationSink, NoopSink};

/// Lookup result: the entity plus the redirect proof behind it.
#[derive(Debug, Clone)]
pub struct Lookup<T> {
    /// Decoded entity (canonical id after any redirects).
    pub entity: T,
    /// Followed hops, oldest first, for the durable canonical map.
    pub redirects: Vec<RedirectHop>,
}

/// MusicBrainz client over any [`MbTransport`]. Owns status semantics,
/// redirect walking and criticality routing, and paces every attempt
/// through the shared [`MbPacing`]; owns no cache and no persistence.
pub struct MusicBrainzClient<T: MbTransport, S: DegradationSink = NoopSink> {
    transport: T,
    sink: S,
    source: MbSource,
    resolve: Option<SourceFn>,
    pacing: MbPacing,
    priority: RequestPriority,
}

impl<T: MbTransport> MusicBrainzClient<T, NoopSink> {
    /// Client against `source`, paced through the shared limiters at user
    /// priority.
    pub fn new(transport: T, source: MbSource, pacing: MbPacing) -> Self {
        Self {
            transport,
            sink: NoopSink,
            source,
            resolve: None,
            pacing,
            priority: RequestPriority::UserInitiated,
        }
    }

    /// Client against the official service.
    pub fn official(transport: T, pacing: MbPacing) -> Self {
        Self::new(transport, MbSource::official(), pacing)
    }

    /// Client against BrainzMash. `binding_valid` must be true before any
    /// request is sent; otherwise calls fail closed without touching the
    /// wire (v2 raises "BrainzMash active binding is not valid").
    pub fn brainzmash(transport: T, binding_valid: bool, pacing: MbPacing) -> Self {
        Self::new(transport, MbSource::BrainzMash { binding_valid }, pacing)
    }
}

impl<T: MbTransport, S: DegradationSink> MusicBrainzClient<T, S> {
    /// Swap the degradation sink (the enrichment aggregator mounts its own).
    pub fn with_sink<N: DegradationSink>(self, sink: N) -> MusicBrainzClient<T, N> {
        MusicBrainzClient {
            transport: self.transport,
            sink,
            source: self.source,
            resolve: self.resolve,
            pacing: self.pacing,
            priority: self.priority,
        }
    }

    /// Wait for limiter tokens at `priority`. Background work passes
    /// [`RequestPriority::BackgroundSync`] so user page loads go first.
    #[must_use]
    pub fn with_priority(mut self, priority: RequestPriority) -> Self {
        self.priority = priority;
        self
    }

    /// Read the source from `resolve` at every request instead of the
    /// one fixed at construction.
    #[must_use]
    pub fn with_source_fn(mut self, resolve: SourceFn) -> Self {
        self.resolve = Some(resolve);
        self
    }

    /// The source the next request goes to.
    pub fn source(&self) -> MbSource {
        self.resolve
            .as_ref()
            .map_or_else(|| self.source.clone(), |resolve| resolve())
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
    ) -> Result<SearchPage<ArtistSearchHit>, MbError> {
        let escaped = escape_lucene_phrase(text.trim());
        let query = format!(
            r#"artist:"{escaped}"^3 OR artistaccent:"{escaped}"^3 OR alias:"{escaped}"^2 OR {escaped}"#
        );
        self.search_page(
            "/artist",
            &query,
            limit,
            offset,
            "artists",
            "search_artists",
            criticality,
        )
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
    ) -> Result<SearchPage<ReleaseGroupSearchHit>, MbError> {
        let escaped = escape_lucene_phrase(text.trim());
        let query = format!(r#"releasegroup:"{escaped}"^3 OR release:"{escaped}"^2 OR {escaped}"#);
        self.search_page(
            "/release-group",
            &query,
            limit,
            offset,
            "release-groups",
            "search_release_groups",
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
    ) -> Result<ReleaseGroupBrowsePage, MbError> {
        let operation = "browse_release_groups";
        let params = vec![
            ("artist".to_owned(), normalize_mb_id(artist_mbid)),
            ("inc".to_owned(), "artist-credits".to_owned()),
            ("limit".to_owned(), limit.min(MAX_PAGE_LIMIT).to_string()),
            ("offset".to_owned(), offset.to_string()),
        ];
        match self
            .get("/release-group", params, operation, criticality)
            .await?
        {
            WireOutcome::Found(body) => serde_json::from_slice(&body).map_err(|error| {
                MbError::Contract(format!("{operation} payload breaks the contract: {error}"))
            }),
            WireOutcome::Missing | WireOutcome::Degraded => Ok(ReleaseGroupBrowsePage {
                count: 0,
                offset: u64::from(offset),
                items: Vec::new(),
            }),
            WireOutcome::Redirect { .. } => Err(MbError::RedirectRejected(format!(
                "{operation} never follows redirects"
            ))),
        }
    }

    /// Search releases by title and artist (verified Lucene shape).
    pub async fn search_releases(
        &self,
        title: &str,
        artist: &str,
        limit: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ReleaseSearchHit>, MbError> {
        let query = build_release_search_query(title, artist);
        self.search_page(
            "/release",
            &query,
            limit,
            0,
            "releases",
            "search_releases",
            criticality,
        )
        .await
    }

    /// Search release groups by title and artist (verified Lucene shape).
    pub async fn search_release_groups(
        &self,
        title: &str,
        artist: &str,
        limit: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ReleaseGroupSearchHit>, MbError> {
        let query = build_release_group_search_query(title, artist);
        self.search_page(
            "/release-group",
            &query,
            limit,
            0,
            "release-groups",
            "search_release_groups",
            criticality,
        )
        .await
    }

    /// Search artists by name.
    pub async fn search_artists(
        &self,
        name: &str,
        limit: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<ArtistSearchHit>, MbError> {
        let query = format!(r#"artist:"{}""#, escape_lucene_phrase(name));
        self.search_page(
            "/artist",
            &query,
            limit,
            0,
            "artists",
            "search_artists",
            criticality,
        )
        .await
    }

    /// Search recordings by track title and artist (verified Lucene shape).
    pub async fn search_recordings(
        &self,
        title: &str,
        artist: &str,
        limit: u32,
        criticality: Criticality,
    ) -> Result<SearchPage<RecordingSearchHit>, MbError> {
        let query = build_recording_search_query(title, artist);
        self.search_page(
            "/recording",
            &query,
            limit,
            0,
            "recordings",
            "search_recordings",
            criticality,
        )
        .await
    }

    /// Look up one release with the caller's include set.
    pub async fn lookup_release(
        &self,
        mbid: &str,
        includes: &[&str],
        criticality: Criticality,
    ) -> Result<Option<Lookup<MbRelease>>, MbError> {
        let path = format!("/release/{}", normalize_mb_id(mbid));
        self.lookup(&path, includes, "lookup_release", criticality)
            .await
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
    ) -> Result<Option<Lookup<MbRelease>>, MbError> {
        let found = self
            .lookup_release(
                mbid,
                &["artist-credits", "recordings", "release-groups"],
                criticality,
            )
            .await?;
        match found {
            Some(lookup) if lookup.entity.release_group.is_none() => Err(MbError::Contract(
                format!("exact release {mbid} arrived without its provider release group"),
            )),
            other => Ok(other),
        }
    }

    /// Resolve a release MBID to its release-group MBID (v2
    /// `MusicBrainzIdResolver`: tags carry the release id, the library
    /// keys on the group id).
    pub async fn resolve_release_to_release_group(
        &self,
        release_mbid: &str,
        criticality: Criticality,
    ) -> Result<Option<String>, MbError> {
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
    ) -> Result<Option<Lookup<MbReleaseGroup>>, MbError> {
        let path = format!("/release-group/{}", normalize_mb_id(mbid));
        self.lookup(&path, includes, "lookup_release_group", criticality)
            .await
    }

    /// Look up one artist. The detail surface uses
    /// `tags+aliases+url-rels` (v2 artist mixin).
    pub async fn lookup_artist(
        &self,
        mbid: &str,
        includes: &[&str],
        criticality: Criticality,
    ) -> Result<Option<Lookup<MbArtist>>, MbError> {
        let path = format!("/artist/{}", normalize_mb_id(mbid));
        self.lookup(&path, includes, "lookup_artist", criticality)
            .await
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
    ) -> Result<Option<Lookup<MbRecording>>, MbError> {
        let path = format!("/recording/{}", normalize_mb_id(mbid));
        self.lookup(&path, includes, "lookup_recording", criticality)
            .await
    }

    /// Resolve a recording MBID through merge redirects to its canonical
    /// id. A retired MBID counts as equivalent only after this lookup
    /// proves the redirect target; any other outcome keeps the normal
    /// conflict gate (live 2026-08-10, merged recording identifiers).
    pub async fn resolve_recording_mbid(
        &self,
        recording_mbid: &str,
        criticality: Criticality,
    ) -> Result<Option<String>, MbError> {
        let normalized = normalize_mb_id(recording_mbid);
        if !is_valid_mbid(&normalized) {
            return Err(MbError::InvalidMbid(recording_mbid.to_owned()));
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
    ) -> Result<UrlResolution, MbError> {
        let operation = "resolve_url";
        let mut params = vec![("resource".to_owned(), resource.to_owned())];
        let inc = sorted_includes(includes);
        if !inc.is_empty() {
            params.push(("inc".to_owned(), inc));
        }
        match self.get("/url", params, operation, criticality).await? {
            WireOutcome::Found(body) => serde_json::from_slice(&body).map_err(|error| {
                MbError::Contract(format!("url resolution broke the contract: {error}"))
            }),
            WireOutcome::Missing | WireOutcome::Degraded => Ok(UrlResolution::empty(resource)),
            WireOutcome::Redirect { .. } => Err(MbError::RedirectRejected(
                "url resolution never follows redirects".to_owned(),
            )),
        }
    }

    /// One entity search with bucket isolation: provider death on a
    /// non-critical search records a degradation and yields an empty page
    /// (v2 grouped search returns `[]` plus a failed bucket).
    #[allow(clippy::too_many_arguments)]
    async fn search_page<E>(
        &self,
        path: &str,
        query: &str,
        limit: u32,
        offset: u32,
        array_key: &str,
        operation: &'static str,
        criticality: Criticality,
    ) -> Result<SearchPage<E>, MbError>
    where
        E: for<'de> Deserialize<'de>,
    {
        let mut params = vec![
            ("query".to_owned(), query.to_owned()),
            ("limit".to_owned(), limit.min(MAX_PAGE_LIMIT).to_string()),
        ];
        if offset > 0 {
            params.push(("offset".to_owned(), offset.to_string()));
        }
        match self.get(path, params, operation, criticality).await? {
            WireOutcome::Found(body) => decode_search_page(&body, array_key),
            WireOutcome::Missing | WireOutcome::Degraded => Ok(SearchPage {
                count: 0,
                offset: 0,
                items: Vec::new(),
            }),
            WireOutcome::Redirect { .. } => Err(MbError::RedirectRejected(format!(
                "{operation} never follows redirects"
            ))),
        }
    }

    /// One entity lookup with redirect proof and criticality routing.
    async fn lookup<E>(
        &self,
        path: &str,
        includes: &[&str],
        operation: &'static str,
        criticality: Criticality,
    ) -> Result<Option<Lookup<E>>, MbError>
    where
        E: for<'de> Deserialize<'de>,
    {
        let mut params = Vec::new();
        let inc = sorted_includes(includes);
        if !inc.is_empty() {
            params.push(("inc".to_owned(), inc));
        }
        let mut current_path = path.to_owned();
        let mut redirects = Vec::new();
        for _ in 0..=MAX_REDIRECT_HOPS {
            match self
                .get(&current_path, params.clone(), operation, criticality)
                .await?
            {
                WireOutcome::Found(body) => {
                    let entity = serde_json::from_slice(&body).map_err(|error| {
                        MbError::Contract(format!(
                            "{operation} payload breaks the contract: {error}"
                        ))
                    })?;
                    return Ok(Some(Lookup { entity, redirects }));
                }
                WireOutcome::Missing | WireOutcome::Degraded => return Ok(None),
                WireOutcome::Redirect { hop, next_path } => {
                    redirects.push(hop);
                    current_path = next_path;
                }
            }
        }
        Err(MbError::RedirectRejected(format!(
            "{operation} exceeded {MAX_REDIRECT_HOPS} redirect hops"
        )))
    }

    /// One paced wire attempt with status semantics. Returns the redirect
    /// hop for the caller to follow (lookups) or reject (searches).
    async fn get(
        &self,
        path: &str,
        mut params: Vec<(String, String)>,
        operation: &'static str,
        criticality: Criticality,
    ) -> Result<WireOutcome, MbError> {
        let source = self.source();
        if let MbSource::BrainzMash { binding_valid } = &source
            && !binding_valid
        {
            return Err(MbError::Misconfigured(
                "brainzmash active binding is not valid".to_owned(),
            ));
        }
        let brainzmash = source.is_brainzmash();
        let request_path = if brainzmash {
            validate_brainzmash_path(path)?
        } else {
            path.to_owned()
        };
        if let Err(wait) = self.pacing.acquire(&source, self.priority).await {
            let secs = wait.as_secs_f64();
            if criticality == Criticality::IdentityCritical {
                return Err(MbError::RateLimited {
                    retry_after_secs: Some(secs),
                });
            }
            return self.provider_dead(
                operation,
                criticality,
                format!("brainzmash cooling down {secs:.1}s"),
            );
        }
        params.push(("fmt".to_owned(), "json".to_owned()));
        let url = format!(
            "{}{}",
            source.base_url().trim_end_matches('/'),
            request_path
        );
        let request = MbRequest {
            url: url.clone(),
            query: params,
            headers: Vec::new(),
        };
        let response = match self.transport.get(&request).await {
            Ok(response) => response,
            Err(error) => return self.provider_dead(operation, criticality, error.0),
        };
        if brainzmash {
            self.note_brainzmash_status(&response);
        }
        match response.status {
            200 => Ok(WireOutcome::Found(response.body)),
            404 => Ok(WireOutcome::Missing),
            429 | 503 => {
                let retry_after_secs = parse_retry_after_secs(response.header("Retry-After"));
                if brainzmash && response.status == 429 {
                    // Single note_cooldown call for this response: it is not
                    // idempotent, so the helper above skips 429 on purpose.
                    let selected = self.pacing.cooldown().note_cooldown(retry_after_secs);
                    return self.provider_dead(
                        operation,
                        criticality,
                        format!("brainzmash rate limited; cooling down {selected:.1}s"),
                    );
                }
                if brainzmash && response.status == 503 {
                    // A BrainzMash 503 is a dead mirror, not a rate signal:
                    // v2 labels only the official 503 rate-limited.
                    return self.provider_dead(
                        operation,
                        criticality,
                        "brainzmash unavailable (HTTP 503)".to_owned(),
                    );
                }
                if criticality == Criticality::BestEffort {
                    self.sink.record(
                        "musicbrainz",
                        format!(
                            "{operation}: musicbrainz rate limited (HTTP {})",
                            response.status
                        ),
                    );
                    return Ok(WireOutcome::Degraded);
                }
                Err(MbError::RateLimited { retry_after_secs })
            }
            // A lookup 400 names a well-formed id the service rejects; a
            // search or browse 400 is a bad query.
            400 if operation.starts_with("lookup") || operation.starts_with("resolve") => {
                Err(MbError::InvalidMbid(format!("{operation} {path}")))
            }
            300..=399 => {
                let hop = self.redirect_hop(&source, &url, &request_path, &response, operation)?;
                Ok(WireOutcome::Redirect {
                    hop: hop.hop,
                    next_path: hop.next_path,
                })
            }
            400..=499 => Err(MbError::Rejected(response.status)),
            _ => self.provider_dead(
                operation,
                criticality,
                format!("HTTP {} from {}", response.status, source.base_url()),
            ),
        }
    }

    /// Validate one 3xx into a followable hop, or reject it.
    fn redirect_hop(
        &self,
        source: &MbSource,
        request_url: &str,
        request_path: &str,
        response: &RawResponse,
        operation: &'static str,
    ) -> Result<FollowHop, MbError> {
        let location = response.header("location").unwrap_or("");
        if source.is_brainzmash() {
            if let Some(hop_path) =
                brainzmash_redirect_path(request_url, response.status, response.header("location"))
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
        Err(MbError::RedirectRejected(format!(
            "{operation} refused {request_path} -> {location}"
        )))
    }

    /// BrainzMash per-response bookkeeping: 200 clears the cooldown.
    /// The 429 arm below owns the single `note_cooldown` call for its
    /// response (it is not idempotent), so 429 is skipped here on purpose.
    fn note_brainzmash_status(&self, response: &RawResponse) {
        if response.status == 200 {
            self.pacing.cooldown().note_success();
        }
    }

    /// Dead-provider routing: typed failure when identity-critical,
    /// recorded absence otherwise.
    fn provider_dead(
        &self,
        operation: &'static str,
        criticality: Criticality,
        cause: String,
    ) -> Result<WireOutcome, MbError> {
        if criticality == Criticality::IdentityCritical {
            return Err(MbError::Unavailable(cause));
        }
        self.sink.record(
            "musicbrainz",
            format!("{operation}: musicbrainz unavailable: {cause}"),
        );
        Ok(WireOutcome::Degraded)
    }
}

/// One wire attempt's classified result.
enum WireOutcome {
    /// 200 with a body to decode.
    Found(Vec<u8>),
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
