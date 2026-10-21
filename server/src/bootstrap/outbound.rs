//! Where the outside services with an address fixed in code live.
//!
//! [`build_with`](super::build_with) reaches each of them at the address
//! given here. `main` passes [`Outbound::live`]; a test passes loopback
//! addresses. It is a constructor argument only: nothing reads it from the
//! environment or from settings. Services an admin points somewhere in
//! Settings (download clients, indexers, media servers, the OIDC issuer, a
//! MusicBrainz mirror) are not listed.
//!
//! It also carries the two provider ports the library's identification and
//! contributions read through. `main` leaves both alone, so the live
//! clients are built; a test puts scripted ones there. And it carries the
//! ffmpeg the server transcodes with: [`Outbound::live`] finds it on
//! `PATH` once, at boot; a test gives a stand-in or none.

use std::path::PathBuf;
use std::sync::Arc;

use droppedneedle_acquire::wiring::{SPOTIFY_ACCOUNTS_BASE, SPOTIFY_API_BASE, SpotifyEndpoints};
use droppedneedle_auth::{users::hibp::RANGE_URL_PREFIX, wiring::Upstreams};
use droppedneedle_library::{contrib::ContribProviders, identify::providers::IdentifyProviders};
use droppedneedle_plugins::install::GitHubHosts;
use droppedneedle_providers::{
    acoustid, audiodb, coverart, discogs, geocoding, github, itunes, lastfm, listenbrainz, lrclib,
    musicbrainz, plextv::PLEX_TV_BASE, preview, skiddle, ticketmaster, wikidata, youtube,
};

/// A provider port the builder fills with its live client unless one is
/// given here.
#[derive(Clone)]
pub struct Port<T>(Option<T>);

impl<T: Clone> Port<T> {
    /// The live client.
    pub fn live() -> Self {
        Self(None)
    }

    /// `port` in place of the live client.
    pub fn given(port: T) -> Self {
        Self(Some(port))
    }

    /// The port given, if one was.
    pub fn get(&self) -> Option<T> {
        self.0.clone()
    }
}

impl<T> std::fmt::Debug for Port<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() { "given" } else { "live" })
    }
}

/// The base URL of every outside service whose address is fixed in code.
#[derive(Debug, Clone)]
pub struct Outbound {
    /// ListenBrainz API root: charts, wrapped, popularity, the Weekly Mix,
    /// scrobble forwarding and the token check.
    pub listenbrainz: String,
    /// Last.fm API endpoint: catalog reads, charts, scrobble forwarding
    /// and account linking.
    pub lastfm: String,
    /// TheAudioDB API root.
    pub audiodb: String,
    /// Wikidata origin.
    pub wikidata: String,
    /// Wikipedia origin pattern with `{lang}`.
    pub wikipedia: String,
    /// Wikimedia Commons origin.
    pub commons: String,
    /// iTunes Search endpoint: purchase links and previews.
    pub itunes: String,
    /// Deezer API origin (previews).
    pub deezer: String,
    /// Cover Art Archive origin.
    pub cover_art_archive: String,
    /// Discogs API origin.
    pub discogs: String,
    /// LRCLIB origin.
    pub lrclib: String,
    /// The releases list behind the version check.
    pub github_releases: String,
    /// GitHub API origin (plugin installs).
    pub github_api: String,
    /// GitHub archive download origin (plugin installs).
    pub github_codeload: String,
    /// YouTube Data API host: search and the key check.
    pub youtube: String,
    /// AcoustID API origin.
    pub acoustid: String,
    /// The breach check's range URL, up to the hash prefix.
    pub pwned_passwords: String,
    /// plex.tv account API root.
    pub plex_tv: String,
    /// Spotify Web API root.
    pub spotify_api: String,
    /// Spotify accounts origin (token exchange).
    pub spotify_accounts: String,
    /// A playlist cover origin allowed beside the Spotify CDN: the tests'
    /// mock CDN. `None` in production, where only https to `*.scdn.co`
    /// is fetched.
    pub spotify_cover_cdn: Option<String>,
    /// Ticketmaster Discovery root: events and the key check.
    pub ticketmaster: String,
    /// Skiddle API root: events and the key check.
    pub skiddle: String,
    /// Open-Meteo geocoding search URL.
    pub geocoding: String,
    /// Official MusicBrainz API root.
    pub musicbrainz: String,
    /// BrainzMash API root.
    pub brainzmash: String,
    /// What library identification asks MusicBrainz and AcoustID through.
    pub identify_providers: Port<Arc<dyn IdentifyProviders>>,
    /// What library contributions ask Discogs and MusicBrainz through.
    pub contribution_providers: Port<ContribProviders>,
    /// The ffmpeg program transcodes run. `None` turns transcoding off:
    /// every stream is the file as it is.
    pub ffmpeg: Option<PathBuf>,
}

