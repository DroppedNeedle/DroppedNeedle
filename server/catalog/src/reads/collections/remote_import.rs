//! Remote playlist imports into the user's playlists: the collections side
//! of the media servers' import port.

use std::collections::HashSet;

use super::CollectionsState;
use super::models::PlaylistListItem;
use super::service::CollectionsService;
use super::store::playlists::NewEntry;
use droppedneedle_media::remotes::adapter::{BoxFuture, ImportReceipt, ImportSink};
use droppedneedle_media::remotes::models::{SourceName, TrackView};

/// Imports into the user's native playlists, so an imported Plex,
/// Navidrome or Jellyfin playlist shows on the playlists page and in the
/// compat clients. Each entry keeps the remote id it streams from (the
/// Plex part key for Plex, the item id elsewhere); Plex tracks without a
/// part key cannot play and count as failed (v2).
#[derive(Clone)]
pub struct PlaylistImportSink {
    collections: CollectionsState,
}

impl PlaylistImportSink {
    /// Sink over the shared collections.
    pub fn new(collections: CollectionsState) -> Self {
        Self { collections }
    }
}

impl ImportSink for PlaylistImportSink {
    fn import<'a>(
        &'a self,
        owner_id: &'a str,
        source: SourceName,
        remote_playlist_id: &'a str,
        playlist_name: &'a str,
        tracks: Vec<TrackView>,
    ) -> BoxFuture<'a, Result<ImportReceipt, String>> {
        Box::pin(async move {
            let source_type = source.as_str().to_owned();
            let mut failed = 0_i64;
            let entries = tracks
                .into_iter()
                .filter_map(|track| {
                    let (source_id, rating_key) = if source == SourceName::Plex {
                        match track.part_key.clone() {
                            Some(part) => (part, Some(track.id.clone())),
                            None => {
                                failed += 1;
                                return None;
                            }
                        }
                    } else {
                        (track.id.clone(), None)
                    };
                    Some(NewEntry {
                        track_name: track.title,
                        artist_name: track.artist_name,
                        album_name: track.album_name,
                        album_id: track.album_id,
                        artist_id: track.artist_id,
                        track_source_id: Some(source_id),
                        cover_url: track.image_url,
                        source_type: source_type.clone(),
                        available_sources: Some(vec![source_type.clone()]),
                        format: None,
                        track_number: track.track_number,
                        disc_number: track.disc_number,
                        duration: track.duration_secs.map(|secs| secs as f64),
                        plex_rating_key: rating_key,
                        library_file_id: None,
                    })
                })
                .collect::<Vec<_>>();
            let imported = entries.len() as i64;
            let source_ref = format!("{}:{remote_playlist_id}", source.as_str());
            let (local_playlist_id, already_imported) = CollectionsService::new(&self.collections)
                .import_playlist(owner_id, &source_ref, playlist_name, entries)
                .await
                .map_err(|error| format!("playlist import failed: {error:?}"))?;
            Ok(ImportReceipt {
                local_playlist_id,
                tracks_imported: if already_imported { 0 } else { imported },
                tracks_failed: if already_imported { 0 } else { failed },
                already_imported,
            })
        })
    }

    fn imported_ids<'a>(
        &'a self,
        owner_id: &'a str,
        source: SourceName,
    ) -> BoxFuture<'a, Result<HashSet<String>, String>> {
        Box::pin(async move {
            let prefix = format!("{}:", source.as_str());
            let list = CollectionsService::new(&self.collections)
                .list_playlists(owner_id)
                .await
                .map_err(|error| format!("playlist list failed: {error:?}"))?;
            Ok(list
                .playlists
                .into_iter()
                .filter_map(|item| match item {
                    PlaylistListItem::Full(summary) if summary.is_owner => summary.source_ref,
                    _ => None,
                })
                .filter_map(|source_ref| source_ref.strip_prefix(&prefix).map(str::to_owned))
                .collect())
        })
    }
}
