//! What Library Management asks the outside world for while it plans an
//! album: ListenBrainz and Last.fm genres, LRCLIB lyrics, and the front
//! cover from the artwork cache. Planning runs on a blocking worker
//! thread, so each call is driven to completion there with a time limit;
//! an answer that does not come in time counts as *deferred*, never as
//! "nothing there", so a partial answer never replaces what a file holds.

use std::sync::Arc;
use std::time::Duration;

use futures_util::future::BoxFuture;

use super::genre::Candidate;

/// Longest one outside call may take while planning.
const CALL_LIMIT: Duration = Duration::from_secs(20);

/// What LRCLIB is asked for one track.
#[derive(Debug, Clone, PartialEq)]
pub struct LyricsQuery {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub duration_seconds: f64,
}

/// Lyrics LRCLIB matched exactly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LyricsFound {
    pub plain: Option<String>,
    /// LRC text, one timed line per line.
    pub synced: Option<String>,
}

/// A front cover image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cover {
    pub bytes: Vec<u8>,
    pub mime: String,
}

/// The outside sources planning reads. `Err` means the source could not
/// answer now; `Ok(None)` (or an empty list) that it has nothing.
pub trait EnrichmentPort: Send + Sync {
    fn listenbrainz_genres<'a>(
        &'a self,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>>;
    fn lastfm_genres<'a>(
        &'a self,
        artist: &'a str,
        album: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>>;
    fn lyrics<'a>(
        &'a self,
        query: &'a LyricsQuery,
    ) -> BoxFuture<'a, Result<Option<LyricsFound>, String>>;
    fn front_cover<'a>(
        &'a self,
        release_mbid: &'a str,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<Cover>, String>>;
}

/// The slot the port is attached to at boot (empty in test bundles until a
/// test scripts one).
pub type EnrichmentSlot = Arc<std::sync::OnceLock<Arc<dyn EnrichmentPort>>>;

/// Drive one call to completion from the blocking planning thread. `None`
/// when there is no runtime, the call fails, or it runs out of time.
pub fn wait<T>(call: BoxFuture<'_, Result<T, String>>) -> Option<T> {
    let handle = tokio::runtime::Handle::try_current().ok()?;
    // Planning runs on a blocking thread, outside any async context.
    let outcome = handle.block_on(async { tokio::time::timeout(CALL_LIMIT, call).await });
    match outcome {
        Ok(Ok(value)) => Some(value),
        Ok(Err(error)) => {
            tracing::info!(%error, "library management enrichment deferred");
            None
        }
        Err(_) => {
            tracing::info!("library management enrichment timed out");
            None
        }
    }
}

/// What planning gathered for one album before its files are planned.
#[derive(Debug, Clone, Default)]
pub struct AlbumExtras {
    pub genres: super::genre::RemoteGenres,
}

/// Ask the outside sources the profile needs for one album.
pub fn gather(
    slot: &EnrichmentSlot,
    profile: &crate::runtime_config::sections::LibraryManagementProfile,
    release: &crate::library::matching::Release,
) -> AlbumExtras {
    use crate::runtime_config::sections::GenreSource;
    let mut extras = AlbumExtras::default();
    let Some(port) = slot.get() else {
        return extras;
    };
    if profile.genres.enabled {
        if profile.genres.sources.contains(&GenreSource::Listenbrainz) {
            extras.genres.listenbrainz = wait(port.listenbrainz_genres(&release.release_group_id));
        }
        if profile.genres.sources.contains(&GenreSource::Lastfm) {
            let artist = release.artist_text();
            extras.genres.lastfm = wait(port.lastfm_genres(&artist, &release.title));
        }
    }
    extras
}

/// Normalize LRC text: Unix line ends, no blank edges, one final newline.
pub fn normalize_lrc(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
    let body = lines.join("\n");
    format!("{}\n", body.trim())
}

/// The production port: the catalog's ListenBrainz and Last.fm clients,
/// LRCLIB, and the shared artwork cache.
pub struct LiveEnrichment {
    pub upstream: crate::reads::catalog::upstream::Upstream,
    pub http: reqwest::Client,
    pub artwork: Option<crate::reads::platform::artwork::ArtworkService>,
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
        use crate::providers::lrclib::{LrclibClient, signature_matches};
        Box::pin(async move {
            let duration = query.duration_seconds.round();
            if !duration.is_finite() || duration <= 0.0 {
                return Ok(None);
            }
            let transport = crate::providers::adapters::ReqwestGet::new(self.http.clone());
            let client = LrclibClient::new(&transport);
            let found = client
                .get_exact_lyrics(&query.title, &query.artist, &query.album, duration as u32)
                .await
                .map_err(|error| format!("{error:?}"))?;
            let Some(candidate) = found.candidate.filter(|_| found.found) else {
                return Ok(None);
            };
            // v2's accuracy gate: the same recording signature, within two
            // seconds.
            let same = signature_matches(&candidate.track_name, &query.title)
                && signature_matches(&candidate.artist_name, &query.artist)
                && signature_matches(&candidate.album_name, &query.album)
                && (candidate.duration_seconds - query.duration_seconds).abs() <= 2.0;
            if !same || candidate.instrumental {
                return Ok(None);
            }
            Ok(Some(LyricsFound {
                plain: candidate
                    .plain_lyrics
                    .filter(|text| !text.trim().is_empty()),
                synced: candidate
                    .synced_lyrics
                    .filter(|text| !text.trim().is_empty())
                    .map(|text| normalize_lrc(&text)),
            }))
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
}

/// Scripted answers for the test suite.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub struct ScriptedEnrichment {
    pub listenbrainz: Option<Vec<Candidate>>,
    pub lyrics: Option<LyricsFound>,
    pub cover: Option<Cover>,
}

#[cfg(any(test, feature = "test-support"))]
impl EnrichmentPort for ScriptedEnrichment {
    fn listenbrainz_genres<'a>(
        &'a self,
        _release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>> {
        let answer = self
            .listenbrainz
            .clone()
            .ok_or_else(|| "not scripted".to_owned());
        Box::pin(async move { answer })
    }

    fn lastfm_genres<'a>(
        &'a self,
        _artist: &'a str,
        _album: &'a str,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, String>> {
        Box::pin(async { Err("not scripted".to_owned()) })
    }

    fn lyrics<'a>(
        &'a self,
        _query: &'a LyricsQuery,
    ) -> BoxFuture<'a, Result<Option<LyricsFound>, String>> {
        let answer = Ok(self.lyrics.clone());
        Box::pin(async move { answer })
    }

    fn front_cover<'a>(
        &'a self,
        _release_mbid: &'a str,
        _release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<Cover>, String>> {
        let answer = Ok(self.cover.clone());
        Box::pin(async move { answer })
    }
}
