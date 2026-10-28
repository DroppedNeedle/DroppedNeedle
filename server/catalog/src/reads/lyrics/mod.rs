//! Lyrics for the web player, the player apps and Library Management.
//!
//! [`LyricsService`] answers the player route and both compat APIs: the
//! lyrics a track's file carries first, then, while the lyrics switch is
//! on, the online answer. [`OnlineLyrics`] is that online answer, and
//! Library Management asks it directly: LRCLIB's answer kept in the
//! metadata cache (found seven days, missing six hours), else LRCLIB
//! through its executor. A 429 cools LRCLIB's limiter, and every caller
//! is held until the wait it named has passed. A lookup that failed
//! stores nothing.

use std::time::Duration;

use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::LyricsSettings;
use droppedneedle_library::tags::lyrics::parse_lyrics;
use droppedneedle_media::stream::local_files::LibraryFiles;
use droppedneedle_providers::degradation::{IntegrationStatus, record_current};
use droppedneedle_providers::lrclib::{LrclibClient, LyricsCandidate, signature_text};
use droppedneedle_providers::{
    CacheKey, Fetched, Policy, ProviderCacheStore, ProviderError, Providers, RequestPriority,
    Source,
};

use crate::reads::library::file_lyrics;
use crate::reads::library::store::LocalCatalog;
use crate::reads::library::stores::{BoxFuture, LyricDoc, LyricsPort, StoreError};

/// Source name for logs and the degradation registry.
const LRCLIB: &str = "lrclib";

/// How long LRCLIB's answer is kept: lyrics found a week, none six hours.
const LYRICS_POLICY: Policy = Policy {
    fresh: Duration::from_secs(7 * 24 * 3_600),
    miss: Duration::from_secs(6 * 3_600),
};

/// Why an online lookup gave no answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LyricsError {
    /// LRCLIB asked for a pause and it has not passed; nothing was sent.
    Resting,
    /// LRCLIB failed.
    Failed(ProviderError),
}

/// One track as LRCLIB matches it.
#[derive(Debug, Clone, Copy)]
pub struct LyricsQuery<'a> {
    /// Track title.
    pub title: &'a str,
    /// Track artist.
    pub artist: &'a str,
    /// Album title.
    pub album: &'a str,
    /// Track length in seconds.
    pub duration_secs: f64,
}

/// LRCLIB's answers, through the metadata cache.
#[derive(Debug, Clone)]
pub struct OnlineLyrics {
    lrclib: LrclibClient,
    cache: ProviderCacheStore,
    switch: Live<LyricsSettings>,
}

impl OnlineLyrics {
    /// LRCLIB through the providers' client and cache. `switch` is the
    /// player's lyrics setting, read per call by [`LyricsService`].
    #[must_use]
    pub fn new(providers: &Providers, switch: Live<LyricsSettings>) -> Self {
        Self {
            lrclib: providers.lrclib.clone(),
            cache: providers.cache.clone(),
            switch,
        }
    }

    /// Whether the player's lyrics switch is on now. Off when the section
    /// cannot be read.
    fn switched_on(&self) -> bool {
        self.switch
            .get()
            .map(|settings| settings.enabled)
            .unwrap_or_else(|error| {
                tracing::warn!(%error, "cannot read lyrics settings; online lyrics off");
                false
            })
    }

