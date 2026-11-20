//! Song links: a Deezer, Apple Music or Spotify song address, or a
//! song.link one carrying such an id, read into the MusicBrainz songs it
//! names.
//!
//! The link's service gives the song's ISRC (Deezer, Spotify), its title,
//! artist and length (Apple Music), or a video's title and channel
//! (YouTube). An ISRC finds every recording MusicBrainz files it under. A
//! name finds the recordings that score at least [`NAME_SCORE`] and run
//! within [`LENGTH_SLACK_SECS`] of it, so a weak match shows nothing rather
//! than a guess. Spotify is read with the server's Spotify app, without
//! anyone's sign-in, and YouTube only while it is switched on. A Deezer
//! short link is followed one hop and counts only when it lands on a
//! Deezer track. Ids must match each service's pattern, and logs carry
//! `service:id`, never the link.

use std::fmt;
use std::time::Duration;

use droppedneedle_kernel::runtime_config::secret_sections::SpotifySettings;
use droppedneedle_providers::musicbrainz::{Criticality, escape_lucene_phrase};
use droppedneedle_providers::{CacheKey, Fetched, Policy, ProviderError, RequestPriority, Source};
use serde::{Deserialize, Serialize};
use url::Url;

use super::search::{RemoteBucket, RemoteHit, found_hits, song_hits};
use super::{Catalog, CatalogError, mb_search_error};

/// The lowest MusicBrainz score a song found by name may have.
pub const NAME_SCORE: i32 = 90;
/// How far a song found by name may run from the link's length.
pub const LENGTH_SLACK_SECS: i64 = 3;
/// Recordings asked for per link.
const LINK_WIDTH: u32 = 25;

/// A song link: its service and that service's id for the song.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SongLink {
    /// A Deezer track.
    Deezer(u64),
    /// An Apple Music song in one country's store.
    AppleMusic {
        /// The store's song id.
        id: u64,
        /// The store, two lowercase letters.
        country: String,
    },
    /// A Spotify track.
    Spotify(String),
    /// A YouTube video.
    YouTube(String),
    /// A Deezer short link's code (`link.deezer.com/s/{code}`).
    DeezerShort(String),
}

impl fmt::Display for SongLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deezer(id) => write!(f, "deezer:{id}"),
            Self::AppleMusic { id, country } => write!(f, "apple:{country}:{id}"),
            Self::Spotify(id) => write!(f, "spotify:{id}"),
            Self::YouTube(id) => write!(f, "youtube:{id}"),
            Self::DeezerShort(code) => write!(f, "deezer-short:{code}"),
        }
    }
}

/// The first song link among the words of `query`, so a shared
/// "Song by Artist https://..." reads as its link.
pub fn parse(query: &str) -> Option<SongLink> {
    query.split_whitespace().find_map(parse_word)
}

fn parse_word(word: &str) -> Option<SongLink> {
    if let Some(id) = word.strip_prefix("spotify:track:") {
        return spotify_id(id).map(SongLink::Spotify);
    }
    let url = Url::parse(word).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    match (url.host_str()?, segments.as_slice()) {
        ("open.spotify.com", ["track", id]) => spotify_id(id).map(SongLink::Spotify),
        ("open.spotify.com", [intl, "track", id]) if intl.starts_with("intl-") => {
            spotify_id(id).map(SongLink::Spotify)
        }
        ("www.deezer.com" | "deezer.com", ["track", id] | [_, "track", id]) => {
            store_id(id).map(SongLink::Deezer)
        }
        ("music.apple.com" | "geo.music.apple.com", [country, "album", ..]) => {
            let song = url.query_pairs().find(|(key, _)| key == "i")?.1;
            apple(country, &song)
        }
        ("music.apple.com" | "geo.music.apple.com", [country, "song", .., id]) => {
            apple(country, id)
        }
        ("www.youtube.com" | "youtube.com" | "m.youtube.com" | "music.youtube.com", ["watch"]) => {
            let id = url.query_pairs().find(|(key, _)| key == "v")?.1;
            youtube_id(&id).map(SongLink::YouTube)
        }
        ("www.youtube.com" | "youtube.com" | "m.youtube.com", ["shorts", id])
        | ("youtu.be", [id]) => youtube_id(id).map(SongLink::YouTube),
        ("link.deezer.com", ["s", code]) => {
            let ok = !code.is_empty()
                && code.len() <= 40
                && code.bytes().all(|b| b.is_ascii_alphanumeric());
            ok.then(|| SongLink::DeezerShort((*code).to_owned()))
        }
        ("song.link", ["s", id]) => spotify_id(id).map(SongLink::Spotify),
        ("song.link", ["d", id]) => store_id(id).map(SongLink::Deezer),
        // shortcut: song.link's Apple ids carry no store, so the US one is
        // asked; read the store once song.link links name one.
        ("song.link", ["i", id]) => apple("us", id),
        _ => None,
    }
}

