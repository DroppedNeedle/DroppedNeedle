//! Runs the [`IDENTIFY`] jobs a playlist file import queues: each album
//! its kept songs name is proven through [`AlbumResolver`], and what is
//! found is written back onto the songs, so the link pass points them at
//! the library once the album lands.
//!
//! The songs are grouped by their album name and first credited artist,
//! folded, and each group is proven alone. A run is safe to repeat: it
//! reads only songs that still have no album, and the resolver keeps its
//! answers in the metadata cache. A MusicBrainz failure puts the job back
//! with its backoff and leaves the songs as they are; MusicBrainz settings
//! that do not read hold it until they are saved again. A playlist that is
//! gone has no songs, and the job is done.

use std::sync::Arc;
use std::time::Duration;

use droppedneedle_catalog::collections::db::StoreError;
use droppedneedle_catalog::collections::identify_job::IdentifyJob;
use droppedneedle_catalog::collections::store::PlaylistStore;
use droppedneedle_catalog::collections::store::playlists::KeptEntry;
use droppedneedle_kernel::db::fold_text;
use droppedneedle_kernel::jobs::BoxFuture;
use droppedneedle_kernel::jobs::queue::{JobCtx, JobHandler, Outcome, PAYLOAD_INVALID};
use droppedneedle_kernel::runtime_config::{Live, sections::MusicBrainzSettings};
use droppedneedle_providers::ProviderError;

use super::spotify_albums::{AlbumResolver, FileAlbum, FileSong, title_key};

/// MusicBrainz did not answer; the job tries again later.
pub const MUSICBRAINZ_UNAVAILABLE: &str = "MUSICBRAINZ_UNAVAILABLE";
/// The playlist's songs could not be read or written.
pub const STORE_FAILED: &str = "PLAYLIST_STORE_FAILED";

/// The longest a job waits for the MusicBrainz settings to be saved before
/// it reads them again. A save wakes it at once.
const SETTINGS_RECHECK: Duration = Duration::from_secs(300);

/// Finds the albums of the songs playlist file imports kept.
pub struct PlaylistIdentifyJobs {
    playlists: PlaylistStore,
    resolver: Arc<AlbumResolver>,
    settings: Live<MusicBrainzSettings>,
}

impl PlaylistIdentifyJobs {
    /// Jobs writing to `playlists`, proving albums through `resolver`,
    /// which reads its source from `settings`.
    pub fn new(
        playlists: PlaylistStore,
        resolver: Arc<AlbumResolver>,
        settings: Live<MusicBrainzSettings>,
    ) -> Self {
        Self {
            playlists,
            resolver,
            settings,
        }
    }
}

/// One album the songs name, and its songs' entries.
struct Named {
    album: FileAlbum,
    entries: Vec<KeptEntry>,
}

/// The songs grouped by their album: the album name and the first
/// credited artist, folded, in playlist order.
fn by_album(entries: Vec<KeptEntry>) -> Vec<Named> {
    let mut named: Vec<(String, String, Named)> = Vec::new();
    for entry in entries {
        let first_credit = entry.artist.split([',', ';']).next().unwrap_or_default();
        let (album_key, artist_key) = (
            fold_text(entry.album.trim()),
            fold_text(first_credit.trim()),
        );
        let song = FileSong {
            title: entry.title.trim().to_owned(),
            isrc: entry.isrc.clone(),
        };
        match named
            .iter_mut()
            .find(|(album, artist, _)| *album == album_key && *artist == artist_key)
        {
            Some((_, _, group)) => {
                group.album.songs.push(song);
                group.entries.push(entry);
            }
            None => {
                let album = FileAlbum {
                    title: entry.album.trim().to_owned(),
                    artist: entry.artist.trim().to_owned(),
                    songs: vec![song],
                };
                named.push((
                    album_key,
                    artist_key,
                    Named {
                        album,
                        entries: vec![entry],
                    },
                ));
            }
        }
    }
    named.into_iter().map(|(_, _, named)| named).collect()
}

/// A store failure as the job's answer: busy goes back without spending
/// the attempt.
fn store_outcome(error: &StoreError) -> Outcome {
    match error {
        StoreError::Busy(_) => Outcome::Busy,
        StoreError::Internal(cause) => {
            tracing::warn!(%cause, "playlist identify store call failed");
            Outcome::Retry {
                code: STORE_FAILED,
                at: None,
            }
        }
    }
}

impl JobHandler for PlaylistIdentifyJobs {
    fn run(&self, job: JobCtx) -> BoxFuture<'static, Outcome> {
        let (playlists, resolver) = (self.playlists.clone(), self.resolver.clone());
        let mut changes = self.settings.subscribe();
        Box::pin(async move {
            let Ok(IdentifyJob { playlist_id }) = job.payload::<IdentifyJob>() else {
                return Outcome::Fail {
                    code: PAYLOAD_INVALID,
                };
            };
            let entries = match playlists.entries_to_identify(&playlist_id).await {
                Ok(entries) => entries,
                Err(error) => return store_outcome(&error),
            };
            for Named { album, entries } in by_album(entries) {
                let found = match resolver.resolve_file_album(&album).await {
                    Ok(Some(found)) => found,
                    Ok(None) => continue,
                    // The settings read logs its own failure, once per
                    // revision. The job waits for a save, then goes back
                    // without spending its attempt.
                    Err(ProviderError::NotConfigured { .. }) => {
                        tokio::select! {
                            _ = changes.changed() => {}
                            () = job.token().cancelled() => {}
                            () = tokio::time::sleep(SETTINGS_RECHECK) => {}
                        }
                        return Outcome::Busy;
                    }
                    Err(error) => {
                        tracing::warn!(%error, "playlist album lookup failed; trying again later");
                        return Outcome::Retry {
                            code: MUSICBRAINZ_UNAVAILABLE,
                            at: None,
                        };
                    }
                };
                let songs: Vec<(String, Option<String>)> = entries
                    .into_iter()
                    .map(|entry| {
                        let recording = found.recordings.get(&title_key(&entry.title)).cloned();
                        (entry.entry_id, recording)
                    })
                    .collect();
                if let Err(error) = playlists
                    .set_entries_album(&playlist_id, &found.group, songs)
                    .await
                {
                    return store_outcome(&error);
                }
            }
            Outcome::Done
        })
    }
}