    /// LRCLIB's exact match for `query` at `priority`. `None` when LRCLIB
    /// has none, and without asking when the length rounds to nothing or
    /// the title or artist is blank: the length is a required match key.
    ///
    /// # Errors
    ///
    /// [`LyricsError`] when LRCLIB is resting or failed and no kept answer
    /// stands in.
    pub async fn lookup(
        &self,
        query: LyricsQuery<'_>,
        priority: RequestPriority,
    ) -> Result<Option<LyricsCandidate>, LyricsError> {
        let duration = query.duration_secs.round();
        if !duration.is_finite()
            || duration < 1.0
            || query.title.trim().is_empty()
            || query.artist.trim().is_empty()
        {
            return Ok(None);
        }
        let duration = duration as u32;
        let key = CacheKey::hashed(
            Source::Lrclib,
            "get",
            &[
                &signature_text(query.title),
                &signature_text(query.artist),
                &signature_text(query.album),
                &duration.to_string(),
            ],
        );
        let client = self.lrclib.clone();
        let (title, artist, album) = (
            query.title.to_owned(),
            query.artist.to_owned(),
            query.album.to_owned(),
        );
        self.cache
            .get_or_fetch(&key, LYRICS_POLICY, priority, move |priority| async move {
                // A resting LRCLIB is not waited out.
                if !client.limiter().cooling_for().is_zero() {
                    return Err(LyricsError::Resting);
                }
                match client
                    .get_exact_lyrics(&title, &artist, &album, duration, priority)
                    .await
                {
                    Ok(found) => Ok(match found.candidate.filter(|_| found.found) {
                        Some(candidate) => Fetched::Found(candidate),
                        None => Fetched::Missing,
                    }),
                    Err(ProviderError::RateLimited { .. }) => Err(LyricsError::Resting),
                    Err(error) => Err(LyricsError::Failed(error)),
                }
            })
            .await
    }
}

/// The lyrics behind the player route and the compat APIs.
///
/// A track the lyrics sweep marked is read from its file. Otherwise, while
/// the switch is on, the catalog's title, artist, album and length are
/// looked up online at user priority. A failed or resting lookup reads as
/// absent with the cause logged; a database error is an error.
pub struct LyricsService {
    catalog: LocalCatalog,
    files: LibraryFiles,
    online: Option<OnlineLyrics>,
}

impl LyricsService {
    /// File lyrics over `catalog` and `files`, then `online` when given.
    #[must_use]
    pub fn new(catalog: LocalCatalog, files: LibraryFiles, online: Option<OnlineLyrics>) -> Self {
        Self {
            catalog,
            files,
            online,
        }
    }

    async fn online(
        &self,
        online: &OnlineLyrics,
        track_id: &str,
    ) -> Result<Option<LyricDoc>, StoreError> {
        let track = self
            .catalog
            .get_track(track_id)
            .await
            .map_err(|error| StoreError::Internal(error.to_string()))?;
        let Some(track) = track else {
            return Ok(None);
        };
        let Some(duration_secs) = track.duration_seconds else {
            return Ok(None);
        };
        let query = LyricsQuery {
            title: &track.title,
            artist: &track.artist_name,
            album: &track.album_title,
            duration_secs,
        };
        let candidate = match online.lookup(query, RequestPriority::UserInitiated).await {
            Ok(Some(candidate)) => candidate,
            Ok(None) => return Ok(None),
            Err(error) => {
                tracing::debug!(?error, "online lyrics degraded to absence");
                record_current(LRCLIB, IntegrationStatus::Error, false);
                return Ok(None);
            }
        };
        match parse_lyrics(
            candidate.plain_lyrics.as_deref(),
            candidate.synced_lyrics.as_deref(),
        ) {
            Ok(parsed) => Ok(parsed.map(|parsed| LyricDoc {
                lines: parsed.lines,
                synced: parsed.synced,
            })),
            Err(refused) => {
                tracing::debug!(cause = %refused, "online lyrics unusable; reading as absent");
                record_current(LRCLIB, IntegrationStatus::Error, false);
                Ok(None)
            }
        }
    }
}

impl LyricsPort for LyricsService {
    fn get<'a>(&'a self, track_id: &'a str) -> BoxFuture<'a, Result<Option<LyricDoc>, StoreError>> {
        Box::pin(async move {
            if let Some(doc) = file_lyrics::read(&self.catalog, &self.files, track_id).await? {
                return Ok(Some(doc));
            }
            match &self.online {
                Some(online) if online.switched_on() => self.online(online, track_id).await,
                _ => Ok(None),
            }
        })
    }
}
