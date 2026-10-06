//! What acquisition asks MusicBrainz: which releases carry a recording,
//! and the tracklist of one release. [`AlbumLookup`] is the port;
//! [`MusicBrainzAlbums`] answers it over the shared client. Lookups are
//! identity critical: an outage is an error, never an empty answer that
//! would read as "no album".

use futures_util::future::BoxFuture;

use crate::providers::degradation::DegradationSink;
use crate::providers::musicbrainz::{
    Criticality, MbRelease, MbTransport, MusicBrainzClient, parse_year,
};

/// Various Artists: a compilation credit that no Soulseek path carries.
pub const VARIOUS_ARTISTS_MBID: &str = "89ad4ac3-39f7-470e-963a-56509c546377";

/// Medium formats that carry video, not downloadable audio (v2
/// `_VIDEO_MEDIUM_FORMATS`). DVD-Audio stays.
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

/// One release carrying a recording, with the fields release choice ranks.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq)]
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
}

/// One release with its audio tracklist.
#[derive(Debug, Clone, PartialEq)]
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
}

/// [`AlbumLookup`] over the MusicBrainz client.
pub struct MusicBrainzAlbums<T: MbTransport, S: DegradationSink> {
    client: MusicBrainzClient<T, S>,
}

impl<T: MbTransport, S: DegradationSink> MusicBrainzAlbums<T, S> {
    /// Lookups over one client.
    pub fn new(client: MusicBrainzClient<T, S>) -> Self {
        Self { client }
    }
}

impl<T: MbTransport, S: DegradationSink> AlbumLookup for MusicBrainzAlbums<T, S> {
    fn recording_releases<'a>(
        &'a self,
        recording_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<ReleaseCandidate>>, LookupError>> {
        Box::pin(async move {
            let found = self
                .client
                .lookup_recording(
                    recording_mbid,
                    &["releases", "release-groups"],
                    Criticality::IdentityCritical,
                )
                .await
                .map_err(|error| LookupError(error.to_string()))?;
            Ok(found.map(|lookup| {
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
                    .collect()
            }))
        })
    }

    fn release<'a>(
        &'a self,
        release_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<AlbumRelease>, LookupError>> {
        Box::pin(async move {
            let found = self
                .client
                .lookup_exact_release(release_mbid, Criticality::IdentityCritical)
                .await
                .map_err(|error| LookupError(error.to_string()))?;
            Ok(found.and_then(|lookup| album_release(lookup.entity)))
        })
    }
}

/// The production lookups: background priority on the shared limiter,
/// the configured MusicBrainz source read per request, failures counted
/// toward system health.
pub fn live_albums(
    http: &crate::http_client::HttpClientFactory,
    providers: std::sync::Arc<crate::providers::Providers>,
    source: crate::providers::musicbrainz::SourceFn,
) -> MusicBrainzAlbums<
    crate::providers::musicbrainz::ReqwestMbTransport,
    crate::providers::adapters::HealthSink,
> {
    use crate::providers::RequestPriority;
    use crate::providers::adapters::HealthSink;
    use crate::providers::musicbrainz::{MbPacing, ReqwestMbTransport};

    let health = HealthSink::new(&providers);
    MusicBrainzAlbums::new(
        MusicBrainzClient::official(
            ReqwestMbTransport::new(http.no_redirect().clone()),
            MbPacing::new(providers),
        )
        .with_source_fn(source)
        .with_priority(RequestPriority::BackgroundSync)
        .with_sink(health),
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
        let video = medium.format.as_deref().is_some_and(|format| {
            VIDEO_FORMATS.contains(&format.trim().to_ascii_lowercase().as_str())
        });
        if video {
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
        tracks,
    })
}

/// Fixed answers for tests: recordings and releases by MBID. A missing
/// entry reads as "MusicBrainz does not know it".
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct FixedAlbums {
    /// Recording MBID to its releases.
    pub recordings: std::collections::HashMap<String, Vec<ReleaseCandidate>>,
    /// Release MBID to the release.
    pub releases: std::collections::HashMap<String, AlbumRelease>,
}

#[cfg(any(test, feature = "test-support"))]
impl AlbumLookup for FixedAlbums {
    fn recording_releases<'a>(
        &'a self,
        recording_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<Vec<ReleaseCandidate>>, LookupError>> {
        Box::pin(async move { Ok(self.recordings.get(recording_mbid).cloned()) })
    }

    fn release<'a>(
        &'a self,
        release_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Option<AlbumRelease>, LookupError>> {
        Box::pin(async move { Ok(self.releases.get(release_mbid).cloned()) })
    }
}
