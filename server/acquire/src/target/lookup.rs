//! What acquisition asks MusicBrainz: which releases carry a recording,
//! the tracklist of one release, and when a release group first came out. [`AlbumLookup`] is the port;
//! [`MusicBrainzAlbums`] answers it over the shared client. Lookups are
//! identity critical: an outage is an error, never an empty answer that
//! would read as "no album".

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};

use droppedneedle_providers::musicbrainz::{
    Criticality, LiveSource, MbRelease, MusicBrainzView, is_valid_mbid, parse_year,
};
use droppedneedle_providers::{
    CacheKey, Fetched, Policy, ProviderCacheStore, RequestPriority, Source,
};

/// Various Artists: a compilation credit that no Soulseek path carries.
pub const VARIOUS_ARTISTS_MBID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

/// What the one release lookup of a task asks for: the tracklist with
/// its recordings' ISRCs, the credits and the group. The barcode comes
/// with every release lookup.
const RELEASE_INCLUDES: [&str; 4] = ["artist-credits", "isrcs", "recordings", "release-groups"];

/// Medium formats that carry video, not downloadable audio.
/// DVD-Audio stays.
const VIDEO_FORMATS: [&str; 12] = [
    "dvd",
    "dvd-video",
    "blu-ray",
    "hd-dvd",
    "hdv",
    "vhs",
    "betamax",
    "video cd",
    "video-cd",
    "vcd",
    "svcd",
    "laserdisc",
];

/// Whether a medium format carries video, not downloadable audio.
pub fn is_video_format(format: &str) -> bool {
    VIDEO_FORMATS.contains(&format.trim().to_ascii_lowercase().as_str())
}

/// One release carrying a recording, with the fields release choice ranks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseCandidate {
    /// Release MBID, lowercase.
    pub id: String,
    /// `Official`, `Bootleg`, ...
    pub status: Option<String>,
    /// Release date.
    pub date: Option<String>,
    /// Release-group MBID, lowercase.
    pub release_group_mbid: String,
    /// Group primary type (`Album`, `Single`, ...).
    pub primary_type: Option<String>,
    /// Group secondary types (`Compilation`, `Live`, ...).
    pub secondary_types: Vec<String>,
}

/// One audio track of a release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumTrack {
    /// Disc number (1-based).
    pub disc: u32,
    /// Position on the disc (1-based).
    pub position: u32,
    /// Track title on this release.
    pub title: String,
    /// Length in seconds.
    pub duration_seconds: Option<f64>,
    /// Recording MBID, lowercase.
    pub recording_mbid: String,
    /// Release-track MBID, lowercase.
    pub release_track_mbid: String,
    /// The recording's ISRCs, uppercase.
    pub isrcs: Vec<String>,
}

/// One release with its audio tracklist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AlbumRelease {
    /// Release MBID, lowercase.
    pub id: String,
    /// Release title.
    pub title: String,
    /// Release-group MBID, lowercase.
    pub release_group_mbid: String,
    /// Credited release artist.
    pub artist: String,
    /// Whether the release is credited to Various Artists.
    pub various_artists: bool,
    /// Year of the group's first release, else of this release.
    pub year: Option<i32>,
    /// The release's barcode, trimmed; `None` when it has none.
    pub barcode: Option<String>,
    /// Audio tracks in disc and position order.
    pub tracks: Vec<AlbumTrack>,
}

/// MusicBrainz could not answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("musicbrainz lookup failed: {0}")]
pub struct LookupError(pub String);

/// The MusicBrainz reads acquisition needs.
pub trait AlbumLookup: Send + Sync {
    /// Releases carrying one recording; `None` when MusicBrainz does not
    /// know the recording.
    fn recording_releases<'a>(
        &'a self,
        recording_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<ReleaseCandidate>>, LookupError>>;

    /// One release with its audio tracklist; `None` when unknown.
    fn release<'a>(
        &'a self,
        release_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<AlbumRelease>, LookupError>>;