impl Outbound {
    /// The real services.
    pub fn live() -> Self {
        Self {
            listenbrainz: listenbrainz::DEFAULT_BASE_URL.to_owned(),
            lastfm: lastfm::DEFAULT_BASE_URL.to_owned(),
            audiodb: audiodb::DEFAULT_BASE_URL.to_owned(),
            wikidata: wikidata::WIKIDATA_BASE.to_owned(),
            wikipedia: wikidata::WIKIPEDIA_PATTERN.to_owned(),
            commons: wikidata::COMMONS_BASE.to_owned(),
            itunes: itunes::SEARCH_URL.to_owned(),
            deezer: preview::DEEZER_API_BASE.to_owned(),
            cover_art_archive: coverart::COVER_ART_ARCHIVE_BASE.to_owned(),
            discogs: discogs::API_BASE.to_owned(),
            lrclib: lrclib::API_BASE.to_owned(),
            github_releases: github::GITHUB_RELEASES_URL.to_owned(),
            github_api: droppedneedle_plugins::install::API_BASE.to_owned(),
            github_codeload: droppedneedle_plugins::install::CODELOAD_BASE.to_owned(),
            youtube: youtube::YOUTUBE_API_HOST.to_owned(),
            acoustid: acoustid::DEFAULT_BASE_URL.to_owned(),
            pwned_passwords: RANGE_URL_PREFIX.to_owned(),
            plex_tv: PLEX_TV_BASE.to_owned(),
            spotify_api: SPOTIFY_API_BASE.to_owned(),
            spotify_accounts: SPOTIFY_ACCOUNTS_BASE.to_owned(),
            spotify_cover_cdn: None,
            ticketmaster: ticketmaster::TICKETMASTER_API_URL.to_owned(),
            skiddle: skiddle::SKIDDLE_API_URL.to_owned(),
            geocoding: geocoding::GEOCODING_API_URL.to_owned(),
            musicbrainz: musicbrainz::MB_API_BASE.to_owned(),
            brainzmash: musicbrainz::BRAINZMASH_API_BASE.to_owned(),
            identify_providers: Port::live(),
            contribution_providers: Port::live(),
            ffmpeg: droppedneedle_media::stream::transcode::ffmpeg_on_path(),
        }
    }

    /// The addresses of the providers built once for every area.
    pub fn providers(&self) -> droppedneedle_providers::ProviderEndpoints {
        droppedneedle_providers::ProviderEndpoints {
            musicbrainz: self.musicbrainz.clone(),
            brainzmash: self.brainzmash.clone(),
            coverart: self.cover_art_archive.clone(),
            listenbrainz: self.listenbrainz.clone(),
            lastfm: self.lastfm.clone(),
            audiodb: self.audiodb.clone(),
            acoustid: self.acoustid.clone(),
            discogs: self.discogs.clone(),
            itunes: self.itunes.clone(),
            deezer: self.deezer.clone(),
            lrclib: self.lrclib.clone(),
            wikidata: self.wikidata.clone(),
            wikipedia: self.wikipedia.clone(),
            commons: self.commons.clone(),
            ticketmaster: self.ticketmaster.clone(),
            skiddle: self.skiddle.clone(),
            geocoding: self.geocoding.clone(),
            youtube: self.youtube.clone(),
            github_releases: self.github_releases.clone(),
        }
    }

    /// The addresses sign-in and account linking use.
    pub fn sign_in(&self) -> Upstreams {
        Upstreams {
            plex_tv: self.plex_tv.clone(),
            pwned_passwords: self.pwned_passwords.clone(),
        }
    }

    /// The addresses the Spotify import reaches.
    pub fn spotify(&self) -> SpotifyEndpoints {
        SpotifyEndpoints {
            api: self.spotify_api.clone(),
            accounts: self.spotify_accounts.clone(),
            cover_cdn: self.spotify_cover_cdn.clone(),
        }
    }

    /// The provider ports the library uses.
    pub fn library(&self) -> droppedneedle_library::wiring::Endpoints {
        droppedneedle_library::wiring::Endpoints {
            identify_providers: self.identify_providers.get(),
            contribution_providers: self.contribution_providers.get(),
        }
    }

    /// The addresses plugin installs fetch from.
    pub fn plugin_hosts(&self) -> GitHubHosts {
        GitHubHosts {
            api: self.github_api.clone(),
            codeload: self.github_codeload.clone(),
        }
    }
}
