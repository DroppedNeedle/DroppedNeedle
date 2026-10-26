//! Which tracks carry lyrics in their files, recorded after each scan.
//!
//! [`SqliteScanStore::refresh_track_lyrics`] walks the indexed albums and
//! records in `local_track_lyrics` each track whose file carries lyrics: a
//! `.lrc` file beside it, or a lyrics tag. It reads them with the same
//! function that serves them, so a marked track can be served.
//!
//! It works like the album art sweep and shares its album list, folders and
//! signature: an album is read again only when its tracks' stat revisions
//! or its folders' modification times changed. Adding, removing or renaming
//! a `.lrc` file changes its folder's time, so no new walk is needed.
//! Editing one in place does not; serving always reads the file as it is
//! now, so only an edit that empties the file leaves a stale mark.
//!
//! The sweep only reads music files and never follows a link. It gives way
//! to scans and shutdown between albums, and after a slice of reading,
//! exactly as the art sweep does.

use std::collections::HashMap;

use rusqlite::params;

use super::artwork::{
    MIN_PER_SLICE, SWEEP_SLICE, TrackFile, WRITE_BATCH, album_dirs, album_signature, album_tracks,
};
use super::{Lane, RunStop, ScanStoreError, SqliteScanStore};
use crate::tags::lyrics::{LyricsSource, read_file_lyrics};

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

/// Every live album with its indexed tracks.
type Albums = Vec<(String, Vec<TrackFile>)>;

/// One album's result, waiting to be written.
struct Checked {
    album_id: String,
    signature: String,
    marks: Vec<(String, LyricsSource)>,
}

impl SqliteScanStore {
    /// Record which tracks carry lyrics, for every album whose files
    /// changed since the last sweep. File reads happen outside the store
    /// lock; writes go in small transactions. `stop` is polled between
    /// albums. A catalog that cannot be read is an error; a batch that
    /// cannot be written is logged and looked at again next sweep.
    pub fn refresh_track_lyrics(
        &self,
        stop: &dyn Fn() -> bool,
    ) -> Result<LyricsSweep, ScanStoreError> {
        let mut started: Option<std::time::Instant> = None;
        let (albums, checks) = self.lyrics_snapshot()?;
        let mut sweep = LyricsSweep {
            complete: true,
            ..LyricsSweep::default()
        };
        let mut pending = Vec::new();
        for (album_id, tracks) in &albums {
            let slice_spent = started.is_some_and(|at| at.elapsed() >= SWEEP_SLICE)
                && sweep.checked >= MIN_PER_SLICE;
            if stop() || slice_spent {
                sweep.complete = false;
                break;
            }
            let signature = album_signature(tracks, &album_dirs(tracks), None);
            if checks.get(album_id) == Some(&signature) {
                continue;
            }
            started.get_or_insert_with(std::time::Instant::now);
            sweep.checked += 1;
            let marks = tracks
                .iter()
                .filter_map(|track| {
                    read_file_lyrics(&track.path).map(|(source, _)| (track.id.clone(), source))
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
        Ok(sweep)
    }

    /// The albums with their tracks, and each album's last signature.
    fn lyrics_snapshot(&self) -> Result<(Albums, HashMap<String, String>), ScanStoreError> {
        self.read_long(&RunStop::never(), "scan_lyrics_snapshot", |conn| {
            let albums = album_tracks(conn)?;
            let mut checks = HashMap::new();
            let mut statement =
                conn.prepare("SELECT local_album_id, signature FROM local_album_lyrics_checks")?;
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                checks.insert(row.get(0)?, row.get(1)?);
            }
            Ok((albums, checks))
        })
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
