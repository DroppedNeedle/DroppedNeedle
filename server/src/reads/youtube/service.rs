//! YouTube link logic: find videos through the server's one YouTube client
//! (quota-governed), save them, and edit or remove saved links.
//!
//! A saved link is never searched again: generating a link that already
//! exists answers the stored one and spends no quota.

use std::sync::Arc;

use crate::ids::IdGenerator;
use crate::providers::youtube::utc_today;
use crate::reads::collections::db::StoreError;
use crate::reads::discover::ports::{ProviderFailure, YouTubeSource};

use super::models::{
    YouTubeLink, YouTubeLinkGenerateRequest, YouTubeLinkUpdateRequest, YouTubeManualLinkRequest,
    YouTubeQuotaStatus, YouTubeTrackInput, YouTubeTrackLink, YouTubeTrackLinkBatchGenerateRequest,
    YouTubeTrackLinkFailure, YouTubeTrackLinkGenerateRequest,
};
use super::store::{AlbumRef, LinkEdit, LinkOrigin, LinkStore, NewTrackLink};

/// Most tracks one batch call may ask for. Each new one costs a search.
pub const BATCH_TRACKS_MAX: usize = 500;

/// Longest album id accepted.
const ALBUM_ID_MAX: usize = 200;

/// Failure reason when a search ran and found nothing, kept from v2.
const NO_VIDEO_FOUND: &str = "No video found";

/// Failure reason when YouTube itself failed for one track.
const SEARCH_FAILED: &str = "YouTube search failed. Try again later.";

/// Everything a YouTube link call can fail with.
#[derive(Debug, thiserror::Error)]
pub enum LinkError {
    /// Nothing saved or found. The text is for the user.
    #[error("{0}")]
    NotFound(String),
    /// The request is wrong. The text is for the user.
    #[error("{0}")]
    Invalid(String),
    /// YouTube search needs setting up. The text is for the user.
    #[error("{0}")]
    NotConfigured(String),
    /// Today's search budget is spent. The text is for the user.
    #[error("{0}")]
    Exhausted(String),
    /// YouTube failed. The text goes to the log only.
    #[error("youtube search failed: {0}")]
    Upstream(String),
    /// The database failed.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<ProviderFailure> for LinkError {
    fn from(failure: ProviderFailure) -> Self {
        match failure {
            ProviderFailure::Failed(cause) => Self::Upstream(cause),
            ProviderFailure::NotConfigured(sentence) | ProviderFailure::NotBuilt(sentence) => {
                Self::NotConfigured(sentence)
            }
            ProviderFailure::Exhausted(sentence) => Self::Exhausted(sentence),
        }
    }
}

/// The YouTube link service.
#[derive(Clone)]
pub struct YouTubeLinks {
    store: LinkStore,
    search: Arc<dyn YouTubeSource>,
    ids: Arc<dyn IdGenerator>,
}

impl YouTubeLinks {
    /// Service over `store`, searching through `search`, the server's one
    /// YouTube client.
    pub fn new(
        store: LinkStore,
        search: Arc<dyn YouTubeSource>,
        ids: Arc<dyn IdGenerator>,
    ) -> Self {
        Self { store, search, ids }
    }

    /// The same service over another store.
    #[must_use]
    pub fn with_store(mut self, store: LinkStore) -> Self {
        self.store = store;
        self
    }

    /// Fresh ids, for error ids in the handler layer.
    pub fn ids(&self) -> &dyn IdGenerator {
        self.ids.as_ref()
    }

    /// Today's search budget. While search is switched off or has no key
    /// there is nothing to spend, which reads as a zero budget.
    pub async fn quota(&self) -> YouTubeQuotaStatus {
        self.search
            .quota()
            .await
            .map(quota_status)
            .unwrap_or_else(|| YouTubeQuotaStatus {
                used: 0,
                limit: 0,
                remaining: 0,
                date: utc_today(),
            })
    }

    /// The saved link for an album, searching YouTube only when there is no
    /// album video yet.
    pub async fn generate_link(
        &self,
        request: YouTubeLinkGenerateRequest,
    ) -> Result<YouTubeLink, LinkError> {
        let album_id = album_id(&request.album_id)?;
        if let Some(saved) = self.store.link(&album_id).await?
            && saved.video_id.is_some()
        {
            return Ok(saved);
        }
        let video_id = self
            .search
            .search_video(&request.artist_name, &request.album_name)
            .await?
            .ok_or_else(|| {
                LinkError::NotFound(format!(
                    "No YouTube video found for '{} - {}'",
                    request.artist_name, request.album_name
                ))
            })?;
        let album = AlbumRef {
            album_id,
            album_name: request.album_name,
            artist_name: request.artist_name,
            cover_url: request.cover_url,
        };
        Ok(self
            .store
            .save_link(album, video_id, LinkOrigin::Search)
            .await?)
    }

