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
    /// Per library track.
    pub files: std::collections::HashMap<String, FileExtras>,
    /// Why ReplayGain was not measured for the album, when it was wanted.
    pub replaygain_note: Option<String>,
    /// The album's front cover, when the profile writes art and one was
    /// found.
    pub cover: Option<Cover>,
    /// The cover could not be fetched now.
    pub cover_deferred: bool,
    /// The library track whose plan carries the album's cover file.
    pub cover_owner: Option<String>,
}

/// What planning gathered for one file.
#[derive(Debug, Clone, Default)]
pub struct FileExtras {
    pub replaygain: Option<ReplayGainValues>,
}

/// Measured ReplayGain tag values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayGainValues {
    pub track_gain: String,
    pub track_peak: String,
    /// Present when the album was measured as a whole.
    pub album_gain: Option<String>,
    pub album_peak: Option<String>,
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
    let artwork = &profile.artwork;
    let archive = artwork.providers.iter().any(|provider| {
        matches!(
            provider,
            crate::runtime_config::sections::ArtworkProvider::CoverArtArchiveRelease
                | crate::runtime_config::sections::ArtworkProvider::CoverArtArchiveReleaseGroup
        )
    });
    if (artwork.embedded_enabled || artwork.external_enabled) && archive {
        let release_id = if super::custom::is_custom(release) {
            ""
        } else {
            release.id.as_str()
        };
        match wait(port.front_cover(release_id, &release.release_group_id)) {
            Some(cover) => extras.cover = cover,
            None => extras.cover_deferred = true,
        }
    }
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

/// What LRCLIB said about one file's lyrics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LyricsAnswer {
    /// The profile does not manage lyrics.
    Off,
    /// LRCLIB could not be asked now.
    Deferred,
    /// No exact match.
    Missing,
    Found(LyricsFound),
}

/// Ask LRCLIB for one track's lyrics by its exact signature (v2's
/// accuracy gate: title, artist, album and a length within two seconds).
pub fn lyrics(
    slot: &EnrichmentSlot,
    settings: &crate::runtime_config::sections::LyricsManagementSettings,
    release: &crate::library::matching::Release,
    track: &crate::library::matching::ReleaseTrack,
) -> LyricsAnswer {
    if !settings.enabled || (!settings.write_plain && !settings.write_synced) {
        return LyricsAnswer::Off;
    }
    let Some(port) = slot.get() else {
        return LyricsAnswer::Deferred;
    };
    let artist = if track.artists.is_empty() {
        release.artist_text()
    } else {
        crate::library::matching::model::credit_text(&track.artists)
    };
    let Some(length) = track.length_ms else {
        return LyricsAnswer::Missing;
    };
    let query = LyricsQuery {
        title: track.title.clone(),
        artist,
        album: release.title.clone(),
        duration_seconds: length as f64 / 1000.0,
    };
    match wait(port.lyrics(&query)) {
        Some(Some(found)) => LyricsAnswer::Found(found),
        Some(None) => LyricsAnswer::Missing,
        None => LyricsAnswer::Deferred,
    }
}

/// A cover ready to write: bytes, MIME type and size in pixels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedCover {
    pub bytes: Vec<u8>,
    pub mime: String,
    pub width: u32,
    pub height: u32,
}

/// Fit a cover inside `max` pixels (0 keeps its size) and encode it as
/// `format` asks (`Original` keeps its encoding when no resize is
/// needed). `None` for bytes that are not a JPEG, PNG or WebP image, or
/// over 40 megapixels (a decompression-bomb guard).
pub fn render_cover(
    bytes: &[u8],
    max: i64,
    format: crate::runtime_config::sections::ArtworkOutputFormat,
) -> Option<RenderedCover> {
    use crate::runtime_config::sections::ArtworkOutputFormat;
    use image::{ImageFormat, ImageReader, imageops::FilterType};
    use std::io::Cursor;

    let source = image::guess_format(bytes).ok()?;
    if !matches!(
        source,
        ImageFormat::Jpeg | ImageFormat::Png | ImageFormat::WebP
    ) {
        return None;
    }
    let (width, height) = ImageReader::with_format(Cursor::new(bytes), source)
        .into_dimensions()
        .ok()?;
    if u64::from(width) * u64::from(height) > 40_000_000 {
        return None;
    }
    let target = match format {
        ArtworkOutputFormat::Original => source,
        ArtworkOutputFormat::Jpeg => ImageFormat::Jpeg,
        ArtworkOutputFormat::Png => ImageFormat::Png,
        ArtworkOutputFormat::Webp => ImageFormat::WebP,
    };
    let limit = u32::try_from(max).unwrap_or(0);
    let shrink = limit > 0 && width.max(height) > limit;
    let mime = |format: ImageFormat| match format {
        ImageFormat::Png => "image/png",
        ImageFormat::WebP => "image/webp",
        _ => "image/jpeg",
    };
    if !shrink && target == source {
        return Some(RenderedCover {
            bytes: bytes.to_vec(),
            mime: mime(source).to_owned(),
            width,
            height,
        });
    }
    let mut picture = ImageReader::with_format(Cursor::new(bytes), source)
        .decode()
        .ok()?;
    if shrink {
        picture = picture.resize(limit, limit, FilterType::Lanczos3);
    }
    let mut out = Vec::new();
    match target {
        ImageFormat::Jpeg => {
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 90);
            picture.to_rgb8().write_with_encoder(encoder).ok()?;
        }
        other => picture
            .to_rgba8()
            .write_to(&mut Cursor::new(&mut out), other)
            .ok()?,
    }
    Some(RenderedCover {
        width: picture.width(),
        height: picture.height(),
        bytes: out,
        mime: mime(target).to_owned(),
    })
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
    pub lrclib: LrclibGate,
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