fn apple(country: &str, id: &str) -> Option<SongLink> {
    let country_ok = country.len() == 2 && country.bytes().all(|b| b.is_ascii_alphabetic());
    Some(SongLink::AppleMusic {
        id: store_id(id)?,
        country: country_ok.then(|| country.to_ascii_lowercase())?,
    })
}

/// A Deezer or Apple id: digits only.
fn store_id(id: &str) -> Option<u64> {
    (!id.is_empty() && id.len() <= 19 && id.bytes().all(|b| b.is_ascii_digit()))
        .then(|| id.parse().ok())
        .flatten()
}

/// A Spotify id: 22 base-62 characters.
fn spotify_id(id: &str) -> Option<String> {
    (id.len() == 22 && id.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| id.to_owned())
}

/// A YouTube video id: 11 letters, digits, `-` or `_`.
fn youtube_id(id: &str) -> Option<String> {
    (id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
    .then(|| id.to_owned())
}

/// Why a song link shows no songs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkUnread {
    /// Its service does not hold the song or did not answer.
    Unreadable,
    /// A Spotify link, and Spotify is not set up or is switched off.
    NeedsSpotify,
}

/// What a link's service says about its song, as the cache keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SongClue {
    isrc: Option<String>,
    title: String,
    artist: String,
    duration_secs: Option<i64>,
}

/// The MusicBrainz query for `clue`: its ISRC when it has a well-formed
/// one, else its title and artist; `None` without a title or an artist.
fn link_query(clue: &SongClue) -> Option<String> {
    let isrc = clue.isrc.as_deref().map(str::trim).unwrap_or_default();
    if isrc.len() == 12 && isrc.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Some(format!("isrc:{}", isrc.to_ascii_uppercase()));
    }
    let title = bare_title(&clue.title);
    let artist = clue.artist.trim();
    (!title.is_empty() && !artist.is_empty()).then(|| {
        format!(
            r#"recording:"{}" AND artist:"{}""#,
            escape_lucene_phrase(title),
            escape_lucene_phrase(artist)
        )
    })
}

/// `title` without a "(feat. ...)" part, which MusicBrainz keeps in the
/// artist credit instead.
fn bare_title(title: &str) -> &str {
    let lower = title.to_ascii_lowercase();
    let cut = [" (feat", " [feat"]
        .iter()
        .filter_map(|mark| lower.find(mark))
        .min()
        .unwrap_or(title.len());
    title[..cut].trim()
}

/// The song a music video's title and channel name: an auto-generated
/// "Artist - Topic" channel's video by the channel, "Artist - Title
/// (Official Video)" by its dash or en dash, else the channel as the
/// artist.
fn video_clue(title: &str, channel: &str) -> SongClue {
    let title = without_video_tags(title);
    let (artist, song) = match (channel.strip_suffix(" - Topic"), split_dash(title)) {
        (Some(artist), _) => (artist, title),
        (None, Some((artist, song))) => (artist, song),
        (None, None) => (channel.strip_suffix("VEVO").unwrap_or(channel), title),
    };
    SongClue {
        isrc: None,
        title: song.trim().to_owned(),
        artist: lead_artist(artist).to_owned(),
        duration_secs: None,
    }
}

/// `title` split at its first spaced dash or en dash.
fn split_dash(title: &str) -> Option<(&str, &str)> {
    [" - ", " \u{2013} "]
        .iter()
        .filter_map(|dash| title.find(dash).map(|at| (at, dash.len())))
        .min()
        .map(|(at, len)| (&title[..at], &title[at + len..]))
}

