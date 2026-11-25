//! Playlists: CRUD, entry order, visibility, covers, remote imports.
//!
//! Ownership rules: anyone authenticated lists; owners see full rows for
//! their playlists and everyone sees full rows for public ones. Other users'
//! private playlists redact to existence plus count plus owner, and opening
//! one answers that same stub. Mutations are owner-only: private plus
//! non-owner is 404 (the row stays hidden), public plus non-owner is 403.
//! Admins hold one extra right: deleting a playlist with no owner. A private
//! one is listed to admins only, as a stub; to anyone else it does not exist.

use std::collections::{HashMap, HashSet};

use droppedneedle_auth::users::roles::{Admin, Principal};

use super::CollectionsService;
use super::favorites::library_error;
use crate::collections::db::StoreError;
use crate::collections::error::CollectionsError;
use crate::collections::import;
use crate::collections::models::{
    AddTracksBody, AddTracksResponse, CheckTracksBody, CheckTracksResponse, CoverUploadBody,
    CreatePlaylistBody, ExportFormat, ImportPlaylistBody, ImportPlaylistResponse,
    ImportUnmatchedReason as Reason, MembersResponse, PlaylistDetail, PlaylistListItem,
    PlaylistListResponse, PlaylistSummary, PlaylistTrack, RedactedPlaylist, ResolveSourcesResponse,
    TrackInput, UpdatePlaylistBody, UpdateTrackBody,
};
use crate::collections::sql::PlaylistExportRow;
use crate::collections::store::PlaylistStore;
pub use crate::collections::store::playlists::SavedPlaylist;
use crate::collections::store::playlists::{
    CoverImage, EntryNames, Listed, NewEntry, PlaylistRow, Written,
};
use crate::local::player::PlayerTrack;
use crate::metadata::clean_recording;
use droppedneedle_kernel::playlist_files::{self, Entry};

/// Longest accepted playlist name.
const MAX_NAME_LEN: usize = 200;
/// Decoded cover cap: 5 MiB.
pub(crate) const MAX_COVER_BYTES: usize = 5 * 1024 * 1024;
/// Cover mime allowlist.
const COVER_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/webp"];
/// Accepted source types and what they normalize to. `howler` and
/// `droppedneedle-local` are older names of the local library source.
const SOURCE_TYPES: [(&str, &str); 8] = [
    ("", ""),
    ("local", "local"),
    ("howler", "local"),
    ("droppedneedle-local", "local"),
    ("jellyfin", "jellyfin"),
    ("navidrome", "navidrome"),
    ("plex", "plex"),
    ("youtube", "youtube"),
];

/// Most tracks one membership check accepts.
const MAX_CHECK_TRACKS: usize = 1000;
/// Longest accepted export path prefix, characters.
const MAX_EXPORT_PREFIX_LEN: usize = 200;

/// One playlist the caller may read.
#[derive(Debug, Clone)]
pub struct Visible {
    /// The row with aggregates.
    pub row: PlaylistRow,
    /// True when the caller owns it.
    pub is_owner: bool,
}