    /// A release group's first release date as MusicBrainz gives it
    /// (`YYYY`, `YYYY-MM` or `YYYY-MM-DD`); `None` when the group is
    /// unknown or undated.
    fn first_release_date<'a>(
        &'a self,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, LookupError>>;
}

/// [`AlbumLookup`] over the MusicBrainz client, cached in `ProviderCacheStore`.
pub struct MusicBrainzAlbums {
    client: MusicBrainzView,
    cache: ProviderCacheStore,
}

impl MusicBrainzAlbums {
    /// Lookups over one client backed by `cache`.
    pub fn new(client: MusicBrainzView, cache: ProviderCacheStore) -> Self {
        Self { client, cache }
    }
}

impl AlbumLookup for MusicBrainzAlbums {
    fn recording_releases<'a>(
        &'a self,
        recording_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<ReleaseCandidate>>, LookupError>> {
        Box::pin(async move {
            let asked = recording_mbid.trim().to_ascii_lowercase();
            let client = self.client.clone();
            let asked_fetch = asked.clone();
            let fetch = move || {
                let client = client.clone();
                let asked_fetch = asked_fetch.clone();
                async move {
                    let found = client
                        .lookup_recording(
                            &asked_fetch,
                            &["releases", "release-groups"],
                            Criticality::IdentityCritical,
                        )
                        .await
                        .map_err(|error| LookupError(error.to_string()))?;
                    Ok::<_, LookupError>(found.map(|lookup| {
                        lookup
                            .entity
                            .releases
                            .into_iter()
                            .filter_map(|release| {
                                let group = release.release_group?;
                                Some(ReleaseCandidate {
                                    id: release.id.to_ascii_lowercase(),
                                    status: release.status,
                                    date: release.date,
                                    release_group_mbid: group.id.to_ascii_lowercase(),
                                    primary_type: group.primary_type,
                                    secondary_types: group.secondary_types,
                                })
                            })
                            .collect::<Vec<_>>()
                    }))
                }
            };
            let Some(namespace) = self
                .client
                .cache_namespace()
                .filter(|_| is_valid_mbid(&asked))
            else {
                return fetch().await;
            };
            let key = CacheKey::new(
                Source::MusicBrainz,
                &[&namespace, "acquire_rec_releases", &asked],
            );
            self.cache
                .get_or_fetch(
                    &key,
                    Policy::MUSICBRAINZ_ENTITY,
                    RequestPriority::BackgroundSync,
                    move |_| async move {
                        Ok(match fetch().await? {
                            Some(releases) => Fetched::Found(releases),
                            None => Fetched::Missing,
                        })
                    },
                )
                .await
        })
    }

    fn release<'a>(
        &'a self,
        release_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<AlbumRelease>, LookupError>> {
        Box::pin(async move {
            let asked = release_mbid.trim().to_ascii_lowercase();
            let client = self.client.clone();
            let asked_fetch = asked.clone();
            let fetch = move || {
                let client = client.clone();
                let asked_fetch = asked_fetch.clone();
                async move {
                    let found = client
                        .lookup_exact_release(
                            &asked_fetch,
                            &RELEASE_INCLUDES,
                            Criticality::IdentityCritical,
                        )
                        .await
                        .map_err(|error| LookupError(error.to_string()))?;
                    Ok::<_, LookupError>(found.and_then(|lookup| album_release(lookup.entity)))
                }
            };
            let Some(namespace) = self
                .client
                .cache_namespace()
                .filter(|_| is_valid_mbid(&asked))
            else {
                return fetch().await;
            };
            let key = CacheKey::new(
                Source::MusicBrainz,
                &[&namespace, "acquire_release", &asked],
            );
            self.cache
                .get_or_fetch(
                    &key,
                    Policy::MUSICBRAINZ_ENTITY,
                    RequestPriority::BackgroundSync,
                    move |_| async move {
                        Ok(match fetch().await? {
                            Some(release) => Fetched::Found(release),
                            None => Fetched::Missing,
                        })
                    },
                )
                .await
        })
    }

