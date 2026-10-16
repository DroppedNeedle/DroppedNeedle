//! Playlists: CRUD, entry order, visibility, covers, remote imports.
//!
//! Ownership rules: anyone authenticated lists; owners see full rows for
//! their playlists and everyone sees full rows for public ones. Other users'
//! private playlists redact to existence plus count plus owner. Mutations are
//! owner-only: private plus non-owner is 404 (the row stays hidden), public
//! plus non-owner is 403. Admins hold no extra playlist rights.

use std::collections::HashMap;

use super::CollectionsService;
use crate::reads::collections::error::CollectionsError;
use crate::reads::collections::models::{
    AddTracksBody, AddTracksResponse, CheckTracksBody, CheckTracksResponse, CoverUploadBody,
    CreatePlaylistBody, PlaylistDetail, PlaylistListItem, PlaylistListResponse, PlaylistSummary,
    PlaylistTrack, RedactedPlaylist, ResolveSourcesResponse, TrackInput, UpdatePlaylistBody,
    UpdateTrackBody,
};
use crate::reads::collections::store::playlists::{CoverImage, NewEntry, PlaylistRow, Written};

/// Longest accepted playlist name.
const MAX_NAME_LEN: usize = 200;
/// Decoded cover cap: 5 MiB.
const MAX_COVER_BYTES: usize = 5 * 1024 * 1024;
/// Cover mime allowlist.
const COVER_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];
/// Source type of an entry added from the local library by a compat client.
pub const LOCAL_SOURCE: &str = "droppedneedle-local";
/// Accepted source types and what they normalize to (v2 aliases; `howler`
/// is the old name of the local source).
const SOURCE_TYPES: [(&str, &str); 8] = [
    ("", ""),
    ("local", "local"),
    ("howler", "local"),
    ("jellyfin", "jellyfin"),
    ("navidrome", "navidrome"),
    ("plex", "plex"),
    ("youtube", "youtube"),
    (LOCAL_SOURCE, LOCAL_SOURCE),
];

/// One playlist as one caller may see it.
#[derive(Debug, Clone)]
pub struct Visible {
    /// The row with aggregates.
    pub row: PlaylistRow,
    /// True when the caller owns it.
    pub is_owner: bool,
    /// False for another user's private playlist: only id, count and
    /// owner may be shown.
    pub full: bool,
}

/// Trimmed non-empty name or 400.
fn clean_name(raw: &str) -> Result<String, CollectionsError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(CollectionsError::invalid("Playlist name must not be blank"));
    }
    if name.len() > MAX_NAME_LEN {
        return Err(CollectionsError::invalid("Playlist name is too long"));
    }
    Ok(name.to_owned())
}

/// Normalized source type or 400.
fn clean_source(raw: &str) -> Result<String, CollectionsError> {
    SOURCE_TYPES
        .iter()
        .find(|(alias, _)| *alias == raw)
        .map(|(_, canonical)| (*canonical).to_owned())
        .ok_or_else(|| CollectionsError::InvalidInput {
            message: format!("Invalid source type '{raw}'"),
        })
}

fn clean_sources(raw: &[String]) -> Result<Vec<String>, CollectionsError> {
    raw.iter().map(|source| clean_source(source)).collect()
}

/// Canonical cover URL for a playlist.
fn cover_url(playlist_id: &str) -> String {
    format!("/api/v3/playlists/{playlist_id}/cover")
}

fn summary(row: &PlaylistRow, user_id: &str) -> PlaylistSummary {
    PlaylistSummary {
        id: row.id.clone(),
        name: row.name.clone(),
        track_count: row.track_count,
        total_duration: row.total_duration,
        cover_urls: row.cover_urls.clone(),
        custom_cover_url: row.has_cover.then(|| cover_url(&row.id)),
        source_ref: row.source_ref.clone(),
        created_at: row.created_at,
        updated_at: row.updated_at,
        is_public: row.is_public,
        is_owner: owns(row, user_id),
        owner_name: row.owner_name.clone(),
        is_redacted: false,
    }
}

fn detail(row: &PlaylistRow, user_id: &str, tracks: Vec<PlaylistTrack>) -> PlaylistDetail {
    let summary = summary(row, user_id);
    PlaylistDetail {
        id: summary.id,
        name: summary.name,
        cover_urls: summary.cover_urls,
        custom_cover_url: summary.custom_cover_url,
        source_ref: summary.source_ref,
        tracks,
        track_count: summary.track_count,
        total_duration: summary.total_duration,
        created_at: summary.created_at,
        updated_at: summary.updated_at,
        is_public: summary.is_public,
        is_owner: summary.is_owner,
        owner_name: summary.owner_name,
        is_redacted: false,
    }
}

fn owns(row: &PlaylistRow, user_id: &str) -> bool {
    row.owner_id.as_deref() == Some(user_id)
}