    /// Save a video a person pasted.
    pub async fn save_manual_link(
        &self,
        request: YouTubeManualLinkRequest,
    ) -> Result<YouTubeLink, LinkError> {
        let video_id = video_id_from(&request.youtube_url)?;
        let album_id = match request.album_id.as_deref().map(str::trim) {
            Some(id) if !id.is_empty() => album_id(id)?,
            _ => self.manual_album_id(),
        };
        let album = AlbumRef {
            album_id,
            album_name: request.album_name,
            artist_name: request.artist_name,
            cover_url: request.cover_url,
        };
        Ok(self
            .store
            .save_link(album, video_id, LinkOrigin::Manual)
            .await?)
    }

    /// Edit a saved link. Blank names keep the saved ones, as in v2.
    pub async fn update_link(
        &self,
        album_id: &str,
        request: YouTubeLinkUpdateRequest,
    ) -> Result<YouTubeLink, LinkError> {
        let video_id = match request.youtube_url.as_deref() {
            Some(url) if !url.trim().is_empty() => Some(video_id_from(url)?),
            _ => None,
        };
        let edit = LinkEdit {
            video_id,
            album_name: request.album_name.filter(|name| !name.is_empty()),
            artist_name: request.artist_name.filter(|name| !name.is_empty()),
            cover_url: request.cover_url,
        };
        self.store
            .edit_link(album_id.to_owned(), edit)
            .await?
            .ok_or_else(|| {
                LinkError::NotFound(format!("No YouTube link found for album '{album_id}'"))
            })
    }

    /// One album's saved link, if any.
    pub async fn link(&self, album_id: &str) -> Result<Option<YouTubeLink>, LinkError> {
        Ok(self.store.link(album_id).await?)
    }

    /// Every saved album link, newest first.
    pub async fn links(&self) -> Result<Vec<YouTubeLink>, LinkError> {
        Ok(self.store.links().await?)
    }

    /// Remove an album link and its track links.
    pub async fn delete_link(&self, album_id: &str) -> Result<(), LinkError> {
        Ok(self.store.delete_link(album_id.to_owned()).await?)
    }

    /// One album's track links, in disc then track order.
    pub async fn track_links(&self, album_id: &str) -> Result<Vec<YouTubeTrackLink>, LinkError> {
        Ok(self.store.track_links(album_id).await?)
    }

    /// Remove one track link.
    pub async fn delete_track_link(
        &self,
        album_id: &str,
        disc_number: i64,
        track_number: i64,
    ) -> Result<(), LinkError> {
        Ok(self
            .store
            .delete_track_link(album_id.to_owned(), disc_number, track_number)
            .await?)
    }

    /// The saved video for one track, searching YouTube when there is none.
    pub async fn generate_track_link(
        &self,
        request: YouTubeTrackLinkGenerateRequest,
    ) -> Result<YouTubeTrackLink, LinkError> {
        let album_id = album_id(&request.album_id)?;
        let saved = self.store.track_links(&album_id).await?;
        if let Some(link) = saved.into_iter().find(|link| {
            link.disc_number == request.disc_number && link.track_number == request.track_number
        }) {
            return Ok(link);
        }
        let video_id = self
            .search
            .search_track(&request.artist_name, &request.track_name)
            .await?
            .ok_or_else(|| {
                LinkError::NotFound(format!(
                    "No YouTube video found for '{} - {}'",
                    request.track_name, request.artist_name
                ))
            })?;
        let album = AlbumRef {
            album_id,
            album_name: request.album_name,
            artist_name: request.artist_name,
            cover_url: request.cover_url,
        };
        let track = NewTrackLink {
            disc_number: request.disc_number,
            track_number: request.track_number,
            track_name: request.track_name,
            video_id,
        };
        self.store
            .save_track_links(album, vec![track])
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| {
                LinkError::Store(StoreError::Internal(
                    "saved youtube track link missing".to_owned(),
                ))
            })
    }

    /// Find videos for many tracks. Tracks with a saved video keep it; the
    /// rest are searched one by one. Once search is switched off or the
    /// budget runs out, the remaining tracks fail with that reason without
    /// asking YouTube again.
    pub async fn generate_track_links(
        &self,
        request: YouTubeTrackLinkBatchGenerateRequest,
    ) -> Result<(Vec<YouTubeTrackLink>, Vec<YouTubeTrackLinkFailure>), LinkError> {
        let album_id = album_id(&request.album_id)?;
        if request.tracks.len() > BATCH_TRACKS_MAX {
            return Err(LinkError::Invalid(format!(
                "At most {BATCH_TRACKS_MAX} tracks can be generated at once"
            )));
        }
        let saved = self.store.track_links(&album_id).await?;
        let mut kept = Vec::new();
        let mut found = Vec::new();
        let mut failed = Vec::new();
        let mut stop: Option<String> = None;
        for track in request.tracks {
            if let Some(link) = saved.iter().find(|link| {
                link.disc_number == track.disc_number && link.track_number == track.track_number
            }) {
                kept.push(link.clone());
                continue;
            }
            if let Some(reason) = &stop {
                failed.push(failure(&track, reason));
                continue;
            }
            match self
                .search
                .search_track(&request.artist_name, &track.track_name)
                .await
            {
                Ok(Some(video_id)) => found.push(NewTrackLink {
                    disc_number: track.disc_number,
                    track_number: track.track_number,
                    track_name: track.track_name,
                    video_id,
                }),
                Ok(None) => failed.push(failure(&track, NO_VIDEO_FOUND)),
                Err(ProviderFailure::Failed(cause)) => {
                    tracing::warn!(%cause, album_id, "youtube track search failed");
                    failed.push(failure(&track, SEARCH_FAILED));
                }
                Err(
                    ProviderFailure::NotConfigured(reason)
                    | ProviderFailure::NotBuilt(reason)
                    | ProviderFailure::Exhausted(reason),
                ) => {
                    failed.push(failure(&track, &reason));
                    stop = Some(reason);
                }
            }
        }
        if kept.is_empty() && found.is_empty() {
            return Ok((Vec::new(), failed));
        }
        let album = AlbumRef {
            album_id,
            album_name: request.album_name,
            artist_name: request.artist_name,
            cover_url: request.cover_url,
        };
        let stored = self.store.save_track_links(album, found).await?;
        kept.extend(stored);
        Ok((kept, failed))
    }

    /// A made-up album id for a manual link with no album behind it.
    fn manual_album_id(&self) -> String {
        let short: String = self
            .ids
            .new_id()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(12)
            .collect();
        format!("manual-{short}")
    }
}

