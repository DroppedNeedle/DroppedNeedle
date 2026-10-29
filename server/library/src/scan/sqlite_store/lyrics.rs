//! Which tracks carry lyrics in their files, recorded after each scan.
//!
//! [`SqliteScanStore::refresh_track_lyrics`] walks the indexed albums and
//! records in `local_track_lyrics` each track whose file carries lyrics: a
//! `.lrc` file beside it, or a lyrics tag. It reads them with the same
//! function that serves them, so a marked track can be served.
//!
//! It works like the album art sweep and shares its snapshot, folders and
//! signature: an album is read again only when its tracks' stat revisions,
//! its folders' modification times or the size and time of a `.lrc` file
//! in its track folders changed. Adding, removing or renaming a `.lrc`
//! file changes its folder's time; editing one in place changes its own,
//! so a `.lrc` fixed in place is marked and served after the next sweep.
//! Each track folder is listed once per album.
//!
//! The sweep only reads music files and never follows a link. It gives way
//! to scans and shutdown between albums, and after a slice of reading,
//! exactly as the art sweep does.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::params;

use super::artwork::{
    ArtworkSweep, MIN_PER_SLICE, SWEEP_SLICE, Snapshot, TrackFile, WRITE_BATCH, album_dirs,
    album_signature,
};
use super::{Lane, ScanStoreError, SqliteScanStore};
use crate::tags::lyrics::{LyricsSource, Sidecars, read_lyrics_beside};

/// What one sweep did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct LyricsSweep {
    /// Albums whose files changed and were looked at again.
    pub checked: usize,
    /// Tracks of those albums found to carry lyrics.
    pub marked: usize,
    /// False when the sweep stopped early; run it again to finish.
    pub complete: bool,
}

/// One album's result, waiting to be written.
struct Checked {
    album_id: String,
    signature: String,
    marks: Vec<(String, LyricsSource)>,
}

impl SqliteScanStore {
    /// The album art sweep then the lyrics sweep, over one read of the
    /// catalog.
    pub fn refresh_album_sweeps(
        &self,
        stop: &dyn Fn() -> bool,
    ) -> Result<(ArtworkSweep, LyricsSweep), ScanStoreError> {
        let snapshot = self.sweep_snapshot()?;
        let artwork = self.sweep_artwork(&snapshot, stop);
        let lyrics = self.sweep_lyrics(&snapshot, stop);
        Ok((artwork, lyrics))
    }

    /// Record which tracks carry lyrics, for every album whose files
    /// changed since the last sweep. File reads happen outside the store
    /// lock; writes go in small transactions. `stop` is polled between
    /// albums. A catalog that cannot be read is an error; a batch that
    /// cannot be written is logged and looked at again next sweep.
    pub fn refresh_track_lyrics(
        &self,
        stop: &dyn Fn() -> bool,
    ) -> Result<LyricsSweep, ScanStoreError> {
        Ok(self.sweep_lyrics(&self.sweep_snapshot()?, stop))
    }

    fn sweep_lyrics(&self, snapshot: &Snapshot, stop: &dyn Fn() -> bool) -> LyricsSweep {
        let mut started: Option<std::time::Instant> = None;
        let mut sweep = LyricsSweep {
            complete: true,
            ..LyricsSweep::default()
        };
        let mut pending = Vec::new();
        for (album_id, tracks) in &snapshot.albums {
            let slice_spent = started.is_some_and(|at| at.elapsed() >= SWEEP_SLICE)
                && sweep.checked >= MIN_PER_SLICE;
            if stop() || slice_spent {
                sweep.complete = false;
                break;
            }
            let sidecars = track_sidecars(tracks);
            let signature = lyrics_signature(tracks, &sidecars);
            if snapshot.lyrics_checks.get(album_id) == Some(&signature) {
                continue;
            }
            started.get_or_insert_with(std::time::Instant::now);
            sweep.checked += 1;
            let marks = tracks
                .iter()
                .filter_map(|track| {
                    let sidecar = track
                        .path
                        .parent()
                        .and_then(|dir| sidecars.get(dir))
                        .and_then(|listed| listed.for_track(&track.path));
                    read_lyrics_beside(&track.path, sidecar)
                        .map(|(source, _)| (track.id.clone(), source))
                })
                .collect();
            pending.push(Checked {
                album_id: album_id.clone(),
                signature,
                marks,
            });
            if pending.len() >= WRITE_BATCH {
                self.write_lyrics(&mut pending, &mut sweep);
            }
        }
        self.write_lyrics(&mut pending, &mut sweep);
        if sweep.checked > 0 {
            tracing::info!(
                checked = sweep.checked,
                marked = sweep.marked,
                complete = sweep.complete,
                "lyrics sweep finished"
            );
        }
        sweep
    }