/// `title` without the bracketed tags a video adds at its end:
/// "(Official Video)", "[Lyrics]", "(HD)" and the like.
fn without_video_tags(title: &str) -> &str {
    const TAGS: [&str; 11] = [
        "official",
        "video",
        "audio",
        "lyric",
        "lyrics",
        "visualizer",
        "visualiser",
        "hd",
        "hq",
        "4k",
        "mv",
    ];
    let mut rest = title.trim();
    loop {
        let open = match rest.chars().last() {
            Some(')') => rest.rfind('('),
            Some(']') => rest.rfind('['),
            _ => None,
        };
        let Some(open) = open else {
            return rest;
        };
        let tag = rest[open + 1..rest.len() - 1].to_ascii_lowercase();
        if !tag
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| TAGS.contains(&word))
        {
            return rest;
        }
        rest = rest[..open].trim_end();
    }
}

/// `artist` without a "feat." part, which MusicBrainz credits apart.
fn lead_artist(artist: &str) -> &str {
    let lower = artist.to_ascii_lowercase();
    let cut = [" feat. ", " feat ", " ft. ", " featuring "]
        .iter()
        .filter_map(|mark| lower.find(mark))
        .min()
        .unwrap_or(artist.len());
    artist[..cut].trim()
}

/// Whether a song found by name runs close enough to the link's length;
/// either length unknown passes.
fn length_fits(hit: &RemoteHit, duration_secs: Option<i64>) -> bool {
    let found = hit.track.as_ref().and_then(|track| track.duration_seconds);
    match (found, duration_secs) {
        (Some(found), Some(wanted)) => (found - wanted).abs() <= LENGTH_SLACK_SECS,
        _ => true,
    }
}

impl Catalog {
    /// The MusicBrainz songs `link` names, found within `deadline`, or why
    /// there are none to show. What the link's service said is kept a day
    /// under `<service>:<id>`, as the MusicBrainz answer is, so the same
    /// link again asks neither.
    ///
    /// # Errors
    ///
    /// [`LinkUnread::NeedsSpotify`] for a Spotify link while Spotify is not
    /// set up or is switched off, and [`LinkUnread::Unreadable`] when the
    /// service does not hold the song, did not answer, or (YouTube) is
    /// switched off.
    pub async fn search_song_link(
        &self,
        link: &SongLink,
        deadline: Duration,
    ) -> Result<RemoteBucket, LinkUnread> {
        let started = tokio::time::Instant::now();
        let settings = self.upstream().settings();
        let spotify = match link {
            SongLink::Spotify(_) => Some(settings.spotify_app().ok_or(LinkUnread::NeedsSpotify)?),
            SongLink::YouTube(_) if !settings.youtube_on() => return Err(LinkUnread::Unreadable),
            _ => None,
        };
        let clue = match tokio::time::timeout(deadline, self.song_clue(link, spotify)).await {
            Ok(Ok(Some(clue))) => clue,
            Ok(Ok(None)) => return Err(LinkUnread::Unreadable),
            Ok(Err(error)) => {
                // The service's own failure, not the catalog's MusicBrainz wording.
                let error = match error {
                    CatalogError::Unavailable(cause) => cause.to_string(),
                    other => other.to_string(),
                };
                tracing::warn!(%link, %error, "song link read failed");
                return Err(LinkUnread::Unreadable);
            }
            Err(_) => {
                tracing::warn!(%link, "song link read timed out");
                return Err(LinkUnread::Unreadable);
            }
        };
        let query = link_query(&clue).ok_or(LinkUnread::Unreadable)?;
        let by_name = !query.starts_with("isrc:");
        let (_, namespace) = self.upstream().musicbrainz(RequestPriority::UserInitiated);
        let key = CacheKey::hashed(
            Source::MusicBrainz,
            "song_link",
            &[namespace.as_str(), query.as_str()],
        );
        let catalog = self.clone();
        let fetch = move |priority| async move {
            let (client, _) = catalog.upstream().musicbrainz(priority);
            let page = client
                .search_recordings_query(&query, LINK_WIDTH, Criticality::IdentityCritical)
                .await
                .map_err(mb_search_error)?;
            Ok(found_hits(song_hits(page.items)))
        };
        let left = deadline.saturating_sub(started.elapsed());
        let mut bucket = self.remote_bucket(key, left, fetch).await;
        if by_name {
            bucket
                .hits
                .retain(|hit| hit.score >= NAME_SCORE && length_fits(hit, clue.duration_secs));
        }
        Ok(bucket)
    }