    fn first_release_date<'a>(
        &'a self,
        release_group_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, LookupError>> {
        Box::pin(async move {
            let asked = release_group_mbid.trim().to_ascii_lowercase();
            let client = self.client.clone();
            let asked_fetch = asked.clone();
            let fetch = move || {
                let client = client.clone();
                let asked_fetch = asked_fetch.clone();
                async move {
                    let found = client
                        .lookup_release_group(&asked_fetch, &[], Criticality::IdentityCritical)
                        .await
                        .map_err(|error| LookupError(error.to_string()))?;
                    Ok::<_, LookupError>(
                        found
                            .and_then(|lookup| lookup.entity.first_release_date)
                            .filter(|date| !date.trim().is_empty()),
                    )
                }
            };
            let Some(namespace) = self
                .client
                .cache_namespace()
                .filter(|_| is_valid_mbid(&asked))
            else {
                return fetch().await;
            };
            let key = CacheKey::new(
                Source::MusicBrainz,
                &[&namespace, "acquire_rg_date", &asked],
            );
            self.cache
                .get_or_fetch(
                    &key,
                    Policy::MUSICBRAINZ_ENTITY,
                    RequestPriority::BackgroundSync,
                    move |_| async move {
                        Ok(match fetch().await? {
                            Some(date) => Fetched::Found(date),
                            None => Fetched::Missing,
                        })
                    },
                )
                .await
        })
    }
}

/// The production lookups: background priority on the shared limiter,
/// the configured MusicBrainz source read per request, failures counted
/// toward system health.
pub fn live_albums(
    providers: &droppedneedle_providers::Providers,
    source: LiveSource,
) -> MusicBrainzAlbums {
    MusicBrainzAlbums::new(
        providers
            .musicbrainz
            .live(source, RequestPriority::BackgroundSync),
        providers.cache.clone(),
    )
}

