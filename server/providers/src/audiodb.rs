//! TheAudioDB artwork client.
//!
//! Ports v2's AudioDB repository plus its wire models: artist and album
//! lookups by MusicBrainz id or by name, returning the artwork URLs
//! TheAudioDB knows. "No match" is `Ok(None)`; every failure is a typed
//! [`ProviderError`].
//!
//! The key rides in the path, and which rate row a call takes follows the
//! key the caller passes, read per call so a new key applies at once: the
//! free key (`123`, or none saved) paces at the documented 30 requests a
//! minute, any other key at the premium row. The free row is never raised:
//! the shared key is banned when over-used.

use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::RequestPriority;
use crate::degradation::{IntegrationStatus, record_current};
use crate::error::{MAX_RETRY_AFTER, ProviderError};
use crate::executor::{Backoff, Executor, Request, Spec, Url, standard_classify};

mod models;

use models::{AlbumAnswer, ArtistAnswer};
pub use models::{AudioDbAlbum, AudioDbArtist};

/// Default API host (without the key segment).
pub const DEFAULT_BASE_URL: &str = "https://www.theaudiodb.com/api/v1/json";
/// Free key, used when no settings key is configured.
pub const FREE_API_KEY: &str = "123";
/// The provider name errors carry, and the free key's rate row.
pub const SOURCE: &str = "audiodb";
/// The rate row a saved premium key paces at.
pub const PREMIUM_ROW: &str = "audiodb-premium";
/// Largest AudioDB answer read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// TheAudioDB on the wire: the standard rules, and a 429 that names no
/// wait rests a minute, as the free API page asks
/// (https://www.theaudiodb.com/free_music_api, read 2026-10-18).
pub static SPEC: Spec = Spec {
    source: SOURCE,
    health_key: SOURCE,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(60)),
    retry_after_cap: MAX_RETRY_AFTER,
    classify: standard_classify,
};

/// TheAudioDB client over its two executors, the free row and the premium
/// row. Holds no key; every call takes the saved one and names its
/// priority. Built once with the providers; cheap to clone.
#[derive(Debug, Clone)]
pub struct AudioDbClient {
    free: Executor,
    premium: Executor,
    base: Option<Url>,
}

impl AudioDbClient {
    /// The client sending through `free` and `premium` to the API root at
    /// `base_url`.
    #[must_use]
    pub fn new(free: Executor, premium: Executor, base_url: &str) -> Self {
        let base = Url::parse(base_url.trim())
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https") && !url.cannot_be_a_base());
        if base.is_none() {
            tracing::error!("the AudioDB address is not an http or https URL");
        }
        Self {
            free,
            premium,
            base,
        }
    }

    /// The executor a call with `api_key` goes through, and the key it
    /// sends: the free row for the free key or none, the premium row for
    /// any other.
    fn row_for<'a>(&'a self, api_key: &'a str) -> (&'a Executor, &'a str) {
        let key = api_key.trim();
        if key.is_empty() || key == FREE_API_KEY {
            (&self.free, FREE_API_KEY)
        } else {
            (&self.premium, key)
        }
    }

    /// Look an artist up by MusicBrainz id (`artist-mb.php`).
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn artist_by_mbid(
        &self,
        api_key: &str,
        mbid: &str,
        priority: RequestPriority,
    ) -> Result<Option<AudioDbArtist>, ProviderError> {
        if mbid.is_empty() {
            return Ok(None);
        }
        let answer = self
            .get::<ArtistAnswer>(api_key, "artist-mb.php", &[("i", mbid)], priority)
            .await?;
        Ok(answer.and_then(|answer| answer.artists.into_iter().next()))
    }

    /// Look an album up by MusicBrainz id (`album-mb.php`).
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn album_by_mbid(
        &self,
        api_key: &str,
        mbid: &str,
        priority: RequestPriority,
    ) -> Result<Option<AudioDbAlbum>, ProviderError> {
        if mbid.is_empty() {
            return Ok(None);
        }
        let answer = self
            .get::<AlbumAnswer>(api_key, "album-mb.php", &[("i", mbid)], priority)
            .await?;
        Ok(answer.and_then(|answer| answer.album.into_iter().next()))
    }

    /// Search for an artist by name (`search.php`).
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn search_artist(
        &self,
        api_key: &str,
        name: &str,
        priority: RequestPriority,
    ) -> Result<Option<AudioDbArtist>, ProviderError> {
        if name.is_empty() {
            return Ok(None);
        }
        let answer = self
            .get::<ArtistAnswer>(api_key, "search.php", &[("s", name)], priority)
            .await?;
        Ok(answer.and_then(|answer| answer.artists.into_iter().next()))
    }

    /// Search for an album by artist and title (`searchalbum.php`).
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn search_album(
        &self,
        api_key: &str,
        artist: &str,
        album: &str,
        priority: RequestPriority,
    ) -> Result<Option<AudioDbAlbum>, ProviderError> {
        if artist.is_empty() || album.is_empty() {
            return Ok(None);
        }
        let answer = self
            .get::<AlbumAnswer>(
                api_key,
                "searchalbum.php",
                &[("s", artist), ("a", album)],
                priority,
            )
            .await?;
        Ok(answer.and_then(|answer| answer.album.into_iter().next()))
    }

    /// One GET, decoded as `A`. A 404 is no match, never an error. An answer
    /// that does not decode is a payload failure, noted for the request.
    async fn get<A: DeserializeOwned>(
        &self,
        api_key: &str,
        endpoint: &str,
        query: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Option<A>, ProviderError> {
        let (executor, key) = self.row_for(api_key);
        let base = self.base.as_ref().ok_or_else(unusable_base)?;
        let request = Request::get(base, &[key, endpoint], query).map_err(|_| unusable_base())?;
        let reply = match executor.send(request, priority).await {
            Ok(reply) => reply,
            Err(ProviderError::NotFound { .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        reply
            .json::<A>()
            .map(Some)
            .inspect_err(|error| note_shape(&error.to_string()))
    }
}

/// An answer without the expected shape: the service answered, so it is
/// noted for the request but never counted as an outage.
fn note_shape(cause: &str) {
    tracing::debug!(
        provider = SOURCE,
        cause,
        "AudioDB answer had no usable shape"
    );
    record_current(SOURCE, IntegrationStatus::Error, true);
}

fn unusable_base() -> ProviderError {
    ProviderError::Invalid {
        provider: SOURCE,
        reason: "the AudioDB address is not usable",
    }
}