/// Owner-only guard: private plus non-owner is 404, public plus non-owner
/// is 403.
fn require_owner(row: &PlaylistRow, user_id: &str) -> Result<(), CollectionsError> {
    if owns(row, user_id) {
        Ok(())
    } else if row.is_public {
        Err(CollectionsError::Forbidden {
            message: "Only the owner can change this playlist".to_owned(),
        })
    } else {
        Err(CollectionsError::NotFound)
    }
}

/// A store write that names a playlist or entry: gone means 404.
fn found<T>(written: Written<T>) -> Result<T, CollectionsError> {
    match written {
        Written::Done(value) => Ok(value),
        Written::Missing => Err(CollectionsError::NotFound),
    }
}

/// Validate one native track input and turn it into a store entry. Local
/// entries whose source id names a library file link to it, so compat
/// clients can stream them (v2 #181).
fn to_entry(input: &TrackInput) -> Result<NewEntry, CollectionsError> {
    for (label, value) in [
        ("track_name", &input.track_name),
        ("artist_name", &input.artist_name),
        ("album_name", &input.album_name),
    ] {
        if value.trim().is_empty() {
            return Err(CollectionsError::InvalidInput {
                message: format!("Track {label} must not be blank"),
            });
        }
    }
    let source_type = clean_source(&input.source_type)?;
    let available_sources = input
        .available_sources
        .as_deref()
        .map(clean_sources)
        .transpose()?;
    let library_file_id = match (&input.track_source_id, source_type.as_str()) {
        (Some(id), "local" | LOCAL_SOURCE) if !id.is_empty() => Some(id.clone()),
        _ => None,
    };
    Ok(NewEntry {
        track_name: input.track_name.clone(),
        artist_name: input.artist_name.clone(),
        album_name: input.album_name.clone(),
        album_id: input.album_id.clone(),
        artist_id: input.artist_id.clone(),
        track_source_id: input.track_source_id.clone(),
        cover_url: input.cover_url.clone(),
        source_type,
        available_sources,
        format: input.format.clone(),
        track_number: input.track_number,
        disc_number: input.disc_number,
        duration: input.duration,
        plex_rating_key: input.plex_rating_key.clone(),
        library_file_id,
    })
}

