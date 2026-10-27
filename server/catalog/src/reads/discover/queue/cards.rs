//! The details behind one queue card (v2 `QueueEnrichmentService`):
//! enrichment (tags, release date, country, artist bio, listen count, a
//! video link) and the on-demand video preview.
//!
//! Each piece is best effort: a provider that fails leaves its field
//! empty, and the card's [`CacheHold`] keeps it from being cached as if
//! that field had nothing. Last.fm fills tags and the bio when MusicBrainz
//! and Wikipedia have none.

use crate::reads::cache_hold::CacheHold;
use crate::reads::catalog::mapping::clean_lastfm_bio;
use crate::reads::discover::models::{DiscoverQueuePreview, QueueEnrichment};
use crate::reads::discover::ports::ProviderFailure;
use crate::reads::discover::youtube::LiveYouTube;
use droppedneedle_providers::RequestPriority;

use super::sources::{QueueSources, SourceResult};

/// Most tags a card shows.
const MAX_TAGS: usize = 10;
/// Recordings tried for a video before falling back to search.
const PREVIEW_RECORDINGS: usize = 3;

fn quiet<T: Default>(what: &str, result: SourceResult<T>, hold: &mut CacheHold) -> T {
    result.unwrap_or_else(|cause| {
        tracing::debug!(read = what, %cause, "queue card read failed; leaving it out");
        match &cause {
            ProviderFailure::Provider(error) => hold.provider(error),
            ProviderFailure::Failed(_) => hold.local(),
            ProviderFailure::NotConfigured(_)
            | ProviderFailure::NotBuilt(_)
            | ProviderFailure::Exhausted(_) => {}
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

/// Build the enrichment for one release group, with how long it may be
/// cached given the reads that failed.
pub async fn enrich(
    sources: &dyn QueueSources,
    youtube: &LiveYouTube,
    user_id: &str,
    release_group_mbid: &str,
) -> (QueueEnrichment, CacheHold) {
    let mut hold = CacheHold::default();
    let facts = quiet(
        "musicbrainz release group",
        sources
            .musicbrainz_release_group(release_group_mbid, RequestPriority::UserInitiated)
            .await,
        &mut hold,
    )
    .unwrap_or_default();
    let album = facts.title.clone();
    let artist = facts.artist_name.clone().unwrap_or_default();
    let mut enrichment = QueueEnrichment {
        artist_mbid: facts.artist_mbid.clone(),
        release_date: facts.first_release_date.clone(),
        country: None,
        tags: facts.tags.iter().take(MAX_TAGS).cloned().collect(),
        youtube_url: facts.youtube_url.as_deref().and_then(youtube_embed),
        youtube_search_url: youtube_search_url(&artist, &album),
        youtube_search_available: false,
        artist_description: None,
        listen_count: None,
    };
    let ids = [release_group_mbid.to_owned()];
    let listens = async {
        let mut hold = CacheHold::default();
        let count = quiet(
            "listenbrainz listen counts",
            sources
                .listenbrainz_listen_counts(user_id, &ids, RequestPriority::UserInitiated)
                .await,
            &mut hold,
        )
        .get(release_group_mbid)
        .copied();
        (count, hold)
    };
    let about = artist_details(sources, user_id, &facts.artist_mbid, &artist, &album);
    let ((listen_count, listens_hold), (country, description, extra)) =
        tokio::join!(listens, about);
    hold.merge(listens_hold);
    hold.merge(extra.hold);
    enrichment.listen_count = listen_count;
    enrichment.country = country;
    enrichment.artist_description = description;
    if enrichment.tags.is_empty() {
        enrichment.tags = extra.tags;
    }
    if enrichment.artist_mbid.is_none() {
        enrichment.artist_mbid = extra.mbid;
    }
    if enrichment.youtube_url.is_none() {
        enrichment.youtube_search_available = youtube.is_configured();
    }
    (enrichment, hold)
}

/// Last.fm facts gathered while filling the bio, and the reads that
/// failed on the way.
#[derive(Default)]
struct Fallback {
    tags: Vec<String>,
    mbid: Option<String>,
    hold: CacheHold,
}

/// Country and bio from MusicBrainz and Wikipedia, then Last.fm for the
/// bio and tags when those are still missing.
async fn artist_details(
    sources: &dyn QueueSources,
    user_id: &str,
    artist_mbid: &Option<String>,
    artist: &str,
    album: &str,
) -> (Option<String>, Option<String>, Fallback) {
    let mut country = None;
    let mut description = None;
    let mut fallback = Fallback::default();
    if let Some(mbid) = artist_mbid {
        let facts = quiet(
            "musicbrainz artist",
            sources
                .musicbrainz_artist(mbid, RequestPriority::UserInitiated)
                .await,
            &mut fallback.hold,
        );
        if let Some(facts) = facts {
            country = facts.country;
            if let Some(url) = facts.wiki_url {
                description = quiet(
                    "wikipedia",
                    sources.wikipedia_extract(&url).await,
                    &mut fallback.hold,
                )
                .filter(|text| !text.trim().is_empty());
            }
        }
    }
    if album.is_empty() || artist.is_empty() {
        return (country, description, fallback);
    }
    let album_facts = quiet(
        "lastfm album",
        sources
            .lastfm_album_facts(user_id, artist, album, RequestPriority::UserInitiated)
            .await,
        &mut fallback.hold,
    );
    if let Some(facts) = album_facts {
        fallback.tags = facts.tags.into_iter().take(MAX_TAGS).collect();
        if description.is_none() {
            description = clean_lastfm_bio(&facts.summary);
        }
    }
    if description.is_some() && !fallback.tags.is_empty() {
        return (country, description, fallback);
    }
    let artist_facts = quiet(
        "lastfm artist",
        sources
            .lastfm_artist_facts(
                user_id,
                artist,
                artist_mbid.as_deref(),
                RequestPriority::UserInitiated,
            )
            .await,
        &mut fallback.hold,
    );
    if let Some(facts) = artist_facts {
        fallback.mbid = facts.mbid;
        if fallback.tags.is_empty() {
            fallback.tags = facts.tags.into_iter().take(MAX_TAGS).collect();
        }
        if description.is_none() {
            description = clean_lastfm_bio(&facts.summary);
        }
    }
    (country, description, fallback)
}

/// Find a video for one release group: links on the group, then on its
/// first release, then on that release's first recordings, then a
/// YouTube search when an API key is set (v2 `preview_queue_item`).
pub async fn preview(
    sources: &dyn QueueSources,
    youtube: &LiveYouTube,
    release_group_mbid: &str,
) -> Result<DiscoverQueuePreview, ProviderFailure> {
    let facts = sources
        .musicbrainz_release_group(release_group_mbid, RequestPriority::UserInitiated)
        .await?;
    let Some(facts) = facts else {
        return Ok(DiscoverQueuePreview {
            status: "not_found".to_owned(),
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
                status: "available".to_owned(),
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
            sources.musicbrainz_release_video(release).await,
            &mut hold,
        );
        if let Some(answer) = found(video) {
            return Ok(answer);
        }
        let recordings = quiet(
            "musicbrainz release recordings",
            sources
                .musicbrainz_release_recordings(release, PREVIEW_RECORDINGS)
                .await,
            &mut hold,
        );
        for recording in recordings {
            let video = quiet(
                "musicbrainz recording links",
                sources.musicbrainz_recording_video(&recording).await,
                &mut hold,
            );
            if let Some(answer) = found(video) {
                return Ok(answer);
            }
        }
    }
    let unavailable = DiscoverQueuePreview {
        status: "unavailable".to_owned(),
        youtube_url: None,
        youtube_search_url: Some(search_url.clone()),
    };
    if artist.is_empty() || album.is_empty() {
        return Ok(unavailable);
    }
    match youtube.search_video(&artist, &album).await {
        Ok(Some(video_id)) => Ok(DiscoverQueuePreview {
            status: "available".to_owned(),
            youtube_url: Some(format!("https://www.youtube.com/embed/{video_id}")),
            youtube_search_url: Some(search_url),
        }),
        Ok(None) => Ok(DiscoverQueuePreview {
            status: "not_found".to_owned(),
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