    /// What `link`'s service says about its song, through the cache;
    /// `None` when it does not hold it. `spotify` is the app a Spotify
    /// link is read with.
    async fn song_clue(
        &self,
        link: &SongLink,
        spotify: Option<SpotifySettings>,
    ) -> Result<Option<SongClue>, CatalogError> {
        let key = CacheKey::new(Source::SongLinks, &[&link.to_string()]);
        let catalog = self.clone();
        let link = link.clone();
        let fetch = move |priority| async move {
            let clue = catalog
                .read_song_link(&link, spotify.as_ref(), priority)
                .await
                .map_err(CatalogError::Unavailable)?;
            Ok(clue.map_or(Fetched::Missing, Fetched::Found))
        };
        self.cached(&key, Policy::SEARCH, RequestPriority::UserInitiated, fetch)
            .await
    }

    /// What `link`'s service says about its song, `None` when it does not
    /// hold it or, for Spotify, there is no `spotify` app.
    async fn read_song_link(
        &self,
        link: &SongLink,
        spotify: Option<&SpotifySettings>,
        priority: RequestPriority,
    ) -> Result<Option<SongClue>, ProviderError> {
        let upstream = self.upstream();
        Ok(match link {
            SongLink::Deezer(id) => self.read_deezer(*id, priority).await?,
            SongLink::DeezerShort(code) => {
                let dest = upstream.deezer().deezer_short_link(code, priority).await?;
                match dest.as_deref().and_then(parse_word) {
                    Some(SongLink::Deezer(id)) => self.read_deezer(id, priority).await?,
                    _ => None,
                }
            }
            SongLink::YouTube(id) => upstream
                .youtube()
                .oembed(id, priority)
                .await?
                .map(|video| video_clue(&video.title, &video.author_name)),
            SongLink::AppleMusic { id, country } => upstream
                .itunes()
                .lookup_song(*id, country, priority)
                .await?
                .map(|song| SongClue {
                    isrc: None,
                    title: song.track_name,
                    artist: song.artist_name,
                    duration_secs: song.track_time_millis.map(|ms| ms / 1000),
                }),
            SongLink::Spotify(id) => {
                let Some(app) = spotify else {
                    return Ok(None);
                };
                let read = upstream.spotify().app_track(
                    &app.client_id,
                    app.client_secret.expose(),
                    id,
                    priority,
                );
                match read.await {
                    Ok(track) => Some(SongClue {
                        isrc: track.external_ids.and_then(|ids| ids.isrc),
                        title: track.name,
                        artist: track
                            .artists
                            .into_iter()
                            .find_map(|artist| artist.name)
                            .unwrap_or_default(),
                        duration_secs: track.duration_ms.map(|ms| ms / 1000),
                    }),
                    Err(ProviderError::NotFound { .. }) => None,
                    Err(error) => return Err(error),
                }
            }
        })
    }