/// One playlist rendered for download.
#[derive(Debug, Clone)]
pub struct PlaylistExport {
    /// The playlist's name, safe as a filename stem.
    pub file_stem: String,
    /// The rendered file.
    pub body: String,
    /// Entries left out because they have no local file.
    pub skipped: usize,
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

/// Normalized source types, each once, in their first order, or 400.
fn clean_sources(raw: &[String]) -> Result<Vec<String>, CollectionsError> {
    let mut sources = Vec::with_capacity(raw.len());
    for source in raw {
        let source = clean_source(source)?;
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    Ok(sources)
}

/// Trimmed export path prefix without its trailing slash; `None` when
/// empty. Control characters are refused: a line-oriented format cannot
/// carry them.
fn clean_export_prefix(raw: Option<&str>) -> Result<Option<String>, CollectionsError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let prefix = raw.trim();
    if prefix.chars().any(char::is_control) {
        return Err(CollectionsError::invalid(
            "Path prefix must not contain control characters",
        ));
    }
    if prefix.chars().count() > MAX_EXPORT_PREFIX_LEN {
        return Err(CollectionsError::invalid("Path prefix is too long"));
    }
    let prefix = prefix.trim_end_matches('/');
    Ok((!prefix.is_empty()).then(|| prefix.to_owned()))
}

/// Canonical cover URL for a playlist.
fn cover_url(playlist_id: &str) -> String {
    format!("/api/v3/playlists/{playlist_id}/cover")
}

/// Who may delete a playlist: its owner, and an admin when it has no owner.
fn can_delete(owner_id: Option<&str>, user_id: &str, admin: bool) -> bool {
    owner_id.map_or(admin, |owner| owner == user_id)
}

fn summary(row: &PlaylistRow, user_id: &str, admin: bool) -> PlaylistSummary {
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
        is_shared: owns(row, user_id) && row.has_members,
        can_edit: owns(row, user_id) || row.shared_to_edit,
        can_delete: can_delete(row.owner_id.as_deref(), user_id, admin),
        owner_name: if owns(row, user_id) {
            row.owner_name.clone()
        } else {
            row.owner_listed_name.clone()
        },
        is_redacted: false,
    }
}

fn detail(
    row: &PlaylistRow,
    user_id: &str,
    admin: bool,
    tracks: Vec<PlaylistTrack>,
) -> PlaylistDetail {
    let summary = summary(row, user_id, admin);
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
        is_shared: summary.is_shared,
        can_edit: summary.can_edit,
        can_delete: summary.can_delete,
        owner_name: summary.owner_name,
        is_redacted: false,
    }
}

fn owns(row: &PlaylistRow, user_id: &str) -> bool {
    row.owner_id.as_deref() == Some(user_id)
}

/// Validate one native track input and turn it into a store entry. Local
/// entries whose source id names a library file link to it, so compat
/// clients can stream them.
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
        (Some(id), "local") if !id.is_empty() => Some(id.clone()),
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
        recording_mbid: input.recording_mbid.as_deref().and_then(clean_recording),
        isrc: None,
    })
}

