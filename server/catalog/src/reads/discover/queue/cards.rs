//! The details behind one queue card (v2 `QueueEnrichmentService`):
//! enrichment (tags, release date, country, artist bio, listen count, a
//! video link) and the on-demand video preview.
//!
//! Each piece is best effort: a provider that fails leaves its field
//! empty, and the card's [`CacheHold`] keeps it from being cached as if
//! that field had nothing. Only the facts that are the same for every
//! user ([`CardFacts`]) are cached as a card; the listen count and
//! Last.fm's tags and bio, which fill in where MusicBrainz and Wikipedia
//! have none, are added for each user when the card is served.

use serde::{Deserialize, Serialize};

use crate::reads::cache_hold::CacheHold;
use crate::reads::catalog::mapping::clean_lastfm_bio;
use crate::reads::discover::listening::{Ask, ListeningData, ListeningError, ListeningResult};
use crate::reads::discover::models::{DiscoverQueuePreview, PreviewStatus, QueueEnrichment};
use crate::reads::discover::ports::ProviderFailure;
use crate::reads::discover::youtube::LiveYouTube;

/// Most tags a card shows.
const MAX_TAGS: usize = 10;
/// Recordings tried for a video before falling back to search.
const PREVIEW_RECORDINGS: usize = 3;

fn quiet<T: Default>(what: &str, result: ListeningResult<T>, hold: &mut CacheHold) -> T {
    result.unwrap_or_else(|cause| {
        tracing::debug!(read = what, %cause, "queue card read failed; leaving it out");
        match &cause {
            ListeningError::Provider(error) => hold.provider(error),
            ListeningError::MediaServer(_) => hold.local(),
        }
        T::default()
    })
}

/// The 11-character video id inside a YouTube watch, short or embed link.
pub fn youtube_video_id(url: &str) -> Option<String> {
    let after = ["youtube.com/watch?v=", "youtu.be/", "youtube.com/embed/"]
        .iter()
        .find_map(|marker| url.find(marker).map(|at| &url[at + marker.len()..]))?;
    let id: String = after
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    (id.len() == 11).then_some(id)
}

/// The embed URL for a YouTube link, when it names a video.
pub fn youtube_embed(url: &str) -> Option<String> {
    youtube_video_id(url).map(|id| format!("https://www.youtube.com/embed/{id}"))
}

/// A YouTube search for the album, as a plain link.
pub fn youtube_search_url(artist: &str, album: &str) -> String {
    let query = format!("{artist} {album}");
    match reqwest::Url::parse_with_params(
        "https://www.youtube.com/results",
        &[("search_query", query.as_str())],
    ) {
        Ok(url) => url.to_string(),
        Err(_) => "https://www.youtube.com/results".to_owned(),
    }
}

/// The facts behind a card that are the same for every user: MusicBrainz
/// and Wikipedia. Kept in the provider cache under the release group.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CardFacts {
    /// Release-group title.
    pub title: String,
    /// First credited artist name.
    pub artist_name: String,
    /// First credited artist id.
    pub artist_mbid: Option<String>,
    /// First release date.
    pub release_date: Option<String>,
    /// The artist's country.
    pub country: Option<String>,
    /// MusicBrainz tags, most voted first.
    pub tags: Vec<String>,
    /// An embed link from the group's URL relationships.
    pub youtube_url: Option<String>,
    /// The artist's Wikipedia summary.
    pub artist_description: Option<String>,
}

/// The shared facts for one release group, with how long they may be
/// cached given the reads that failed.
pub async fn facts(listening: &ListeningData, release_group_mbid: &str) -> (CardFacts, CacheHold) {
    let ask = Ask::person();
    let mut hold = CacheHold::default();
    let group = quiet(
        "musicbrainz release group",
        listening.release_group(release_group_mbid, &ask).await,
        &mut hold,
    )
    .unwrap_or_default();
    let mut facts = CardFacts {
        title: group.title,
        artist_name: group.artist_name.unwrap_or_default(),
        artist_mbid: group.artist_mbid,
        release_date: group.first_release_date,
        country: None,
        tags: group.tags.into_iter().take(MAX_TAGS).collect(),
        youtube_url: group.youtube_url.as_deref().and_then(youtube_embed),
        artist_description: None,
    };
    if let Some(mbid) = &facts.artist_mbid {
        let artist = quiet(
            "musicbrainz artist",
            listening.artist(mbid, &ask).await,
            &mut hold,
        );
        if let Some(artist) = artist {
            facts.country = artist.country;
            if let Some(url) = artist.wiki_url {
                facts.artist_description = quiet(
                    "wikipedia",
                    listening.wikipedia_extract(&url, &ask).await,
                    &mut hold,
                )
                .filter(|text| !text.trim().is_empty());
            }
        }
    }
    (facts, hold)
}

/// One user's card: the shared facts plus what depends on the user, the
/// listen count and Last.fm's tags and bio where MusicBrainz and
/// Wikipedia have none. Those reads are cached on their own.
pub async fn for_user(
    listening: &ListeningData,
    youtube: &LiveYouTube,
    user_id: &str,
    release_group_mbid: &str,
    facts: CardFacts,
) -> QueueEnrichment {
    let ask = Ask::person();
    let ids = [release_group_mbid.to_owned()];
    let listens = async {
        quiet(
            "listenbrainz listen counts",
            listening.listen_counts(user_id, &ids, &ask).await,
            &mut CacheHold::default(),
        )
        .get(release_group_mbid)
        .copied()
    };
    let fallback = lastfm_fallback(listening, &ask, user_id, &facts);
    let (listen_count, fallback) = tokio::join!(listens, fallback);
    let mut enrichment = QueueEnrichment {
        youtube_search_url: youtube_search_url(&facts.artist_name, &facts.title),
        youtube_search_available: facts.youtube_url.is_none() && youtube.is_configured(),
        artist_mbid: facts.artist_mbid.or(fallback.mbid),
        release_date: facts.release_date,
        country: facts.country,
        tags: facts.tags,
        youtube_url: facts.youtube_url,
        artist_description: facts.artist_description.or(fallback.description),
        listen_count,
    };
    if enrichment.tags.is_empty() {
        enrichment.tags = fallback.tags;
    }
    enrichment
}