    /// Deezer track `id`'s ISRC, title, artist and length.
    async fn read_deezer(
        &self,
        id: u64,
        priority: RequestPriority,
    ) -> Result<Option<SongClue>, ProviderError> {
        let track = self.upstream().deezer().deezer_track(id, priority).await?;
        Ok(track.map(|track| SongClue {
            isrc: track.isrc,
            title: if track.title_short.is_empty() {
                track.title
            } else {
                track.title_short
            },
            artist: track.artist.map(|artist| artist.name).unwrap_or_default(),
            duration_secs: track.duration,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{SongClue, SongLink, bare_title, link_query, parse, video_clue};

    #[test]
    fn links_read_as_their_service_and_id() {
        let spotify = || Some(SongLink::Spotify("1a2B3c4D5e6F7g8H9i0JkL".to_owned()));
        let apple = |country: &str| {
            Some(SongLink::AppleMusic {
                id: 1440783625,
                country: country.to_owned(),
            })
        };
        for (query, link) in [
            (
                "https://open.spotify.com/track/1a2B3c4D5e6F7g8H9i0JkL?si=abc",
                spotify(),
            ),
            (
                "https://open.spotify.com/intl-de/track/1a2B3c4D5e6F7g8H9i0JkL",
                spotify(),
            ),
            ("spotify:track:1a2B3c4D5e6F7g8H9i0JkL", spotify()),
            (
                "Listen to this https://open.spotify.com/track/1a2B3c4D5e6F7g8H9i0JkL",
                spotify(),
            ),
            ("https://song.link/s/1a2B3c4D5e6F7g8H9i0JkL", spotify()),
            (
                "https://www.deezer.com/track/13791930",
                Some(SongLink::Deezer(13791930)),
            ),
            (
                "https://www.deezer.com/en/track/13791930?utm_source=x",
                Some(SongLink::Deezer(13791930)),
            ),
            (
                "https://song.link/d/13791930",
                Some(SongLink::Deezer(13791930)),
            ),
            (
                "https://music.apple.com/us/album/nevermind/1440783617?i=1440783625",
                apple("us"),
            ),
            (
                "https://music.apple.com/GB/song/smells-like-teen-spirit/1440783625",
                apple("gb"),
            ),
            (
                "https://geo.music.apple.com/us/song/1440783625",
                apple("us"),
            ),
            ("https://song.link/i/1440783625", apple("us")),
            (
                "https://www.youtube.com/watch?v=hTWKbfoikeg&list=x",
                Some(SongLink::YouTube("hTWKbfoikeg".to_owned())),
            ),
            (
                "https://music.youtube.com/watch?v=hTWKbfoikeg",
                Some(SongLink::YouTube("hTWKbfoikeg".to_owned())),
            ),
            (
                "https://youtu.be/hTWKbfoikeg?si=x",
                Some(SongLink::YouTube("hTWKbfoikeg".to_owned())),
            ),
            (
                "https://www.youtube.com/shorts/hTWKbfoikeg",
                Some(SongLink::YouTube("hTWKbfoikeg".to_owned())),
            ),
            (
                "https://link.deezer.com/s/31Wq4dKe1ARDAb5TeNIGh",
                Some(SongLink::DeezerShort("31Wq4dKe1ARDAb5TeNIGh".to_owned())),
            ),
            // Not songs, not these services, or not well formed.
            ("https://www.youtube.com/watch?v=short", None),
            ("https://www.youtube.com/channel/hTWKbfoikeg", None),
            ("https://link.deezer.com/s/../track", None),
            (
                "https://music.apple.com/us/album/nevermind/1440783617",
                None,
            ),
            (
                "https://open.spotify.com/album/1a2B3c4D5e6F7g8H9i0JkL",
                None,
            ),
            ("https://open.spotify.com/track/short", None),
            (
                "https://open.spotify.com.example.test/track/1a2B3c4D5e6F7g8H9i0JkL",
                None,
            ),
            ("http://www.deezer.com/track/13791930", None),
            ("https://www.deezer.com/track/12ab", None),
            ("https://music.apple.com/usa/song/x/1440783625", None),
            ("smells like teen spirit", None),
        ] {
            assert_eq!(parse(query), link, "{query}");
        }
    }

    #[test]
    fn a_video_reads_as_artist_and_title() {
        let read = |title: &str, channel: &str| {
            let clue = video_clue(title, channel);
            (clue.artist, clue.title)
        };
        let pair = |artist: &str, title: &str| (artist.to_owned(), title.to_owned());
        assert_eq!(
            read("Aurora - Opener (Official Music Video)", "AuroraVEVO"),
            pair("Aurora", "Opener")
        );
        assert_eq!(
            read("Aurora \u{2013} Opener (Official Video)", "Some Channel"),
            pair("Aurora", "Opener")
        );
        assert_eq!(
            read(
                "Aurora ft. Nightfall - Opener [Lyrics] (HD)",
                "Some Channel"
            ),
            pair("Aurora", "Opener")
        );
        assert_eq!(
            read("Opener - Live", "Aurora - Topic"),
            pair("Aurora", "Opener - Live")
        );
        assert_eq!(
            read("Opener (Acoustic)", "AuroraVEVO"),
            pair("Aurora", "Opener (Acoustic)")
        );
    }

    #[test]
    fn a_link_asks_musicbrainz_by_isrc_else_by_name() {
        let clue = |isrc: Option<&str>, title: &str, artist: &str| SongClue {
            isrc: isrc.map(str::to_owned),
            title: title.to_owned(),
            artist: artist.to_owned(),
            duration_secs: None,
        };
        assert_eq!(
            link_query(&clue(Some("usgf19942501"), "Opener", "Aurora")).as_deref(),
            Some("isrc:USGF19942501")
        );
        assert_eq!(
            link_query(&clue(Some("not-an-isrc"), "Opener", "Aurora")).as_deref(),
            Some(r#"recording:"Opener" AND artist:"Aurora""#)
        );
        assert_eq!(link_query(&clue(None, "Opener", " ")), None);
        assert_eq!(bare_title("Second Song (feat. Aurora)"), "Second Song");
        assert_eq!(bare_title("Second Song [Feat. Aurora]"), "Second Song");
    }
}
