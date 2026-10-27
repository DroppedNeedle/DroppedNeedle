//! Provider lyrics: LRCLIB behind the reads [`LyricsPort`] seam, for
//! tracks whose files carry no lyrics. A failed or slow lookup reads as
//! absent with the cause logged; lyrics never fail a request.
//!
//! [`live`] holds the production LRCLIB role and the search counts pair
//! built at boot; [`library`] is Library Management's enrichment port.

pub mod library;
pub mod live;

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use crate::reads::library::store::LocalCatalog;
use crate::reads::library::stores::{LyricDoc, LyricsPort, StoreError};
use droppedneedle_library::tags::lyrics;
use droppedneedle_providers::TransportKind;
use droppedneedle_providers::degradation::{IntegrationStatus as CoreStatus, record_current};

/// Boxed future for dyn-compatible client methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A failed lookup: the provider's own typed failure. It is logged and
/// read as absence.
pub use droppedneedle_providers::ProviderError;

/// Source name for logs and the degradation registry.
const LRCLIB: &str = "lrclib";
/// Longest wait for one lookup, kept from v2 (`_METADATA_TIMEOUT`).
const LOOKUP_TIMEOUT: Duration = Duration::from_secs(30);

/// One lyrics lookup.
#[derive(Debug, Clone)]
pub struct LyricsQuery {
    /// Track artist name.
    pub artist: String,
    /// Track title.
    pub title: String,
    /// Album title, when known.
    pub album: Option<String>,
    /// Track length in seconds, when known.
    pub duration_secs: Option<f64>,
}

/// LRCLIB lyrics outcome. `found` false means the provider has no lyrics
/// for the track: absence, never failure.
#[derive(Debug, Clone)]
pub struct LyricsLookup {
    /// True when the provider holds lyrics for the track.
    pub found: bool,
    /// Plain lyrics, when held.
    pub plain: Option<String>,
    /// Synced (LRC) lyrics, when held.
    pub synced: Option<String>,
}

/// LRCLIB exact lyrics lookups.
pub trait LyricsClient: Send + Sync {
    /// Exact lyrics for one track.
    fn exact_lyrics<'a>(
        &'a self,
        query: &'a LyricsQuery,
    ) -> BoxFuture<'a, Result<LyricsLookup, ProviderError>>;
}

/// Run one lookup under [`LOOKUP_TIMEOUT`]. A timeout becomes a provider
/// failure like any other.
async fn within_timeout<T>(
    leg: impl Future<Output = Result<T, ProviderError>>,
) -> Result<T, ProviderError> {
    match tokio::time::timeout(LOOKUP_TIMEOUT, leg).await {
        Ok(outcome) => outcome,
        Err(_) => Err(ProviderError::Transport {
            provider: LRCLIB,
            kind: TransportKind::Timeout,
        }),
    }
}

/// Shape an LRCLIB lookup into a lyrics document through the library's
/// lyrics parser. Oversized payloads fail so the caller degrades;
/// empty-but-found payloads read as absence.
fn lyric_doc(lookup: &LyricsLookup) -> Result<Option<LyricDoc>, String> {
    let parsed = lyrics::parse_lyrics(lookup.plain.as_deref(), lookup.synced.as_deref())
        .map_err(|refused| refused.to_string())?;
    Ok(parsed.map(|parsed| LyricDoc {
        lines: parsed.lines,
        synced: parsed.synced,
    }))
}

/// Provider lyrics behind the reads [`LyricsPort`] seam.
///
/// The port only carries a track id, so the adapter resolves artist and
/// title through the catalog first: an unknown track reads as absent, and
/// a dead lyrics provider also reads as absent, with the cause logged. Production wires this
/// in `ReadsSetup::build` over live LRCLIB; without the pair, file lyrics
/// fall back to `NoOnlineLyrics`.
pub struct ProviderLyrics {
    catalog: LocalCatalog,
    lyrics: Arc<dyn LyricsClient>,
    settings: Option<
        droppedneedle_kernel::runtime_config::Live<
            droppedneedle_kernel::runtime_config::sections::LyricsSettings,
        >,
    >,
}

impl ProviderLyrics {
    /// Fetch provider lyrics for catalog tracks.
    pub fn new(catalog: LocalCatalog, lyrics: Arc<dyn LyricsClient>) -> Self {
        Self {
            catalog,
            lyrics,
            settings: None,
        }
    }

    /// Serve only while the lyrics setting is on (read per call);
    /// otherwise lyrics read as absent without a catalog read or a
    /// provider call.
    #[must_use]
    pub fn with_settings(
        mut self,
        settings: droppedneedle_kernel::runtime_config::Live<
            droppedneedle_kernel::runtime_config::sections::LyricsSettings,
        >,
    ) -> Self {
        self.settings = Some(settings);
        self
    }
}

impl LyricsPort for ProviderLyrics {
    fn get<'a>(
        &'a self,
        track_id: &'a str,
    ) -> crate::reads::library::stores::BoxFuture<'a, Result<Option<LyricDoc>, StoreError>> {
        Box::pin(async move {
            if self
                .settings
                .as_ref()
                .is_some_and(|settings| !live::lyrics_on(settings))
            {
                return Ok(None);
            }
            let track = self
                .catalog
                .get_track(track_id)
                .await
                .map_err(|error| StoreError::Internal(error.to_string()))?;
            let Some(track) = track else {
                return Ok(None);
            };
            let query = LyricsQuery {
                artist: track.artist_name.clone(),
                title: track.title.clone(),
                album: Some(track.album_title.clone()),
                duration_secs: track.duration_seconds,
            };
            let lookup = match within_timeout(self.lyrics.exact_lyrics(&query)).await {
                Ok(lookup) => lookup,
                Err(error) => {
                    tracing::debug!(
                        source = error.source(),
                        cause = %error,
                        "provider lyrics degraded to absence",
                    );
                    record_current(LRCLIB, CoreStatus::Error, false);
                    return Ok(None);
                }
            };
            if !lookup.found {
                return Ok(None);
            }
            match lyric_doc(&lookup) {
                Ok(doc) => Ok(doc),
                Err(cause) => {
                    tracing::debug!(cause = %cause, "provider lyrics unusable; reading as absent");
                    record_current(LRCLIB, CoreStatus::Error, false);
                    Ok(None)
                }
            }
        })
    }
}