fn failure(track: &YouTubeTrackInput, reason: &str) -> YouTubeTrackLinkFailure {
    YouTubeTrackLinkFailure {
        disc_number: track.disc_number,
        track_number: track.track_number,
        track_name: track.track_name.clone(),
        reason: reason.to_owned(),
    }
}

fn quota_status(quota: crate::reads::discover::models::YouTubeQuotaResponse) -> YouTubeQuotaStatus {
    YouTubeQuotaStatus {
        used: quota.used,
        limit: quota.limit,
        remaining: (quota.limit - quota.used).max(0),
        date: utc_today(),
    }
}

/// A trimmed, non-empty album id of sane length.
fn album_id(raw: &str) -> Result<String, LinkError> {
    let id = raw.trim();
    if id.is_empty() || id.len() > ALBUM_ID_MAX {
        return Err(LinkError::Invalid(format!(
            "album_id must be 1 to {ALBUM_ID_MAX} characters"
        )));
    }
    Ok(id.to_owned())
}

fn video_id_from(url: &str) -> Result<String, LinkError> {
    extract_video_id(url).ok_or_else(|| {
        LinkError::Invalid("Invalid YouTube URL: could not extract a video ID".to_owned())
    })
}

/// The 11-character video id in a YouTube URL, or the input itself when it
/// is a bare id. Takes watch, short, embed, shorts and live links, on
/// youtube.com (any subdomain), youtube-nocookie.com and youtu.be.
pub fn extract_video_id(input: &str) -> Option<String> {
    let input = input.trim();
    if is_video_id(input) {
        return Some(input.to_owned());
    }
    let address = input.split_once("://").map_or(input, |(_, rest)| rest);
    let (host, path) = address.split_once('/')?;
    let host = host.to_ascii_lowercase();
    let on = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    let candidate = if on("youtu.be") {
        path
    } else if on("youtube.com") || on("youtube-nocookie.com") {
        match path.strip_prefix("watch?") {
            Some(query) => query
                .split('#')
                .next()?
                .split('&')
                .find_map(|pair| pair.strip_prefix("v="))?,
            None => ["embed/", "shorts/", "live/", "v/"]
                .iter()
                .find_map(|prefix| path.strip_prefix(prefix))?,
        }
    } else {
        return None;
    };
    let id = candidate.split(['?', '&', '#', '/']).next()?;
    is_video_id(id).then(|| id.to_owned())
}

fn is_video_id(text: &str) -> bool {
    text.len() == 11
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::extract_video_id;

    #[test]
    fn video_ids_come_out_of_every_link_shape() {
        let id = Some("dQw4w9WgXcQ".to_owned());
        for link in [
            "dQw4w9WgXcQ",
            " https://www.youtube.com/watch?v=dQw4w9WgXcQ ",
            "https://music.youtube.com/watch?list=PL1&v=dQw4w9WgXcQ&t=3",
            "youtube.com/embed/dQw4w9WgXcQ?start=1",
            "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ?si=abc",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
        ] {
            assert_eq!(extract_video_id(link), id, "{link}");
        }
        for bad in [
            "",
            "https://example.com/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/watch?xv=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQextra",
            "dQw4w9WgXc",
        ] {
            assert_eq!(extract_video_id(bad), None, "{bad}");
        }
    }
}
