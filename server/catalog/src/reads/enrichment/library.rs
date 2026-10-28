//! The production enrichment port Library Management plans through: the
//! catalog's ListenBrainz and Last.fm clients, the online lyrics, the shared
//! artwork cache and AcoustID, behind the library's [`EnrichmentPort`]. A person's
//! own preview asks at user priority, an automatic run at background
//! priority; AcoustID lookups are fingerprinting and always background.

use futures_util::future::BoxFuture;

use crate::reads::catalog::upstream::Upstream;
use crate::reads::lyrics::{LyricsError, OnlineLyrics};
use crate::reads::platform::artwork::ArtworkService;
use droppedneedle_library::management::enrich::{
    AcoustIdAnswer, AcoustIdPrint, Cover, EnrichmentPort, ExtrasWanted, LyricsFound, LyricsQuery,
    normalize_lrc,
};
use droppedneedle_library::management::genre::Candidate;
use droppedneedle_library::matching::model::ReleaseExtras;
use droppedneedle_providers::RequestPriority;

/// The priority a call asks at: background for automatic runs, user for a
/// person's own preview.
fn priority(background: bool) -> RequestPriority {
    if background {
        RequestPriority::BackgroundSync
    } else {
        RequestPriority::UserInitiated
    }
}

/// The production port: the catalog's ListenBrainz and Last.fm clients,
/// the online lyrics the player shares, the shared artwork cache, and
/// AcoustID on identification's rate limit.
pub struct LiveEnrichment {
    pub upstream: Upstream,
    pub artwork: Option<ArtworkService>,
    pub lyrics: OnlineLyrics,
    pub acoustid: droppedneedle_providers::acoustid::AcoustIdClient,
    /// The library settings the AcoustID key is read from, per lookup.
    pub library: droppedneedle_kernel::runtime_config::Live<
        droppedneedle_kernel::runtime_config::secret_sections::TypedLibrary,
    >,
}

impl LiveEnrichment {
    /// The AcoustID key from the settings; blank when none is set.
    fn acoustid_key(&self) -> String {
        match self.library.get() {
            Ok(settings) => settings.acoustid_api_key.expose().trim().to_owned(),
            Err(error) => {
                tracing::warn!(%error, "cannot read the AcoustID key; AcoustID IDs wait");
                String::new()
            }
        }
    }
}

/// What a planner reads while LRCLIB rests after a 429 or an outage.
const LRCLIB_RESTING: &str = "LRCLIB asked for a pause, so lyrics wait for the next run";

impl EnrichmentPort for LiveEnrichment {
    fn listenbrainz_genres<'a>(
        &'a self,
        release_group_mbid: &'a str,
        background: bool,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>> {
        Box::pin(async move {
            let (client, creds) = self
                .upstream
                .listenbrainz()
                .ok_or_else(|| "ListenBrainz is switched off".to_owned())?;
            let groups = client
                .release_group_genres(
                    &[release_group_mbid.to_owned()],
                    &creds,
                    priority(background),
                )
                .await
                .map_err(|error| error.to_string())?;
            Ok(groups
                .get(release_group_mbid)
                .map(|metadata| {
                    metadata
                        .release_group_tags
                        .iter()
                        .map(|tag| Candidate {
                            name: tag.tag.clone(),
                            count: Some(tag.count),
                            curated: tag.genre_mbid.is_some(),
                        })
                        .collect()
                })
                .unwrap_or_default())
        })
    }

    fn lastfm_genres<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
        background: bool,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>> {
        Box::pin(async move {
            let (client, creds) = self
                .upstream
                .lastfm("")
                .await
                .ok_or_else(|| "Last.fm has no usable key".to_owned())?;
            let priority = priority(background);
            let mut out = Vec::new();
            for answer in [
                client
                    .album_top_genres(&creds, artist, album, priority)
                    .await,
                client.artist_top_genres(&creds, artist, priority).await,
            ] {
                let tags = answer.map_err(|error| error.to_string())?;
                out.extend(tags.into_iter().map(|tag| Candidate {
                    name: tag.name,
                    count: Some(tag.weight),
                    curated: false,
                }));
            }
            Ok(out)
        })
    }

