//! Which MusicBrainz release group a Spotify album is.
//!
//! An album is matched only on proof, never on its name: the release
//! search by title and artist only finds candidates, and a candidate
//! counts when one of its tracks is a recording carrying an ISRC that
//! Spotify gave for a track of that album. One proven release group is the
//! answer. No ISRC, no proven candidate, or two proven groups (an album
//! and a same-named single carrying the same recording) leave the album
//! unresolved, so an import never points a request at the wrong music.
//!
//! The client has no ISRC lookup, so the proof is read from the candidate
//! release (`inc=recordings+isrcs`), which also keeps the calls to what the
//! album needs. Lookups run at background priority, paced by the
//! executor, and each album's answer is kept in the metadata cache.

use std::collections::BTreeSet;

use droppedneedle_kernel::runtime_config::{Live, sections::MusicBrainzSettings};
use droppedneedle_providers::cache::{CacheKey, Fetched, Policy, ProviderCacheStore, Source};
use droppedneedle_providers::musicbrainz::{
    Criticality, MusicBrainzClient, MusicBrainzView, SOURCE, is_valid_mbid, normalize_mb_id,
};
use droppedneedle_providers::{ProviderError, Providers, RequestPriority};

/// Releases read from one search. Editions of a proven group cost no
/// lookup, so this bounds the lookups to the distinct candidates.
const SEARCH_LIMIT: u32 = 5;

/// The includes a candidate is proven with.
const PROOF_INCLUDES: [&str; 3] = ["isrcs", "recordings", "release-groups"];

/// One Spotify album as the import knows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpotifyAlbum {
    /// Album title.
    pub title: String,
    /// The first credited artist of a track on it.
    pub artist: String,
    /// The ISRCs of its tracks on the playlist, uppercased and well formed.
    pub isrcs: BTreeSet<String>,
}

impl SpotifyAlbum {
    /// The album with `isrc` added when it is a well-formed ISRC.
    pub fn add_isrc(&mut self, isrc: &str) {
        let isrc = isrc.trim().to_ascii_uppercase();
        if is_isrc(&isrc) {
            self.isrcs.insert(isrc);
        }
    }
}

/// Finds the release group of Spotify albums through MusicBrainz.
#[derive(Clone)]
pub struct AlbumResolver {
    musicbrainz: MusicBrainzClient,
    settings: Live<MusicBrainzSettings>,
    cache: ProviderCacheStore,
}

impl AlbumResolver {
    /// The resolver over the shared providers, reading the MusicBrainz
    /// source from `settings` at each album.
    pub fn new(providers: &Providers, settings: Live<MusicBrainzSettings>) -> Self {
        Self {
            musicbrainz: providers.musicbrainz.clone(),
            settings,
            cache: providers.cache.clone(),
        }
    }

    /// The album's release group id, `None` when nothing proves one.
    ///
    /// # Errors
    ///
    /// A MusicBrainz failure, so the caller can stop asking while the
    /// service is down. Nothing is cached for it.
    pub async fn resolve(&self, album: &SpotifyAlbum) -> Result<Option<String>, ProviderError> {
        if album.isrcs.is_empty() || album.title.trim().is_empty() {
            return Ok(None);
        }
        let Some(settings) = self.settings.read() else {
            return Err(ProviderError::NotConfigured {
                provider: SOURCE,
                message: "the musicbrainz settings cannot be read".to_owned(),
            });
        };
        let namespace = format!("{}:g{}", settings.source_id, settings.generation);
        let view = self.musicbrainz.fixed(
            self.musicbrainz.source_from(&settings),
            RequestPriority::BackgroundSync,
        );
        let isrcs: Vec<&str> = album.isrcs.iter().map(String::as_str).collect();
        let joined = isrcs.join(",");
        let key = CacheKey::hashed(
            Source::MusicBrainz,
            "spotify_album",
            &[&namespace, &album.title, &album.artist, &joined],
        );
        let album = album.clone();
        self.cache
            .get_or_fetch(
                &key,
                Policy::RELEASE_TO_GROUP,
                RequestPriority::BackgroundSync,
                move |_| async move {
                    Ok(match prove(&view, &album).await? {
                        Some(group) => Fetched::Found(group),
                        None => Fetched::Missing,
                    })
                },
            )
            .await
    }
}

/// Search each title form until one proves exactly one release group.
async fn prove(
    view: &MusicBrainzView,
    album: &SpotifyAlbum,
) -> Result<Option<String>, ProviderError> {
    for title in title_forms(&album.title) {
        let page = view
            .search_releases(
                &title,
                &album.artist,
                SEARCH_LIMIT,
                Criticality::IdentityCritical,
            )
            .await?;
        let mut proven: BTreeSet<String> = BTreeSet::new();
        for hit in page.items {
            let listed = hit
                .release_group
                .as_ref()
                .map(|group| normalize_mb_id(&group.id));
            if listed.as_ref().is_some_and(|group| proven.contains(group)) {
                continue;
            }
            let Some(release) = view
                .lookup_release(&hit.id, &PROOF_INCLUDES, Criticality::IdentityCritical)
                .await?
            else {
                continue;
            };
            let release = release.entity;
            let Some(group) = release
                .release_group
                .as_ref()
                .map(|group| normalize_mb_id(&group.id))
                .filter(|group| is_valid_mbid(group))
            else {
                continue;
            };
            let carries = release
                .media
                .iter()
                .flat_map(|medium| &medium.tracks)
                .filter_map(|track| track.recording.as_ref())
                .flat_map(|recording| &recording.isrcs)
                .any(|isrc| album.isrcs.contains(&isrc.trim().to_ascii_uppercase()));
            if carries {
                proven.insert(group);
            }
        }
        match proven.len() {
            0 => {}
            1 => return Ok(proven.pop_first()),
            _ => {
                tracing::debug!(
                    groups = proven.len(),
                    "spotify album matches more than one release group; left unresolved"
                );
                return Ok(None);
            }
        }
    }
    Ok(None)
}

/// The title as Spotify gives it, then without a trailing edition note
/// (`Abbey Road (Remastered)`, `Glass Tides - Single`), which MusicBrainz
/// keeps out of the title.
fn title_forms(title: &str) -> Vec<String> {
    let title = title.trim();
    let mut forms = vec![title.to_owned()];
    let base = if let Some(stripped) = title
        .strip_suffix(')')
        .and_then(|rest| rest.rfind('(').map(|at| &rest[..at]))
    {
        stripped
    } else if let Some(stripped) = title
        .strip_suffix(']')
        .and_then(|rest| rest.rfind('[').map(|at| &rest[..at]))
    {
        stripped
    } else if let Some(at) = title.rfind(" - ") {
        &title[..at]
    } else {
        title
    };
    let base = base.trim();
    if !base.is_empty() && base != title {
        forms.push(base.to_owned());
    }
    forms
}

/// `CC XXX YY NNNNN`: two letters, three letters or digits, then seven
/// digits.
fn is_isrc(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 12
        && bytes[..2].iter().all(u8::is_ascii_uppercase)
        && bytes[2..5]
            .iter()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        && bytes[5..].iter().all(u8::is_ascii_digit)
}
