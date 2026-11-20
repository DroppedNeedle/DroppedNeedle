//! Song links: a Deezer, Apple Music or Spotify song address, or a
//! song.link one carrying such an id, read into the MusicBrainz songs it
//! names.
//!
//! The link's service gives the song's ISRC (Deezer, Spotify) or its title,
//! artist and length (Apple Music). An ISRC finds every recording
//! MusicBrainz files it under. A name finds the recordings that score at
//! least [`NAME_SCORE`] and run within [`LENGTH_SLACK_SECS`] of it, so a
//! weak match shows nothing rather than a guess. Spotify is read with the
//! server's Spotify app, without anyone's sign-in. Ids must match each
//! service's pattern, and logs carry `service:id`, never the link.

use std::fmt;
use std::time::Duration;

use droppedneedle_providers::musicbrainz::{Criticality, escape_lucene_phrase};
use droppedneedle_providers::{CacheKey, ProviderError, RequestPriority, Source};
use url::Url;

use super::search::{RemoteBucket, RemoteHit, found_hits, song_hits};
use super::{Catalog, mb_search_error};

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
}

impl fmt::Display for SongLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Deezer(id) => write!(f, "deezer:{id}"),
            Self::AppleMusic { id, country } => write!(f, "apple:{country}:{id}"),
            Self::Spotify(id) => write!(f, "spotify:{id}"),
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

/// What a link's service says about its song.
#[derive(Debug, PartialEq, Eq)]
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
    /// The MusicBrainz songs `link` names, found within `deadline`, or
    /// `None` when the link cannot be read: its service does not hold the
    /// song, did not answer, or (for Spotify) the server has no Spotify
    /// app.
    pub async fn search_song_link(
        &self,
        link: &SongLink,
        deadline: Duration,
    ) -> Option<RemoteBucket> {
        let started = tokio::time::Instant::now();
        let clue = match tokio::time::timeout(deadline, self.read_song_link(link)).await {
            Ok(Ok(clue)) => clue,
            Ok(Err(error)) => {
                tracing::warn!(%link, %error, "song link read failed");
                None
            }
            Err(_) => {
                tracing::warn!(%link, "song link read timed out");
                None
            }
        }?;
        let query = link_query(&clue)?;
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
        Some(bucket)
    }

    /// What `link`'s service says about its song, `None` when it does not
    /// hold it.
    async fn read_song_link(&self, link: &SongLink) -> Result<Option<SongClue>, ProviderError> {
        let upstream = self.upstream();
        let priority = RequestPriority::UserInitiated;
        Ok(match link {
            SongLink::Deezer(id) => {
                upstream
                    .deezer()
                    .deezer_track(*id, priority)
                    .await?
                    .map(|track| SongClue {
                        isrc: track.isrc,
                        title: if track.title_short.is_empty() {
                            track.title
                        } else {
                            track.title_short
                        },
                        artist: track.artist.map(|artist| artist.name).unwrap_or_default(),
                        duration_secs: track.duration,
                    })
            }
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
                let Some(app) = upstream.settings().spotify_app() else {
                    return Ok(None);
                };
                let spotify = upstream.spotify();
                let token = spotify
                    .app_token(&app.client_id, app.client_secret.expose(), priority)
                    .await?;
                match spotify.track(&token.access_token, id, priority).await {
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
}

#[cfg(test)]
mod tests {
    use super::{SongClue, SongLink, bare_title, link_query, parse};

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
            // Not songs, not these services, or not well formed.
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
