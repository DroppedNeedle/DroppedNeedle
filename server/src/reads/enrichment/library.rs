//! The production enrichment port Library Management plans through: the
//! catalog's ListenBrainz and Last.fm clients, LRCLIB, the shared artwork
//! cache and AcoustID, behind the library's [`EnrichmentPort`].

use std::sync::Arc;

use futures_util::future::BoxFuture;

use crate::library::management::enrich::{
    AcoustIdAnswer, AcoustIdPrint, Cover, EnrichmentPort, ExtrasWanted, LyricsFound, LyricsQuery,
    normalize_lrc,
};
use crate::library::management::genre::Candidate;
use crate::library::matching::model::ReleaseExtras;
use crate::reads::catalog::upstream::Upstream;
use crate::reads::platform::artwork::ArtworkService;

/// The production port: the catalog's ListenBrainz and Last.fm clients,
/// LRCLIB, the shared artwork cache, and AcoustID on identification's
/// rate limit.
pub struct LiveEnrichment {
    pub upstream: Upstream,
    pub http: reqwest::Client,
    pub artwork: Option<ArtworkService>,
    pub lrclib: LrclibGate,
    pub acoustid: crate::providers::acoustid::AcoustIdClient<
        crate::providers::adapters::CorePacer,
        crate::providers::adapters::HealthSink,
    >,
    /// Where the AcoustID key is read, per lookup.
    pub config: Arc<crate::runtime_config::ConfigStore>,
}

impl LiveEnrichment {
    /// The AcoustID key from the settings; blank when none is set.
    fn acoustid_key(&self) -> String {
        use crate::runtime_config::secret_sections::TypedLibrary;
        match self.config.get_raw::<TypedLibrary>() {
            Ok(settings) => settings.acoustid_api_key.expose().trim().to_owned(),
            Err(error) => {
                tracing::warn!(%error, "cannot read the AcoustID key; AcoustID IDs wait");
                String::new()
            }
        }
    }
}

/// LRCLIB's local pacing and answer cache (the provider notes leave both
/// to wiring): one request a second, answers kept seven days and "no
/// lyrics" six hours, and a 429 holds every lookup back for its
/// `Retry-After`.
#[derive(Default)]
pub struct LrclibGate {
    next_request: tokio::sync::Mutex<Option<tokio::time::Instant>>,
    held_until: std::sync::Mutex<Option<std::time::Instant>>,
    answers: std::sync::Mutex<
        std::collections::HashMap<String, (std::time::Instant, Option<LyricsFound>)>,
    >,
}

/// LRCLIB's request spacing.
const LRCLIB_SPACING: std::time::Duration = std::time::Duration::from_secs(1);
/// How long a found answer and a "no lyrics" answer are kept.
const LRCLIB_FOUND_FOR: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);
const LRCLIB_MISSING_FOR: std::time::Duration = std::time::Duration::from_secs(6 * 3600);
/// Most answers kept; past it, expired ones go first, then the oldest.
const LRCLIB_CACHE_ENTRIES: usize = 20_000;

impl LrclibGate {
    fn key(query: &LyricsQuery, duration: u32) -> String {
        format!(
            "{}\u{1f}{}\u{1f}{}\u{1f}{duration}",
            query.title, query.artist, query.album
        )
    }

    fn cached(&self, key: &str) -> Option<Option<LyricsFound>> {
        let answers = self.answers.lock().ok()?;
        let (stored, answer) = answers.get(key)?;
        let keep = if answer.is_some() {
            LRCLIB_FOUND_FOR
        } else {
            LRCLIB_MISSING_FOR
        };
        (stored.elapsed() < keep).then(|| answer.clone())
    }

    fn remember(&self, key: String, answer: Option<LyricsFound>) {
        let Ok(mut answers) = self.answers.lock() else {
            return;
        };
        if answers.len() >= LRCLIB_CACHE_ENTRIES {
            answers.retain(|_, (stored, answer)| {
                stored.elapsed()
                    < if answer.is_some() {
                        LRCLIB_FOUND_FOR
                    } else {
                        LRCLIB_MISSING_FOR
                    }
            });
            while answers.len() >= LRCLIB_CACHE_ENTRIES {
                let Some(oldest) = answers
                    .iter()
                    .min_by_key(|(_, (stored, _))| *stored)
                    .map(|(key, _)| key.clone())
                else {
                    break;
                };
                answers.remove(&oldest);
            }
        }
        answers.insert(key, (std::time::Instant::now(), answer));
    }

    /// Refuse while a 429's `Retry-After` runs.
    fn check_held(&self) -> Result<(), String> {
        let held = self.held_until.lock().ok().and_then(|held| *held);
        match held {
            Some(until) if std::time::Instant::now() < until => {
                Err("LRCLIB asked for a pause, so lyrics wait for the next run".to_owned())
            }
            _ => Ok(()),
        }
    }