/// What Last.fm adds to a card.
#[derive(Default)]
struct Fallback {
    tags: Vec<String>,
    mbid: Option<String>,
    description: Option<String>,
}

/// Last.fm's album tags and bio, then the artist's when those are still
/// missing. Empty when the user cannot read Last.fm.
async fn lastfm_fallback(
    listening: &ListeningData,
    ask: &Ask,
    user_id: &str,
    facts: &CardFacts,
) -> Fallback {
    let mut fallback = Fallback::default();
    let (artist, album) = (facts.artist_name.as_str(), facts.title.as_str());
    if album.is_empty() || artist.is_empty() {
        return fallback;
    }
    let mut hold = CacheHold::default();
    let album_facts = quiet(
        "lastfm album",
        listening.lastfm_album(user_id, artist, album, ask).await,
        &mut hold,
    );
    let has_description = facts.artist_description.is_some();
    if let Some(found) = album_facts {
        fallback.tags = found.tags.into_iter().take(MAX_TAGS).collect();
        if !has_description {
            fallback.description = clean_lastfm_bio(&found.summary);
        }
    }
    if (has_description || fallback.description.is_some()) && !fallback.tags.is_empty() {
        return fallback;
    }
    let artist_facts = quiet(
        "lastfm artist",
        listening
            .lastfm_artist(user_id, artist, facts.artist_mbid.as_deref(), ask)
            .await,
        &mut hold,
    );
    if let Some(found) = artist_facts {
        fallback.mbid = found.mbid;
        if fallback.tags.is_empty() {
            fallback.tags = found.tags.into_iter().take(MAX_TAGS).collect();
        }
        if !has_description && fallback.description.is_none() {
            fallback.description = clean_lastfm_bio(&found.summary);
        }
    }
    fallback
}

/// Find a video for one release group: links on the group, then on its
/// first release, then on that release's first recordings, then a
/// YouTube search when an API key is set (v2 `preview_queue_item`).
pub async fn preview(
    listening: &ListeningData,
    youtube: &LiveYouTube,
    release_group_mbid: &str,
) -> Result<DiscoverQueuePreview, ProviderFailure> {
    let ask = Ask::person();
    let facts = listening.release_group(release_group_mbid, &ask).await?;
    let Some(facts) = facts else {
        return Ok(DiscoverQueuePreview {
            status: PreviewStatus::NotFound,
            youtube_url: None,
            youtube_search_url: None,
        });
    };
    let artist = facts.artist_name.clone().unwrap_or_default();
    let album = facts.title.clone();
    let search_url = youtube_search_url(&artist, &album);
    let found = |url: Option<String>| {
        url.as_deref()
            .and_then(youtube_embed)
            .map(|embed| DiscoverQueuePreview {
                status: PreviewStatus::Available,
                youtube_url: Some(embed),
                youtube_search_url: Some(search_url.clone()),
            })
    };
    if let Some(answer) = found(facts.youtube_url.clone()) {
        return Ok(answer);
    }
    // The preview is never cached, so what failed is not kept.
    let mut hold = CacheHold::default();
    if let Some(release) = facts.first_release_id.as_deref() {
        // One at a time: the recordings are only read when the release
        // itself has no video link.
        let video = quiet(
            "musicbrainz release links",
            listening.release_video(release, &ask).await,
            &mut hold,
        );
        if let Some(answer) = found(video) {
            return Ok(answer);
        }
        let recordings = quiet(
            "musicbrainz release recordings",
            listening
                .release_recordings(release, PREVIEW_RECORDINGS, &ask)
                .await,
            &mut hold,
        );
        for recording in recordings {
            let video = quiet(
                "musicbrainz recording links",
                listening.recording_video(&recording, &ask).await,
                &mut hold,
            );
            if let Some(answer) = found(video) {
                return Ok(answer);
            }
        }
    }
    let unavailable = DiscoverQueuePreview {
        status: PreviewStatus::Unavailable,
        youtube_url: None,
        youtube_search_url: Some(search_url.clone()),
    };
    if artist.is_empty() || album.is_empty() {
        return Ok(unavailable);
    }
    match youtube.search_video(&artist, &album).await {
        Ok(Some(video_id)) => Ok(DiscoverQueuePreview {
            status: PreviewStatus::Available,
            youtube_url: Some(format!("https://www.youtube.com/embed/{video_id}")),
            youtube_search_url: Some(search_url),
        }),
        Ok(None) => Ok(DiscoverQueuePreview {
            status: PreviewStatus::NotFound,
            youtube_url: None,
            youtube_search_url: Some(search_url),
        }),
        // No key, or today's quota is spent: the search link still works.
        Err(ProviderFailure::NotConfigured(_) | ProviderFailure::Exhausted(_)) => Ok(unavailable),
        Err(other) => Err(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_ids_come_from_every_link_shape() {
        for url in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&t=1",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://www.youtube.com/embed/dQw4w9WgXcQ",
        ] {
            assert_eq!(
                youtube_embed(url).as_deref(),
                Some("https://www.youtube.com/embed/dQw4w9WgXcQ")
            );
        }
        assert_eq!(youtube_embed("https://www.youtube.com/channel/abc"), None);
        assert_eq!(youtube_embed("https://youtu.be/short"), None);
    }
}