/// One row of an import, in file order.
enum Planned<'a> {
    /// It matched a live track, by id.
    Matched(&'a import::ParsedEntry, String),
    /// It names a title and an artist the library lacks, so it is kept.
    Kept(&'a import::ParsedEntry),
}

/// The entry an unmatched row becomes: the names and numbers the file
/// gave, with no source.
fn kept_entry(entry: &import::ParsedEntry) -> NewEntry {
    NewEntry {
        track_name: entry.title.trim().to_owned(),
        artist_name: entry.artist.trim().to_owned(),
        album_name: entry.album.trim().to_owned(),
        album_id: None,
        artist_id: None,
        track_source_id: None,
        cover_url: None,
        source_type: String::new(),
        available_sources: None,
        format: None,
        track_number: entry.track_number,
        disc_number: entry.disc_number,
        duration: None,
        plex_rating_key: None,
        library_file_id: None,
        recording_mbid: None,
        isrc: entry.isrc.clone(),
    }
}

/// The playlist entry for one matched library track: the file's names, its
/// album's artwork when the album has some, its local id and its recording
/// id, as a local track the web UI adds.
fn import_entry(track: &PlayerTrack) -> NewEntry {
    let cover_url = track.cover_available.then(|| {
        let version = track
            .art_version
            .map(|version| format!("&v={version}"))
            .unwrap_or_default();
        format!(
            "/api/v3/library/albums/{}/artwork?size=250{version}",
            track.album_id
        )
    });
    NewEntry {
        track_name: track.title.clone(),
        artist_name: track.artist_name.clone(),
        album_name: track.album_title.clone(),
        album_id: Some(track.album_id.clone()),
        artist_id: track.artist_id.clone(),
        track_source_id: Some(track.id.clone()),
        cover_url,
        source_type: "local".to_owned(),
        available_sources: Some(vec!["local".to_owned()]),
        format: Some(track.format.clone()),
        track_number: i32::try_from(track.track_number).ok(),
        disc_number: i32::try_from(track.disc_number).ok(),
        duration: track.duration_seconds,
        plex_rating_key: None,
        library_file_id: Some(track.id.clone()),
        recording_mbid: track.recording_mbid.as_deref().and_then(clean_recording),
        isrc: None,
    }
}

impl CollectionsService<'_> {
    fn playlists(&self) -> &PlaylistStore {
        &self.state.stores.playlists
    }

    /// Ok when the caller owns the playlist. Otherwise private plus
    /// non-owner (or gone) is 404, public plus non-owner is 403.
    async fn require_owner(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<(), CollectionsError> {
        match self
            .playlists()
            .visible_to(user_id)
            .is_owner(playlist_id)
            .await?
        {
            Some(true) => Ok(()),
            Some(false) => Err(CollectionsError::Forbidden {
                message: "Only the owner can change this playlist".to_owned(),
            }),
            None => Err(CollectionsError::NotFound),
        }
    }

    /// The outcome of an owner-scoped write. A write that found nothing
    /// answers as [`Self::require_owner`] does, and 404 for the caller's
    /// own playlist (the entry is gone).
    async fn found<T>(
        &self,
        user_id: &str,
        playlist_id: &str,
        written: Written<T>,
    ) -> Result<T, CollectionsError> {
        match written {
            Written::Done(value) => Ok(value),
            Written::Missing => {
                self.require_owner(user_id, playlist_id).await?;
                Err(CollectionsError::NotFound)
            }
        }
    }

    /// The outcome of a song write. A write that found nothing is 403 for
    /// a reader who may not edit, and 404 otherwise: the playlist or entry
    /// is gone, or the caller may not read it.
    async fn found_edit<T>(
        &self,
        user_id: &str,
        playlist_id: &str,
        written: Written<T>,
    ) -> Result<T, CollectionsError> {
        match written {
            Written::Done(value) => Ok(value),
            Written::Missing => match self
                .playlists()
                .visible_to(user_id)
                .may_edit(playlist_id)
                .await?
            {
                Some(false) => Err(CollectionsError::Forbidden {
                    message: "You can't change this playlist's songs".to_owned(),
                }),
                _ => Err(CollectionsError::NotFound),
            },
        }
    }

    /// The playlist as the caller reads it, when they may change its songs
    /// but do not own it. Apps send a playlist's name and visibility back
    /// with every song save, so when the owner's write refuses such a
    /// caller, a value already as asked is no change; any other change
    /// stays the owner's.
    async fn editor_row(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<Option<PlaylistRow>, CollectionsError> {
        Ok(self
            .playlists()
            .visible_to(user_id)
            .get(playlist_id)
            .await?
            .filter(|row| row.shared_to_edit && !owns(row, user_id)))
    }

    /// The playlists the caller may read, name order.
    pub async fn visible_playlists(&self, user_id: &str) -> Result<Vec<Visible>, CollectionsError> {
        let rows = self.playlists().visible_to(user_id).readable().await?;
        Ok(rows
            .into_iter()
            .map(|row| Visible {
                is_owner: owns(&row, user_id),
                row,
            })
            .collect())
    }

    /// The native list: full summaries plus redacted stubs. `admin` adds the
    /// private playlists with no owner, which only an admin can act on.
    pub async fn list_playlists(
        &self,
        user_id: &str,
        admin: bool,
    ) -> Result<PlaylistListResponse, CollectionsError> {
        let playlists = self
            .playlists()
            .visible_to(user_id)
            .list(admin)
            .await?
            .into_iter()
            .map(|listed| match listed {
                Listed::Readable(row) => PlaylistListItem::Full(summary(&row, user_id, admin)),
                Listed::Hidden {
                    id,
                    owner_id,
                    track_count,
                    owner_name,
                } => PlaylistListItem::Redacted(RedactedPlaylist {
                    can_delete: can_delete(owner_id.as_deref(), user_id, admin),
                    id,
                    track_count,
                    owner_name,
                    is_redacted: true,
                }),
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
        self.playlists()
            .visible_to(user_id)
            .get(playlist_id)
            .await?
            .ok_or(CollectionsError::NotFound)
    }

    /// Entries of a readable playlist, in order.
    pub async fn playlist_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<Vec<PlaylistTrack>, CollectionsError> {
        self.readable_playlist(user_id, playlist_id).await?;
        Ok(self
            .playlists()
            .visible_to(user_id)
            .tracks(playlist_id)
            .await?)
    }

    /// Entries of a playlist the caller may read, in order; none when they
    /// may not read it. For callers that have read the playlist already.
    pub async fn readable_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<Vec<PlaylistTrack>, CollectionsError> {
        Ok(self
            .playlists()
            .visible_to(user_id)
            .tracks(playlist_id)
            .await?)
    }

    /// Entries of a playlist the caller may change the songs of, in order;
    /// None when they may not or it is gone.
    pub async fn editable_entries(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<Option<Vec<PlaylistTrack>>, CollectionsError> {
        let visible = self.playlists().visible_to(user_id);
        if visible.may_edit(playlist_id).await? != Some(true) {
            return Ok(None);
        }
        Ok(Some(visible.tracks(playlist_id).await?))
    }

    /// One playlist the caller may read, rendered as `format`. Entries
    /// with no local file are left out; `skipped` counts them. `prefix`
    /// goes before each path, for a player that stores music elsewhere.
    pub async fn export_playlist(
        &self,
        user_id: &str,
        playlist_id: &str,
        format: ExportFormat,
        prefix: Option<&str>,
    ) -> Result<PlaylistExport, CollectionsError> {
        let prefix = clean_export_prefix(prefix)?;
        let row = self.readable_playlist(user_id, playlist_id).await?;
        let entries: Vec<PlaylistExportRow> = self
            .playlists()
            .visible_to(user_id)
            .export_entries(playlist_id)
            .await?;
        let mut kept: Vec<(&PlaylistExportRow, String)> = Vec::new();
        let mut skipped = 0;
        for entry in &entries {
            let Some(path) = entry
                .relative_path
                .as_deref()
                .and_then(playlist_files::writable_path)
            else {
                skipped += 1;
                continue;
            };
            let path = match &prefix {
                Some(prefix) => format!("{prefix}/{path}"),
                None => path.to_owned(),
            };
            kept.push((entry, path));
        }
        let rendered: Vec<Entry<'_>> = kept
            .iter()
            .map(|(entry, path)| Entry {
                title: &entry.track_name,
                artist: &entry.artist_name,
                album: &entry.album_name,
                album_artist: entry.album_artist_name.as_deref().unwrap_or(""),
                duration_seconds: entry.duration_ms.map(|ms| ms / 1000),
                path,
            })
            .collect();
        let body = match format {
            ExportFormat::M3u8 => playlist_files::render_m3u(None, &row.name, &rendered),
            ExportFormat::Csv => playlist_files::render_csv(&rendered),
        };
        Ok(PlaylistExport {
            file_stem: playlist_files::safe_playlist_stem(&row.name),
            body,
            skipped,
        })
    }

    /// One playlist detail. `admin` says the caller may delete it when it
    /// has no owner; it lets them read nothing more.
    pub async fn get_playlist(
        &self,
        user_id: &str,
        admin: bool,
        playlist_id: &str,
    ) -> Result<PlaylistDetail, CollectionsError> {
        let row = self.readable_playlist(user_id, playlist_id).await?;
        let tracks = self
            .playlists()
            .visible_to(user_id)
            .tracks(playlist_id)
            .await?;
        Ok(detail(&row, user_id, admin, tracks))
    }

    /// The stub the caller's list shows for a playlist they may not read.
    /// None when the list does not show it as one: readable, unknown, or
    /// private with no owner and the caller is not an admin.
    pub async fn private_stub(
        &self,
        user_id: &str,
        admin: bool,
        playlist_id: &str,
    ) -> Result<Option<RedactedPlaylist>, CollectionsError> {
        let listed = self.list_playlists(user_id, admin).await?;
        Ok(listed.playlists.into_iter().find_map(|item| match item {
            PlaylistListItem::Redacted(stub) if stub.id == playlist_id => Some(stub),
            _ => None,
        }))
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
        self.playlists()
            .owned_by(user_id)
            .create(&name, source_ref)
            .await?
            .ok_or_else(|| CollectionsError::Conflict {
                message: "This playlist was already imported".to_owned(),
            })
    }

    /// Create a playlist owned by the caller holding `entries`, as one
    /// write.
    pub async fn create_with_entries(
        &self,
        user_id: &str,
        name: &str,
        entries: Vec<NewEntry>,
    ) -> Result<SavedPlaylist, CollectionsError> {
        let name = clean_name(name)?;
        let (id, at) = self
            .playlists()
            .owned_by(user_id)
            .create_with(&name, entries)
            .await?;
        Ok(SavedPlaylist {
            id,
            name,
            is_public: false,
            created_at: at,
            updated_at: at,
        })
    }

    /// Import a playlist file into a new private playlist of the caller's.
    /// Matched tracks are inserted in file order; the first occurrence of
    /// a track wins and later ones are reported.
    pub async fn import_playlist_file(
        &self,
        user_id: &str,
        body: &ImportPlaylistBody,
    ) -> Result<ImportPlaylistResponse, CollectionsError> {
        let name = clean_name(&body.name)?;
        if body.content.len() > import::MAX_IMPORT_BYTES {
            return Err(CollectionsError::invalid(
                "Playlist file is larger than 2 MB",
            ));
        }
        let import::Parsed {
            entries,
            over_limit,
        } = import::parse(body.format, &body.content);
        let identities = self
            .state
            .items
            .track_identities()
            .await
            .map_err(library_error)?;
        let index = import::ImportIndex::new(identities);
        let mut planned: Vec<Planned> = Vec::new();
        let mut seen = HashSet::new();
        let mut unmatched = Vec::new();
        for entry in &entries {
            if entry.path.trim().is_empty()
                && entry.title.trim().is_empty()
                && entry.artist.trim().is_empty()
            {
                unmatched.push(import::unmatched(entry, Reason::NamesNothing));
                continue;
            }
            if let Some(id) = index.find(entry) {
                if !seen.insert(id.to_owned()) {
                    unmatched.push(import::unmatched(entry, Reason::Duplicate));
                    continue;
                }
                planned.push(Planned::Matched(entry, id.to_owned()));
                continue;
            }
            // A row that names a title and an artist becomes an entry with
            // no source, so an import can carry songs the library lacks,
            // unless the import asked for library songs only.
            if !body.library_only
                && !entry.title.trim().is_empty()
                && !entry.artist.trim().is_empty()
            {
                planned.push(Planned::Kept(entry));
                continue;
            }
            unmatched.push(import::unmatched(entry, Reason::NoMatch));
        }
        let ids: Vec<String> = planned
            .iter()
            .filter_map(|planned| match planned {
                Planned::Matched(_, id) => Some(id.clone()),
                Planned::Kept(_) => None,
            })
            .collect();
        let tracks = self
            .state
            .items
            .tracks_by_ids(&ids)
            .await
            .map_err(library_error)?;
        let by_id: HashMap<String, PlayerTrack> = tracks
            .into_iter()
            .map(|track| (track.id.clone(), track))
            .collect();
        let mut new_entries = Vec::with_capacity(planned.len());
        let mut kept = 0_usize;
        let mut kept_an_album = false;
        for planned in planned {
            match planned {
                Planned::Matched(entry, id) => match by_id.get(&id) {
                    Some(track) => new_entries.push(import_entry(track)),
                    None => unmatched.push(import::unmatched(entry, Reason::NoMatch)),
                },
                Planned::Kept(entry) => {
                    let entry = kept_entry(entry);
                    kept_an_album |= !entry.album_name.is_empty();
                    new_entries.push(entry);
                    kept += 1;
                }
            }
        }
        let matched = new_entries.len() - kept;
        let playlists = self.playlists();
        let owned = playlists.owned_by(user_id);
        // A kept song that names its album is looked up in the background.
        let (playlist_id, _) = if kept_an_album {
            owned
                .create_identifying(&name, new_entries, &self.state.jobs)
                .await?
        } else {
            owned.create_with(&name, new_entries).await?
        };
        Ok(ImportPlaylistResponse {
            playlist_id,
            name,
            matched,
            kept,
            unmatched,
            over_limit,
        })
    }

    /// Rewrite the `served` entries of a playlist (the ones a player app
    /// was shown) as `entries` and, when `name` is given, rename it, as one
    /// write. Entries the app was not shown stay where they are, and a
    /// served entry of a file still listed keeps its id.
    pub async fn rewrite_served(
        &self,
        user_id: &str,
        playlist_id: &str,
        name: Option<&str>,
        served: Vec<String>,
        entries: Vec<NewEntry>,
    ) -> Result<SavedPlaylist, CollectionsError> {
        let name = name.map(clean_name).transpose()?;
        let written = self
            .playlists()
            .owned_by(user_id)
            .rewrite_served_named(
                playlist_id,
                name.as_deref(),
                served.clone(),
                entries.clone(),
            )
            .await?;
        if let Written::Missing = written
            && let Some(row) = self.editor_row(user_id, playlist_id).await?
            && name.as_ref().is_none_or(|name| *name == row.name)
        {
            let written = self
                .playlists()
                .editable_by(user_id)
                .rewrite_served(playlist_id, served, entries)
                .await?;
            self.found_edit(user_id, playlist_id, written).await?;
            let row = self.readable_playlist(user_id, playlist_id).await?;
            return Ok(SavedPlaylist {
                id: row.id,
                name: row.name,
                is_public: row.is_public,
                created_at: row.created_at,
                updated_at: row.updated_at,
            });
        }
        self.found(user_id, playlist_id, written).await
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
        self.get_playlist(user_id, false, &id).await
    }

    /// Rename a playlist. A missing name leaves it unchanged.
    pub async fn update_playlist(
        &self,
        user_id: &str,
        playlist_id: &str,
        body: &UpdatePlaylistBody,
    ) -> Result<PlaylistDetail, CollectionsError> {
        match body.name.as_deref() {
            Some(name) => self.rename(user_id, playlist_id, name).await?,
            None => self.require_owner(user_id, playlist_id).await?,
        }
        self.get_playlist(user_id, false, playlist_id).await
    }

    /// Rename a playlist, reading nothing back.
    pub async fn rename(
        &self,
        user_id: &str,
        playlist_id: &str,
        name: &str,
    ) -> Result<(), CollectionsError> {
        let name = clean_name(name)?;
        let written = self
            .playlists()
            .owned_by(user_id)
            .rename(playlist_id, &name)
            .await?;
        if let Written::Missing = written
            && let Some(row) = self.editor_row(user_id, playlist_id).await?
            && row.name == name
        {
            return Ok(());
        }
        self.found(user_id, playlist_id, written).await
    }

    /// Delete a playlist as the signed-in caller: their own, or as an admin
    /// one with no owner. Every other playlist answers as
    /// [`Self::require_owner`] does. Each write checks who holds the row in
    /// its own statement.
    pub async fn delete_as_caller(
        &self,
        caller: &Principal,
        playlist_id: &str,
    ) -> Result<(), CollectionsError> {
        let user_id = caller.user_id();
        let written = self
            .playlists()
            .owned_by(user_id)
            .delete(playlist_id)
            .await?;
        if matches!(written, Written::Missing)
            && let Ok(admin) = Admin::try_from(caller.clone())
            && matches!(
                self.playlists().strays(&admin).delete(playlist_id).await?,
                Written::Done(())
            )
        {
            return Ok(());
        }
        self.found(user_id, playlist_id, written).await
    }

    /// Delete one of the caller's own playlists.
    pub async fn delete_playlist(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<(), CollectionsError> {
        let written = self
            .playlists()
            .owned_by(user_id)
            .delete(playlist_id)
            .await?;
        self.found(user_id, playlist_id, written).await
    }

    /// Flip visibility.
    pub async fn set_visibility(
        &self,
        user_id: &str,
        playlist_id: &str,
        public: bool,
    ) -> Result<PlaylistSummary, CollectionsError> {
        self.set_public(user_id, playlist_id, public).await?;
        let row = self.readable_playlist(user_id, playlist_id).await?;
        Ok(summary(&row, user_id, false))
    }

    /// Flip visibility, reading nothing back.
    pub async fn set_public(
        &self,
        user_id: &str,
        playlist_id: &str,
        public: bool,
    ) -> Result<(), CollectionsError> {
        let written = self
            .playlists()
            .owned_by(user_id)
            .set_public(playlist_id, public)
            .await?;
        if let Written::Missing = written
            && let Some(row) = self.editor_row(user_id, playlist_id).await?
            && row.is_public == public
        {
            return Ok(());
        }
        self.found(user_id, playlist_id, written).await
    }

    /// The people the caller's playlist is shared with.
    pub async fn members(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<MembersResponse, CollectionsError> {
        self.require_owner(user_id, playlist_id).await?;
        let members = self
            .playlists()
            .owned_by(user_id)
            .members(playlist_id)
            .await?;
        Ok(MembersResponse { members })
    }

    /// Share the caller's playlist with exactly `user_ids`, of whom
    /// `editor_ids` may change its songs; empty unshares it. Every id must
    /// name another user, and every editor must be one of them.
    pub async fn set_members(
        &self,
        user_id: &str,
        playlist_id: &str,
        mut user_ids: Vec<String>,
        editor_ids: &[String],
    ) -> Result<MembersResponse, CollectionsError> {
        user_ids.sort();
        user_ids.dedup();
        let unknown = || CollectionsError::invalid("Share only with other users of this server");
        if user_ids.iter().any(|id| id == user_id) {
            return Err(unknown());
        }
        if editor_ids
            .iter()
            .any(|id| user_ids.binary_search(id).is_err())
        {
            return Err(CollectionsError::invalid(
                "Only people it is shared with can edit it",
            ));
        }
        let editors: HashSet<&String> = editor_ids.iter().collect();
        let members = user_ids
            .iter()
            .map(|id| (id.clone(), editors.contains(id)))
            .collect();
        let written = self
            .playlists()
            .owned_by(user_id)
            .set_members(playlist_id, members)
            .await?;
        if !self.found(user_id, playlist_id, written).await? {
            return Err(unknown());
        }
        self.members(user_id, playlist_id).await
    }

    /// Stop seeing a playlist shared with the caller; 404 when it is not
    /// shared with them.
    pub async fn leave(&self, user_id: &str, playlist_id: &str) -> Result<(), CollectionsError> {
        if self
            .playlists()
            .visible_to(user_id)
            .leave(playlist_id)
            .await?
        {
            Ok(())
        } else {
            Err(CollectionsError::NotFound)
        }
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
        let written = self
            .playlists()
            .editable_by(user_id)
            .insert(playlist_id, position, entries)
            .await?;
        self.found_edit(user_id, playlist_id, written).await
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
            .playlists()
            .visible_to(user_id)
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
        let written = self
            .playlists()
            .editable_by(user_id)
            .remove(playlist_id, entry_ids)
            .await?;
        self.found_edit(user_id, playlist_id, written).await
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
        let written = self
            .playlists()
            .editable_by(user_id)
            .move_entry(playlist_id, entry_id, position)
            .await?;
        self.found_edit(user_id, playlist_id, written).await
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
        let written = self
            .playlists()
            .editable_by(user_id)
            .update_sources(playlist_id, entry_id, source_type, sources)
            .await?;
        self.found_edit(user_id, playlist_id, written).await?;
        self.playlists()
            .visible_to(user_id)
            .tracks(playlist_id)
            .await?
            .into_iter()
            .find(|track| track.id == entry_id)
            .ok_or(CollectionsError::NotFound)
    }

    /// Which of the caller's readable playlists hold each queried track,
    /// matched on exact name, artist and album. At most
    /// [`MAX_CHECK_TRACKS`] tracks per check.
    pub async fn check_track_membership(
        &self,
        user_id: &str,
        body: &CheckTracksBody,
    ) -> Result<CheckTracksResponse, CollectionsError> {
        if body.tracks.len() > MAX_CHECK_TRACKS {
            return Err(CollectionsError::invalid("At most 1000 tracks per check"));
        }
        let asked = body
            .tracks
            .iter()
            .map(|query| EntryNames {
                track_name: &query.track_name,
                artist_name: &query.artist_name,
                album_name: &query.album_name,
            })
            .collect::<Vec<_>>();
        let mut membership = (0..asked.len())
            .map(|index| (index.to_string(), Vec::new()))
            .collect::<HashMap<_, _>>();
        for (index, playlist_id) in self.playlists().visible_to(user_id).holding(&asked).await? {
            if let Some(holding) = membership.get_mut(&index.to_string()) {
                holding.push(playlist_id);
            }
        }
        Ok(CheckTracksResponse { membership })
    }

    /// Link every playlist's entries to the library copies it now holds,
    /// as opening the playlist does; how many entries were linked.
    pub async fn link_library_everywhere(&self) -> Result<usize, CollectionsError> {
        let mut linked = 0;
        for (owner, playlist_id) in self.playlists().to_link().await? {
            match self
                .playlists()
                .owned_by(&owner)
                .link_library_copies(&playlist_id)
                .await
            {
                Ok(count) => linked += count,
                Err(StoreError::Busy(operation)) => {
                    return Err(CollectionsError::Busy { operation });
                }
                // One playlist failing holds back none of the others.
                Err(error) => tracing::warn!(
                    playlist_id,
                    %error,
                    "could not link a playlist's entries to the library"
                ),
            }
        }
        Ok(linked)
    }

    /// Each entry's known sources. First, on the caller's own playlist, an
    /// entry with no source yet whose album the library owns is pointed at
    /// the library's copy, so an imported song plays once its album is in.
    pub async fn resolve_sources(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<ResolveSourcesResponse, CollectionsError> {
        // Best effort: the sources read below answer either way, and the
        // next visit links again.
        if let Err(error) = self
            .playlists()
            .owned_by(user_id)
            .link_library_copies(playlist_id)
            .await
        {
            tracing::warn!(%error, "playlist entries not linked to the library");
        }
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
        let cover = CoverImage {
            content_type: body.content_type.clone(),
            bytes,
        };
        let written = self
            .playlists()
            .owned_by(user_id)
            .set_cover(playlist_id, cover)
            .await?;
        self.found(user_id, playlist_id, written).await?;
        Ok(cover_url(playlist_id))
    }

    /// Cover bytes of a readable playlist; no cover is 404.
    pub async fn cover(
        &self,
        user_id: &str,
        playlist_id: &str,
    ) -> Result<CoverImage, CollectionsError> {
        self.playlists()
            .visible_to(user_id)
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
        let written = self
            .playlists()
            .owned_by(user_id)
            .remove_cover(playlist_id)
            .await?;
        self.found(user_id, playlist_id, written).await
    }

    /// Import a remote playlist into the owner's playlists, keyed by its
    /// provenance (`<source>:<id>`). A remote playlist already imported by
    /// this user is not copied again: the answer names the existing
    /// copy. Returns the local id and whether it already existed.
    pub async fn import_playlist(
        &self,
        owner_id: &str,
        source_ref: &str,
        name: &str,
        entries: Vec<NewEntry>,
    ) -> Result<(String, bool), CollectionsError> {
        let playlists = self.playlists().owned_by(owner_id);
        if let Some(existing) = playlists.find_by_source(source_ref).await? {
            return Ok((existing, true));
        }
        let name = clean_name(name).or_else(|_| clean_name("Imported playlist"))?;
        let Some(id) = playlists.create(&name, Some(source_ref)).await? else {
            // A concurrent import won the race; answer with its copy.
            let existing = playlists
                .find_by_source(source_ref)
                .await?
                .ok_or(CollectionsError::NotFound)?;
            return Ok((existing, true));
        };
        if !entries.is_empty() {
            let written = playlists.songs().insert(&id, None, entries).await?;
            self.found(owner_id, &id, written).await?;
        }
        Ok((id, false))
    }
}