    /// Write one batch of results and clear it. A failed batch is logged
    /// and dropped; its albums carry no new check row, so the next sweep
    /// looks at them again.
    fn write_lyrics(&self, pending: &mut Vec<Checked>, sweep: &mut LyricsSweep) {
        if pending.is_empty() {
            return;
        }
        let now = self.clock.now_ms();
        let written = self.write_once(Lane::Background, "scan_lyrics", |tx| {
            let mut marked = 0;
            for checked in pending.iter() {
                tx.execute(
                    "DELETE FROM local_track_lyrics WHERE local_track_id IN \
                     (SELECT id FROM local_tracks WHERE local_album_id = ?1)",
                    params![checked.album_id],
                )?;
                for (track_id, source) in &checked.marks {
                    marked += tx.execute(
                        "INSERT INTO local_track_lyrics (local_track_id, source) \
                         SELECT ?1, ?2 WHERE EXISTS (SELECT 1 FROM local_tracks WHERE id = ?1) \
                         ON CONFLICT(local_track_id) DO UPDATE SET source = excluded.source",
                        params![track_id, source.as_str()],
                    )?;
                }
                tx.execute(
                    "INSERT INTO local_album_lyrics_checks (local_album_id, signature, checked_at) \
                     SELECT ?1, ?2, ?3 WHERE EXISTS (SELECT 1 FROM local_albums WHERE id = ?1) \
                     ON CONFLICT(local_album_id) DO UPDATE SET \
                     signature = excluded.signature, checked_at = excluded.checked_at",
                    params![checked.album_id, checked.signature, now],
                )?;
            }
            Ok(marked)
        });
        match written {
            Ok(marked) => sweep.marked += marked,
            Err(error) => {
                tracing::warn!(%error, albums = pending.len(), "lyrics batch not saved; retried next sweep");
            }
        }
        pending.clear();
    }
}

/// The `.lrc` files of each folder the album's tracks live in, each folder
/// listed once.
fn track_sidecars(tracks: &[TrackFile]) -> HashMap<PathBuf, Sidecars> {
    let mut listed: HashMap<PathBuf, Sidecars> = HashMap::new();
    for track in tracks {
        if let Some(dir) = track.path.parent()
            && !listed.contains_key(dir)
        {
            listed.insert(dir.to_path_buf(), Sidecars::list(dir));
        }
    }
    listed
}

/// The album signature with every `.lrc` file of its track folders, so an
/// edit in place brings the album back.
fn lyrics_signature(tracks: &[TrackFile], sidecars: &HashMap<PathBuf, Sidecars>) -> String {
    let dirs = album_dirs(tracks);
    let mut files: Vec<&Path> = sidecars.values().flat_map(Sidecars::paths).collect();
    files.sort();
    album_signature(tracks, &dirs, &files)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.lrc` rewritten in place leaves its folder's time alone, so only
    /// its own stat can bring the album back to the sweep.
    #[test]
    fn a_lrc_edited_in_place_changes_the_signature() {
        let dir = std::env::temp_dir().join(format!("dn-lyrics-sign-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lrc = dir.join("01 Song.LRC");
        std::fs::write(&lrc, "").unwrap();
        let tracks = [TrackFile {
            id: "t1".to_owned(),
            path: dir.join("01 Song.flac"),
            stat_revision: "r1".to_owned(),
        }];
        let folder_time = |dir: &Path| std::fs::metadata(dir).unwrap().modified().unwrap();
        let before_time = folder_time(&dir);
        let before = lyrics_signature(&tracks, &track_sidecars(&tracks));

        std::fs::write(&lrc, "[00:01.00]Fixed\n").unwrap();
        assert_eq!(folder_time(&dir), before_time, "the folder reads unchanged");
        let after = lyrics_signature(&tracks, &track_sidecars(&tracks));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_ne!(before, after);
    }
}
