//! Lists on rotation as a person reaches them: the switch, what waits its
//! turn, keeping a list on, and saving a copy. The lists themselves are
//! written only by their refresh (`crate::collections::rotation`).

use super::CollectionsService;
use super::playlists::{rotation_read_only, summary};
use crate::collections::error::CollectionsError;
use crate::collections::models::{PlaylistDetail, PlaylistSummary, RotationStatus};
use crate::collections::rotation::{self, RotationIdea, job, store};
use crate::collections::store::playlists::NewEntry;

/// The answer for a playlist that is not a list on rotation.
fn not_on_rotation() -> CollectionsError {
    CollectionsError::Conflict {
        message: "This playlist is not on rotation".to_owned(),
    }
}

/// "25 Nov" for the day `now_ms` falls in, UTC.
fn short_date(now_ms: i64) -> String {
    jiff::Timestamp::from_millisecond(now_ms)
        .map(|at| {
            at.to_zoned(jiff::tz::TimeZone::UTC)
                .strftime("%-d %b")
                .to_string()
        })
        .unwrap_or_default()
}

impl CollectionsService<'_> {
    /// Whether the caller has lists on rotation on, when they next change,
    /// and which ideas wait their turn or have too few songs, from their
    /// stored rows.
    pub async fn rotation_status(&self, user_id: &str) -> Result<RotationStatus, CollectionsError> {
        let (enabled, rows) = store::standing(&self.state.db, user_id).await?;
        let mut waiting = Vec::new();
        let mut thin = Vec::new();
        for idea in RotationIdea::ALL {
            let Some(row) = rows.iter().find(|row| row.idea == idea) else {
                continue;
            };
            if row.playlist_id.is_some() {
                continue;
            }
            if row.match_count >= idea.bar() {
                waiting.push(idea);
            } else if row.match_count > 0 {
                thin.push(idea);
            }
        }
        Ok(RotationStatus {
            enabled,
            next_change_at: rotation::next_change_at(self.state.db.now_ms()),
            waiting,
            thin,
        })
    }

    /// Turn the caller's lists on rotation on or off. Off deletes them; on
    /// queues a refresh that picks them at once.
    pub async fn set_rotation(
        &self,
        user_id: &str,
        enabled: bool,
    ) -> Result<RotationStatus, CollectionsError> {
        let now = self.state.db.now_ms();
        let (queue, user) = (self.state.jobs.clone(), user_id.to_owned());
        store::set_enabled(&self.state.db, user_id, enabled, now, move |tx| {
            Ok(job::queue_refresh(&queue, tx, &user, now)?)
        })
        .await?;
        self.rotation_status(user_id).await
    }

    /// The idea of the caller's own list on rotation. Another person's
    /// playlist answers as [`Self::require_owner`] does; one of theirs that
    /// is not on rotation is a conflict.
    async fn own_rotation(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<RotationIdea, CollectionsError> {
        self.require_owner(user_id, playlist_id).await?;
        store::idea_of(&self.state.db, user_id, playlist_id)
            .await?
            .ok_or_else(not_on_rotation)
    }

    /// Keep the caller's list on rotation on past Monday, or let it go. The
    /// always-on lists stay on by themselves.
    pub async fn pin_rotation(
        &self,
        user_id: &str,
        playlist_id: &str,
        pinned: bool,
    ) -> Result<PlaylistSummary, CollectionsError> {
        let idea = self.own_rotation(user_id, playlist_id).await?;
        if idea.always_on() {
            return Err(CollectionsError::Conflict {
                message: "This list is always on".to_owned(),
            });
        }
        if !store::pin(&self.state.db, user_id, playlist_id, pinned).await? {
            return Err(rotation_read_only());
        }
        let row = self.readable_playlist(user_id, playlist_id).await?;
        Ok(summary(&row, user_id, false, self.state.db.now_ms()))
    }

    /// Save the caller's list on rotation as an ordinary playlist of
    /// theirs, with the same songs, named for today.
    pub async fn copy_rotation(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<PlaylistDetail, CollectionsError> {
        self.own_rotation(user_id, playlist_id).await?;
        let row = self.readable_playlist(user_id, playlist_id).await?;
        let entries = self
            .playlists()
            .owned_by(user_id)
            .tracks(playlist_id)
            .await?
            .into_iter()
            .map(|track| NewEntry {
                track_name: track.track_name,
                artist_name: track.artist_name,
                album_name: track.album_name,
                album_id: track.album_id,
                artist_id: track.artist_id,
                track_source_id: track.track_source_id,
                cover_url: track.cover_url,
                source_type: track.source_type,
                available_sources: track.available_sources,
                format: track.format,
                track_number: track.track_number,
                disc_number: track.disc_number,
                duration: track.duration,
                plex_rating_key: track.plex_rating_key,
                library_file_id: track.library_file_id,
                recording_mbid: track.recording_mbid,
                isrc: None,
            })
            .collect();
        let name = format!("{} ({})", row.name, short_date(self.state.db.now_ms()));
        let saved = self.create_with_entries(user_id, &name, entries).await?;
        self.get_playlist(user_id, false, &saved.id).await
    }
}