    fn hold(&self, seconds: f64) {
        if let Ok(mut held) = self.held_until.lock() {
            *held = Some(
                std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds.max(1.0)),
            );
        }
    }

    /// Wait for this request's slot, one a second.
    async fn pace(&self) {
        let mut next = self.next_request.lock().await;
        let now = tokio::time::Instant::now();
        let slot = next.map_or(now, |next| next.max(now));
        *next = Some(slot + LRCLIB_SPACING);
        drop(next);
        tokio::time::sleep_until(slot).await;
    }
}

impl EnrichmentPort for LiveEnrichment {
    fn listenbrainz_genres<'a>(
        &'a self,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>> {
        use crate::providers::listenbrainz::Outcome;
        Box::pin(async move {
            let (client, creds) = self
                .upstream
                .listenbrainz()
                .ok_or_else(|| "ListenBrainz is switched off".to_owned())?;
            let answer = client
                .release_group_genres(&[release_group_mbid.to_owned()], &creds)
                .await
                .map_err(|error| error.to_string())?;
            match answer {
                Outcome::Found(groups) => Ok(groups
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
                    .unwrap_or_default()),
                Outcome::Missing => Ok(Vec::new()),
                Outcome::Unavailable { message, .. } => Err(message),
            }
        })
    }

    fn lastfm_genres<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>> {
        use crate::providers::lastfm::Outcome;
        Box::pin(async move {
            let (client, creds) = self
                .upstream
                .lastfm("")
                .await
                .ok_or_else(|| "Last.fm has no usable key".to_owned())?;
            let mut out = Vec::new();
            for answer in [
                client.album_top_genres(&creds, artist, album).await,
                client.artist_top_genres(&creds, artist).await,
            ] {
                match answer {
                    Outcome::Found(tags) => out.extend(tags.into_iter().map(|tag| Candidate {
                        name: tag.name,
                        count: Some(tag.weight),
                        curated: false,
                    })),
                    Outcome::Missing => {}
                    Outcome::Unavailable { message, .. } => return Err(message),
                }
            }
            Ok(out)
        })
    }

    fn lyrics<'a>(
        &'a self,
        query: &'a LyricsQuery,
    ) -> BoxFuture<'a, Result<Option<LyricsFound>, String>> {
        use crate::providers::lrclib::{FetchError, LrclibClient};
        Box::pin(async move {
            let duration = query.duration_seconds.round();
            if !duration.is_finite() || duration <= 0.0 {
                return Ok(None);
            }
            let key = LrclibGate::key(query, duration as u32);
            if let Some(answer) = self.lrclib.cached(&key) {
                return Ok(answer);
            }
            self.lrclib.check_held()?;
            self.lrclib.pace().await;
            let transport = crate::providers::adapters::ReqwestGet::new(self.http.clone());
            let client = LrclibClient::with_base(&transport, &self.upstream.endpoints().lrclib);
            let found = match client
                .get_exact_lyrics(&query.title, &query.artist, &query.album, duration as u32)
                .await
            {
                Ok(found) => found,
                Err(FetchError::RateLimited { retry_after_secs }) => {
                    self.lrclib.hold(retry_after_secs);
                    return Err("LRCLIB asked for a pause, so lyrics wait for the next run".into());
                }
                Err(error) => return Err(format!("LRCLIB did not answer: {error:?}")),
            };
            let answer = lyrics_answer(query, found);
            self.lrclib.remember(key, answer.clone());
            Ok(answer)
        })
    }

    fn front_cover<'a>(
        &'a self,
        release_mbid: &'a str,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<Cover>, String>> {
        Box::pin(async move {
            let artwork = self
                .artwork
                .as_ref()
                .ok_or_else(|| "artwork is switched off".to_owned())?;
            match artwork
                .front_cover(release_mbid, release_group_mbid, None)
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
        use crate::library::identify::sources::{
            NAME_INCLUDES, RELATION_INCLUDES, names_from_wire, relations_from_wire,
        };
        use crate::providers::RequestPriority;
        use crate::providers::musicbrainz::Criticality;
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
            let priority = if background {
                RequestPriority::BackgroundSync
            } else {
                RequestPriority::UserInitiated
            };
            let (client, _) = self.upstream.musicbrainz(priority);
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
        use crate::providers::acoustid::{BatchOutcome, BatchQuery};
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
            match self.acoustid.lookup_batch_request(&key, &queries).await {
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
    found: crate::providers::lrclib::LyricsLookup,
) -> Option<LyricsFound> {
    use crate::providers::lrclib::signature_matches;
    let candidate = found.candidate.filter(|_| found.found)?;
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