impl CollectionsService<'_> {
    /// Every playlist with what the caller may see of it, name order.
    pub async fn visible_playlists(&self, user_id: &str) -> Result<Vec<Visible>, CollectionsError> {
        let rows = self.state.stores.playlists.list().await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let is_owner = owns(&row, user_id);
                Visible {
                    full: is_owner || row.is_public,
                    is_owner,
                    row,
                }
            })
            .collect())
    }

    /// The native list: full summaries plus redacted stubs.
    pub async fn list_playlists(
        &self,
        user_id: &str,
    ) -> Result<PlaylistListResponse, CollectionsError> {
        let playlists = self
            .visible_playlists(user_id)
            .await?
            .into_iter()
            .map(|visible| {
                if visible.full {
                    PlaylistListItem::Full(summary(&visible.row, user_id))
                } else {
                    PlaylistListItem::Redacted(RedactedPlaylist {
                        id: visible.row.id.clone(),
                        track_count: visible.row.track_count,
                        owner_name: visible.row.owner_name.clone(),
                        is_redacted: true,
                    })
                }
            })
            .collect();
        Ok(PlaylistListResponse { playlists })
    }

    /// One playlist the caller may read. Private plus non-owner is 404:
    /// detail callers get the full row or nothing.
    pub async fn readable_playlist(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<PlaylistRow, CollectionsError> {
        let row = self
            .state
            .stores
            .playlists
            .get(playlist_id)
            .await?
            .ok_or(CollectionsError::NotFound)?;
        if owns(&row, user_id) || row.is_public {
            Ok(row)
        } else {
            Err(CollectionsError::NotFound)
        }
    }

    /// One playlist the caller owns, for a mutation.
    async fn owned_playlist(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<PlaylistRow, CollectionsError> {
        let row = self
            .state
            .stores
            .playlists
            .get(playlist_id)
            .await?
            .ok_or(CollectionsError::NotFound)?;
        require_owner(&row, user_id)?;
        Ok(row)
    }

    /// Entries of a readable playlist, in order.
    pub async fn playlist_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<Vec<PlaylistTrack>, CollectionsError> {
        self.readable_playlist(user_id, playlist_id).await?;
        Ok(self.state.stores.playlists.tracks(playlist_id).await?)
    }

    /// One playlist detail.
    pub async fn get_playlist(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<PlaylistDetail, CollectionsError> {
        let row = self.readable_playlist(user_id, playlist_id).await?;
        let tracks = self.state.stores.playlists.tracks(playlist_id).await?;
        Ok(detail(&row, user_id, tracks))
    }

    /// Create a playlist owned by the caller; returns its id. A second
    /// import of the same source for the same user is a conflict.
    pub async fn create_named(
        &self,
        user_id: &str,
        name: &str,
        source_ref: Option<&str>,
    ) -> Result<String, CollectionsError> {
        let name = clean_name(name)?;
        self.state
            .stores
            .playlists
            .create(user_id, &name, source_ref)
            .await?
            .ok_or_else(|| CollectionsError::Conflict {
                message: "This playlist was already imported".to_owned(),
            })
    }

    /// Create a playlist (native body).
    pub async fn create_playlist(
        &self,
        user_id: &str,
        body: &CreatePlaylistBody,
    ) -> Result<PlaylistDetail, CollectionsError> {
        let id = self
            .create_named(user_id, &body.name, body.source_ref.as_deref())
            .await?;
        self.get_playlist(user_id, &id).await
    }

    /// Rename a playlist. A missing name leaves it unchanged.
    pub async fn update_playlist(
        &self,
        user_id: &str,
        playlist_id: &str,
        body: &UpdatePlaylistBody,
    ) -> Result<PlaylistDetail, CollectionsError> {
        let name = body.name.as_deref().map(clean_name).transpose()?;
        self.owned_playlist(user_id, playlist_id).await?;
        if let Some(name) = name {
            found(
                self.state
                    .stores
                    .playlists
                    .rename(playlist_id, &name)
                    .await?,
            )?;
        }
        self.get_playlist(user_id, playlist_id).await
    }

    /// Delete a playlist.
    pub async fn delete_playlist(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<(), CollectionsError> {
        self.owned_playlist(user_id, playlist_id).await?;
        found(self.state.stores.playlists.delete(playlist_id).await?)
    }

    /// Flip visibility.
    pub async fn set_visibility(
        &self,
        user_id: &str,
        playlist_id: &str,
        public: bool,
    ) -> Result<PlaylistSummary, CollectionsError> {
        self.owned_playlist(user_id, playlist_id).await?;
        found(
            self.state
                .stores
                .playlists
                .set_public(playlist_id, public)
                .await?,
        )?;
        let row = self.readable_playlist(user_id, playlist_id).await?;
        Ok(summary(&row, user_id))
    }

    /// Insert ready-made entries at a position (None appends); returns the
    /// new entry ids.
    pub async fn add_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
        position: Option<usize>,
        entries: Vec<NewEntry>,
    ) -> Result<Vec<String>, CollectionsError> {
        self.owned_playlist(user_id, playlist_id).await?;
        found(
            self.state
                .stores
                .playlists
                .insert(playlist_id, position, entries)
                .await?,
        )
    }

    /// Add native tracks at a position, or append.
    pub async fn add_tracks(
        &self,
        user_id: &str,
        playlist_id: &str,
        body: &AddTracksBody,
    ) -> Result<AddTracksResponse, CollectionsError> {
        let entries = body
            .tracks
            .iter()
            .map(to_entry)
            .collect::<Result<Vec<_>, _>>()?;
        let added = self
            .add_entries(user_id, playlist_id, body.position, entries)
            .await?;
        let tracks = self
            .state
            .stores
            .playlists
            .tracks(playlist_id)
            .await?
            .into_iter()
            .filter(|track| added.contains(&track.id))
            .collect();
        Ok(AddTracksResponse { tracks })
    }

    /// Remove entries by id, skipping unknown ids; returns how many went.
    pub async fn remove_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_ids: &[String],
    ) -> Result<usize, CollectionsError> {
        self.owned_playlist(user_id, playlist_id).await?;
        found(
            self.state
                .stores
                .playlists
                .remove(playlist_id, entry_ids)
                .await?,
        )
    }

    /// Remove one entry; an unknown entry is 404.
    pub async fn remove_entry(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_id: &str,
    ) -> Result<(), CollectionsError> {
        let removed = self
            .remove_entries(user_id, playlist_id, &[entry_id.to_owned()])
            .await?;
        if removed == 0 {
            return Err(CollectionsError::NotFound);
        }
        Ok(())
    }

    /// Move one entry, clamping past-the-end; returns where it landed.
    pub async fn move_entry(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_id: &str,
        position: usize,
    ) -> Result<usize, CollectionsError> {
        self.owned_playlist(user_id, playlist_id).await?;
        found(
            self.state
                .stores
                .playlists
                .move_entry(playlist_id, entry_id, position)
                .await?,
        )
    }

    /// Update one entry's source fields.
    pub async fn update_track(
        &self,
        user_id: &str,
        playlist_id: &str,
        entry_id: &str,
        body: &UpdateTrackBody,
    ) -> Result<PlaylistTrack, CollectionsError> {
        let source_type = body.source_type.as_deref().map(clean_source).transpose()?;
        let sources = body
            .available_sources
            .as_deref()
            .map(clean_sources)
            .transpose()?;
        self.owned_playlist(user_id, playlist_id).await?;
        found(
            self.state
                .stores
                .playlists
                .update_sources(playlist_id, entry_id, source_type, sources)
                .await?,
        )?;
        self.state
            .stores
            .playlists
            .tracks(playlist_id)
            .await?
            .into_iter()
            .find(|track| track.id == entry_id)
            .ok_or(CollectionsError::NotFound)
    }

    /// Which of the caller's readable playlists hold each queried track,
    /// matched on exact name, artist and album.
    pub async fn check_track_membership(
        &self,
        user_id: &str,
        body: &CheckTracksBody,
    ) -> Result<CheckTracksResponse, CollectionsError> {
        let readable = self
            .visible_playlists(user_id)
            .await?
            .into_iter()
            .filter(|visible| visible.full)
            .map(|visible| visible.row.id)
            .collect::<Vec<_>>();
        let mut membership = HashMap::new();
        for (index, query) in body.tracks.iter().enumerate() {
            let holding = self
                .state
                .stores
                .playlists
                .holding(
                    &readable,
                    &query.track_name,
                    &query.artist_name,
                    &query.album_name,
                )
                .await?;
            membership.insert(index.to_string(), holding);
        }
        Ok(CheckTracksResponse { membership })
    }

    /// Each entry's known sources.
    pub async fn resolve_sources(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<ResolveSourcesResponse, CollectionsError> {
        let sources = self
            .playlist_entries(user_id, playlist_id)
            .await?
            .into_iter()
            .map(|track| (track.id, track.available_sources.unwrap_or_default()))
            .collect();
        Ok(ResolveSourcesResponse { sources })
    }

    /// Store a cover from base64 bytes; returns its URL.
    pub async fn upload_cover(
        &self,
        user_id: &str,
        playlist_id: &str,
        body: &CoverUploadBody,
    ) -> Result<String, CollectionsError> {
        use base64::Engine as _;
        if !COVER_TYPES.contains(&body.content_type.as_str()) {
            return Err(CollectionsError::invalid(
                "Cover must be png, jpeg, or webp",
            ));
        }
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(body.image_base64.trim())
            .map_err(|_| CollectionsError::invalid("Cover is not valid base64"))?;
        if bytes.len() > MAX_COVER_BYTES {
            return Err(CollectionsError::invalid("Cover exceeds the 5 MiB limit"));
        }
        if bytes.is_empty() {
            return Err(CollectionsError::invalid("Cover must not be empty"));
        }
        self.owned_playlist(user_id, playlist_id).await?;
        found(
            self.state
                .stores
                .playlists
                .set_cover(
                    playlist_id,
                    CoverImage {
                        content_type: body.content_type.clone(),
                        bytes,
                    },
                )
                .await?,
        )?;
        Ok(cover_url(playlist_id))
    }

    /// Cover bytes of a readable playlist; no cover is 404.
    pub async fn cover(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<CoverImage, CollectionsError> {
        self.readable_playlist(user_id, playlist_id).await?;
        self.state
            .stores
            .playlists
            .cover(playlist_id)
            .await?
            .ok_or(CollectionsError::NotFound)
    }

    /// Delete the cover; no cover is 404.
    pub async fn remove_cover(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<(), CollectionsError> {
        self.owned_playlist(user_id, playlist_id).await?;
        found(
            self.state
                .stores
                .playlists
                .remove_cover(playlist_id)
                .await?,
        )
    }

    /// Import a remote playlist into the owner's playlists, keyed by its
    /// provenance (`<source>:<id>`). A remote playlist already imported by
    /// this user is not copied again (v2): the answer names the existing
    /// copy. Returns the local id and whether it already existed.
    pub async fn import_playlist(
        &self,
        owner_id: &str,
        source_ref: &str,
        name: &str,
        entries: Vec<NewEntry>,
    ) -> Result<(String, bool), CollectionsError> {
        let playlists = &self.state.stores.playlists;
        if let Some(existing) = playlists.find_by_source(owner_id, source_ref).await? {
            return Ok((existing, true));
        }
        let name = clean_name(name).or_else(|_| clean_name("Imported playlist"))?;
        let Some(id) = playlists.create(owner_id, &name, Some(source_ref)).await? else {
            // A concurrent import won the race; answer with its copy.
            let existing = playlists
                .find_by_source(owner_id, source_ref)
                .await?
                .ok_or(CollectionsError::NotFound)?;
            return Ok((existing, true));
        };
        if !entries.is_empty() {
            found(playlists.insert(&id, None, entries).await?)?;
        }
        Ok((id, false))
    }
}