    fn lyrics<'a>(
        &'a self,
        query: &'a LyricsQuery,
        background: bool,
    ) -> BoxFuture<'a, Result<Option<LyricsFound>, String>> {
        Box::pin(async move {
            let lookup = crate::reads::lyrics::LyricsQuery {
                title: &query.title,
                artist: &query.artist,
                album: &query.album,
                duration_secs: query.duration_seconds,
            };
            match self.lyrics.lookup(lookup, priority(background)).await {
                Ok(candidate) => {
                    Ok(candidate.and_then(|candidate| lyrics_answer(query, candidate)))
                }
                Err(LyricsError::Resting) => Err(LRCLIB_RESTING.to_owned()),
                Err(LyricsError::Failed(error)) => Err(format!("LRCLIB did not answer: {error}")),
            }
        })
    }

    fn front_cover<'a>(
        &'a self,
        release_mbid: &'a str,
        release_group_mbid: &'a str,
        background: bool,
    ) -> BoxFuture<'a, Result<Option<Cover>, String>> {
        Box::pin(async move {
            let artwork = self
                .artwork
                .as_ref()
                .ok_or_else(|| "artwork is switched off".to_owned())?;
            match artwork
                .front_cover(
                    release_mbid,
                    release_group_mbid,
                    None,
                    if background {
                        RequestPriority::BackgroundSync
                    } else {
                        RequestPriority::ImageFetch
                    },
                )
                .await
            {
                Ok(found) => Ok(found.map(|cover| Cover {
                    bytes: cover.bytes,
                    mime: cover.content_type,
                })),
                Err(()) => Err("the cover is still being fetched".to_owned()),
            }
        })
    }

    fn release_extras<'a>(
        &'a self,
        release_mbid: &'a str,
        wants: ExtrasWanted,
        background: bool,
    ) -> BoxFuture<'a, Result<ReleaseExtras, String>> {
        use droppedneedle_library::identify::sources::{
            NAME_INCLUDES, RELATION_INCLUDES, names_from_wire, relations_from_wire,
        };
        use droppedneedle_providers::musicbrainz::Criticality;
        Box::pin(async move {
            let mut includes: Vec<&str> = Vec::new();
            if wants.relations {
                includes.extend(RELATION_INCLUDES);
            }
            if wants.names {
                includes.extend(NAME_INCLUDES);
            }
            includes.sort_unstable();
            includes.dedup();
            if includes.is_empty() {
                return Ok(ReleaseExtras::default());
            }
            let (client, _) = self.upstream.musicbrainz(priority(background));
            // Identity-critical: a best-effort lookup turns an outage into
            // "not found", which must never read as "none".
            let found = client
                .lookup_release(release_mbid, &includes, Criticality::IdentityCritical)
                .await
                .map_err(|error| format!("MusicBrainz could not be read: {error}"))?;
            let lookup =
                found.ok_or_else(|| "MusicBrainz no longer has this release".to_owned())?;
            Ok(ReleaseExtras {
                relations: wants
                    .relations
                    .then(|| relations_from_wire(&lookup.entity, 0)),
                names: wants.names.then(|| names_from_wire(&lookup.entity, 0)),
            })
        })
    }

    fn acoustid_lookup<'a>(&'a self, prints: &'a [AcoustIdPrint]) -> BoxFuture<'a, AcoustIdAnswer> {
        use droppedneedle_providers::acoustid::{BatchOutcome, BatchQuery};
        Box::pin(async move {
            let key = self.acoustid_key();
            if key.is_empty() {
                return AcoustIdAnswer::NoKey;
            }
            if prints.is_empty() {
                return AcoustIdAnswer::Answered(Vec::new());
            }
            let queries: Vec<BatchQuery<'_>> = prints
                .iter()
                .map(|print| BatchQuery {
                    fingerprint: &print.print,
                    duration_secs: u64::from(print.duration_seconds),
                })
                .collect();
            match self
                .acoustid
                .lookup_batch_request(&key, &queries, RequestPriority::BackgroundSync)
                .await
            {
                BatchOutcome::Answered(entries) => {
                    let mut out = vec![Vec::new(); prints.len()];
                    for (index, results) in entries {
                        if let Some(slot) = out.get_mut(index) {
                            *slot = results;
                        }
                    }
                    AcoustIdAnswer::Answered(out)
                }
                BatchOutcome::Rejected { message } => {
                    tracing::warn!(message, "AcoustID refused the lookup; AcoustID IDs wait");
                    AcoustIdAnswer::NoKey
                }
                BatchOutcome::Unavailable { message, .. } => {
                    tracing::info!(message, "AcoustID lookup deferred");
                    AcoustIdAnswer::Unavailable
                }
            }
        })
    }
}

/// v2's accuracy gate on an exact LRCLIB answer: the same recording
/// signature, within two seconds, and not instrumental.
fn lyrics_answer(
    query: &LyricsQuery,
    candidate: droppedneedle_providers::lrclib::LyricsCandidate,
) -> Option<LyricsFound> {
    use droppedneedle_providers::lrclib::signature_matches;
    let same = signature_matches(&candidate.track_name, &query.title)
        && signature_matches(&candidate.artist_name, &query.artist)
        && signature_matches(&candidate.album_name, &query.album)
        && (candidate.duration_seconds - query.duration_seconds).abs() <= 2.0;
    if !same || candidate.instrumental {
        return None;
    }
    Some(LyricsFound {
        plain: candidate
            .plain_lyrics
            .filter(|text| !text.trim().is_empty()),
        synced: candidate
            .synced_lyrics
            .filter(|text| !text.trim().is_empty())
            .map(|text| normalize_lrc(&text)),
    })
}