/// The wire release as acquisition reads it: audio media only, tracks
/// without a recording or a position dropped.
pub fn album_release(release: MbRelease) -> Option<AlbumRelease> {
    let group = release.release_group?;
    let various_artists = release
        .artist_credit
        .iter()
        .any(|credit| credit.artist.id.eq_ignore_ascii_case(VARIOUS_ARTISTS_MBID));
    let artist = release
        .artist_credit
        .iter()
        .map(|credit| {
            let name = if credit.name.is_empty() {
                credit.artist.name.as_str()
            } else {
                credit.name.as_str()
            };
            format!("{name}{}", credit.joinphrase)
        })
        .collect::<String>()
        .trim()
        .to_owned();
    let mut tracks = Vec::new();
    for (index, medium) in release.media.iter().enumerate() {
        if medium.format.as_deref().is_some_and(is_video_format) {
            continue;
        }
        let disc = medium
            .position
            .unwrap_or_else(|| u32::try_from(index + 1).unwrap_or(1));
        for track in &medium.tracks {
            let (Some(position), Some(recording)) = (track.position, track.recording.as_ref())
            else {
                continue;
            };
            tracks.push(AlbumTrack {
                disc,
                position,
                title: track.display_title().unwrap_or_default().to_owned(),
                duration_seconds: track.length_ms().map(|ms| ms as f64 / 1000.0),
                recording_mbid: recording.id.to_ascii_lowercase(),
                release_track_mbid: track.id.to_ascii_lowercase(),
                isrcs: recording
                    .isrcs
                    .iter()
                    .map(|isrc| isrc.trim().to_ascii_uppercase())
                    .filter(|isrc| !isrc.is_empty())
                    .collect(),
            });
        }
    }
    Some(AlbumRelease {
        id: release.id.to_ascii_lowercase(),
        title: release
            .title
            .unwrap_or_else(|| group.title.clone().unwrap_or_default()),
        release_group_mbid: group.id.to_ascii_lowercase(),
        artist,
        various_artists,
        year: parse_year(group.first_release_date.as_deref())
            .or_else(|| parse_year(release.date.as_deref())),
        barcode: release
            .barcode
            .map(|barcode| barcode.trim().to_owned())
            .filter(|barcode| !barcode.is_empty()),
        tracks,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The recorded release lookup (its include set covers ours) gives
    /// the release's barcode and every track's ISRCs, which the plan keeps.
    #[test]
    fn a_release_lookup_keeps_the_barcode_and_each_tracks_isrcs() {
        assert!(
            RELEASE_INCLUDES.contains(&"isrcs"),
            "the one release lookup asks for the ISRCs it maps"
        );
        let wire: MbRelease = serde_json::from_str(include_str!(
            "../../../tests/fixtures/musicbrainz/release_lookup_classical.json"
        ))
        .expect("the recorded release decodes");
        let release = album_release(wire).expect("the release has its group");
        assert_eq!(release.barcode.as_deref(), Some("888880520562"));
        let isrcs: Vec<Vec<&str>> = release
            .tracks
            .iter()
            .map(|track| track.isrcs.iter().map(String::as_str).collect())
            .collect();
        assert_eq!(
            isrcs,
            [
                vec!["DEE860800220"],
                vec!["DEE860800221", "DEE860800222"],
                vec![],
                vec!["DEE860800224"],
                vec!["DEE860800225"],
                vec!["DEE860800226"],
                vec!["DEE860800262"],
            ]
        );
    }

    #[tokio::test]
    async fn musicbrainz_albums_reuses_provider_cache() {
        use droppedneedle_providers::musicbrainz::MbSource;
        use droppedneedle_providers::test_support::ScriptedWire;
        use std::sync::Arc;

        const MB: &str = "http://mb.test/ws/2";
        const REC: &str = "11111111-1111-4111-8111-111111111111";
        const REL: &str = "22222222-2222-4222-8222-222222222222";
        const RG: &str = "33333333-3333-4333-8333-333333333333";

        let wire = Arc::new(ScriptedWire::new());
        let json = [("content-type", "application/json")];
        wire.answer(
            &format!("{MB}/recording/{REC}"),
            200,
            &json,
            format!(
                r#"{{"id":"{REC}","title":"Track","releases":[{{"id":"{REL}","status":"Official","date":"2020-01-01","release-group":{{"id":"{RG}","primary-type":"Album","secondary-types":[]}}}}]}}"#
            ),
        );
        wire.answer(
            &format!("{MB}/release/{REL}"),
            200,
            &json,
            format!(
                r#"{{"id":"{REL}","title":"Album","date":"2020-01-01","artist-credit":[{{"name":"Band","joinphrase":"","artist":{{"id":"44444444-4444-4444-8444-444444444444","name":"Band"}}}}],"release-group":{{"id":"{RG}","title":"Album","first-release-date":"2020-01-01"}},"media":[{{"position":1,"tracks":[{{"id":"55555555-5555-4555-8555-555555555555","position":1,"title":"Track","recording":{{"id":"{REC}","title":"Track","isrcs":[]}}}}]}}]}}"#
            ),
        );
        wire.answer(
            &format!("{MB}/release-group/{RG}"),
            200,
            &json,
            format!(r#"{{"id":"{RG}","title":"Album","first-release-date":"2020-01-01"}}"#),
        );
        let providers = droppedneedle_providers::Providers::unpaced(wire.clone());
        let lookup = MusicBrainzAlbums::new(
            providers.musicbrainz.fixed(
                MbSource::Mirror {
                    base_url: MB.to_owned(),
                    rate_per_sec: 0.0,
                },
                RequestPriority::BackgroundSync,
            ),
            providers.cache.clone(),
        );

        let r1 = lookup.recording_releases(REC).await.unwrap().unwrap();
        let r2 = lookup.recording_releases(REC).await.unwrap().unwrap();
        assert_eq!(r1, r2);

        let a1 = lookup.release(REL).await.unwrap().unwrap();
        let a2 = lookup.release(REL).await.unwrap().unwrap();
        assert_eq!(a1, a2);

        let d1 = lookup.first_release_date(RG).await.unwrap();
        let d2 = lookup.first_release_date(RG).await.unwrap();
        assert_eq!(d1.as_deref(), Some("2020-01-01"));
        assert_eq!(d1, d2);

        assert_eq!(
            wire.sent().len(),
            3,
            "repeat recording_releases, release, and first_release_date calls must hit cache"
        );
    }
}
