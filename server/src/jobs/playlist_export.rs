//! Navidrome playlist export: DroppedNeedle playlists written as extended
//! M3U files into a folder Navidrome imports from.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use unicode_normalization::UnicodeNormalization;

use super::playlist_sync::{PlaylistExporter, PlaylistSyncConfig, PlaylistSyncResult};
use super::registry::BoxFuture;

/// File suffix Navidrome imports.
pub const PLAYLIST_SUFFIX: &str = ".m3u8";
/// Ownership marker inside every file we write.
pub const OWNER_MARKER: &str = "#DROPPEDNEEDLE-PLAYLIST-ID:";
/// How far into a file the marker may sit.
const MARKER_SCAN_BYTES: usize = 512;
/// Longest filename stem.
const MAX_STEM: usize = 120;
/// Names Windows refuses, applied everywhere: the folder is often a share.
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// Writes DroppedNeedle playlists as extended M3U files for Navidrome to
/// import (v2 `NavidromePlaylistExportService`). One-way: Navidrome's own
/// playlists are never read back. The folder is not ours alone, so each
/// file carries an ownership marker, a failed export keeps its old file,
/// entries are relative paths, and nothing is remembered between runs.
#[derive(Clone)]
pub struct M3uPlaylistExporter {
    pool: Option<sqlx::SqlitePool>,
    lock: Arc<tokio::sync::Mutex<()>>,
}

/// One playlist row to export.
#[derive(Debug, Clone)]
struct ExportPlaylist {
    id: String,
    name: String,
}

/// One playlist entry joined to its local file.
#[derive(Debug, Clone)]
struct ExportEntry {
    track_name: String,
    artist_name: String,
    duration: Option<i64>,
    file_path: Option<String>,
}

impl PlaylistExporter for M3uPlaylistExporter {
    fn sync(&self, config: PlaylistSyncConfig) -> BoxFuture<'_, PlaylistSyncResult> {
        Box::pin(async move {
            // The route's Sync Now and the loop must not interleave.
            let _guard = self.lock.lock().await;
            self.sync_locked(config).await
        })
    }
}

impl M3uPlaylistExporter {
    /// An exporter over the database; `None` answers "not available".
    pub fn new(pool: Option<sqlx::SqlitePool>) -> Self {
        Self {
            pool,
            lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    async fn sync_locked(&self, config: PlaylistSyncConfig) -> PlaylistSyncResult {
        let mut result = PlaylistSyncResult::default();
        let Some(pool) = &self.pool else {
            result.message = "Playlists are not available in this build.".to_owned();
            return result;
        };
        let directory = PathBuf::from(config.target_dir.trim());
        if config.target_dir.trim().is_empty() {
            result.message = "Set a playlist folder before syncing.".to_owned();
            return result;
        }
        if !directory.is_absolute() {
            result.message = "The playlist folder must be a full path starting with /.".to_owned();
            return result;
        }
        let playlists = match read_playlists(pool, &config.scope).await {
            Ok(playlists) => playlists,
            Err(error) => {
                tracing::error!(%error, "playlist export could not read playlists");
                result.message = "Could not read playlists.".to_owned();
                return result;
            }
        };
        let mut rendered = Vec::new();
        let mut errors = 0_u64;
        for playlist in playlists {
            match read_entries(pool, &playlist.id).await {
                Ok(entries) => rendered.push((playlist, entries)),
                Err(error) => {
                    tracing::error!(%error, playlist = %playlist.id, "playlist export failed to read entries");
                    errors += 1;
                    rendered.push((playlist, Vec::new()));
                }
            }
        }
        let remove_deleted = config.remove_deleted;
        let outcome = tokio::task::spawn_blocking(move || {
            write_playlists(&directory, rendered, remove_deleted)
        })
        .await;
        match outcome {
            Ok(Ok(mut written)) => {
                written.errors += errors;
                written.finish()
            }
            Ok(Err(message)) => {
                result.message = message;
                result
            }
            Err(error) => {
                tracing::error!(%error, "playlist export task failed");
                result.message = "The playlist export stopped unexpectedly.".to_owned();
                result
            }
        }
    }
}

async fn read_playlists(
    pool: &sqlx::SqlitePool,
    scope: &str,
) -> Result<Vec<ExportPlaylist>, sqlx::Error> {
    // Default-deny: only an explicit "all" publishes private playlists.
    let sql = if scope == "all" {
        "SELECT id, name FROM playlists ORDER BY updated_at DESC, id"
    } else {
        "SELECT id, name FROM playlists WHERE is_public = 1 ORDER BY updated_at DESC, id"
    };
    let rows: Vec<(String, String)> = sqlx::query_as(sql).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|(id, name)| ExportPlaylist { id, name })
        .collect())
}

async fn read_entries(
    pool: &sqlx::SqlitePool,
    playlist_id: &str,
) -> Result<Vec<ExportEntry>, sqlx::Error> {
    // Entries with no local file (remote or YouTube tracks) come back with
    // no path so they can be counted, not silently dropped. Collections
    // keeps the local track id in `library_file_id`, and stores durations
    // as float seconds, so the column is read as REAL and truncated (v2).
    let rows: Vec<(String, String, Option<f64>, Option<String>)> = sqlx::query_as(
        "SELECT pt.track_name, pt.artist_name, CAST(pt.duration AS REAL), t.file_path \
         FROM playlist_tracks pt LEFT JOIN local_tracks t ON t.id = pt.library_file_id \
         WHERE pt.playlist_id = ?1 ORDER BY pt.position",
    )
    .bind(playlist_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(
            |(track_name, artist_name, duration, file_path)| ExportEntry {
                track_name,
                artist_name,
                duration: duration.map(|secs| secs as i64),
                file_path,
            },
        )
        .collect())
}

/// Counters for one run; [`Written::finish`] builds the report.
#[derive(Debug, Default)]
struct Written {
    written: u64,
    unchanged: u64,
    removed: u64,
    removal_failures: u64,
    skipped_empty: u64,
    skipped_not_ours: u64,
    tracks_missing_files: u64,
    tracks_unrepresentable: u64,
    errors: u64,
}

impl Written {
    fn finish(self) -> PlaylistSyncResult {
        let mut parts = vec![format!("{} written", self.written)];
        if self.unchanged > 0 {
            parts.push(format!("{} already current", self.unchanged));
        }
        if self.removed > 0 {
            parts.push(format!("{} removed", self.removed));
        }
        if self.skipped_empty > 0 {
            parts.push(format!(
                "{} skipped, no tracks in your library",
                self.skipped_empty
            ));
        }
        if self.skipped_not_ours > 0 {
            parts.push(format!(
                "{} skipped, a file of that name was not created by DroppedNeedle",
                self.skipped_not_ours
            ));
        }
        if self.tracks_missing_files > 0 {
            parts.push(format!(
                "{} tracks not in your library",
                self.tracks_missing_files
            ));
        }
        if self.tracks_unrepresentable > 0 {
            parts.push(format!(
                "{} tracks on a different drive to the playlist folder",
                self.tracks_unrepresentable
            ));
        }
        if self.removal_failures > 0 {
            parts.push(format!(
                "{} could not be removed, will retry next sync",
                self.removal_failures
            ));
        }
        if self.errors > 0 {
            parts.push(format!("{} failed", self.errors));
        }
        PlaylistSyncResult {
            // Not a success while a removal is outstanding: the playlist is
            // still readable in Navidrome.
            success: self.errors == 0 && self.removal_failures == 0,
            message: format!("{}.", parts.join(", ")),
            written: self.written,
            unchanged: self.unchanged,
            removed: self.removed,
            removal_failures: self.removal_failures,
            skipped_empty: self.skipped_empty,
            skipped_not_ours: self.skipped_not_ours,
            tracks_missing_files: self.tracks_missing_files,
            tracks_unrepresentable: self.tracks_unrepresentable,
        }
    }
}

/// Render and write every playlist, then remove our stale exports.
fn write_playlists(
    directory: &Path,
    playlists: Vec<(ExportPlaylist, Vec<ExportEntry>)>,
    remove_deleted: bool,
) -> Result<Written, String> {
    std::fs::create_dir_all(directory)
        .map_err(|error| format!("Cannot create the playlist folder: {error}"))?;
    if !directory_writable(directory) {
        return Err(
            "DroppedNeedle cannot write to the playlist folder. Check it is mounted \
                    into this container and writable by it."
                .to_owned(),
        );
    }
    let mut counts = Written::default();
    let mut current_names = std::collections::HashSet::new();
    let mut failed = std::collections::HashSet::new();
    let mut maintained = 0_u64;
    for (playlist, entries) in playlists {
        let (text, missing, unrepresentable) =
            render_playlist(&playlist.name, &playlist.id, &entries, directory);
        counts.tracks_missing_files += missing;
        counts.tracks_unrepresentable += unrepresentable;
        if missing + unrepresentable == entries.len() as u64 {
            // An empty export reads as data loss, not as a failed export.
            counts.skipped_empty += 1;
            failed.insert(playlist.id);
            continue;
        }
        let filename = safe_playlist_filename(&playlist.name, &playlist.id);
        let destination = directory.join(&filename);
        if destination.exists() && owned_playlist_id(&destination).as_deref() != Some(&playlist.id)
        {
            tracing::warn!(%filename, "playlist export skipped a file that is not DroppedNeedle's");
            counts.skipped_not_ours += 1;
            failed.insert(playlist.id);
            continue;
        }
        if std::fs::read_to_string(&destination).is_ok_and(|existing| existing == text) {
            // Skipping the write avoids churning mtimes and a rescan.
            counts.unchanged += 1;
            maintained += 1;
            current_names.insert(filename);
            continue;
        }
        match write_atomic(&destination, &text) {
            Ok(()) => {
                maintained += 1;
                current_names.insert(filename);
            }
            Err(error) => {
                tracing::warn!(%filename, %error, "playlist export could not write a file");
                counts.errors += 1;
                failed.insert(playlist.id);
            }
        }
    }
    if remove_deleted {
        let (removed, failures) = remove_stale(directory, &current_names, &failed);
        counts.removed = removed;
        counts.removal_failures = failures;
    }
    counts.written = maintained - counts.unchanged;
    Ok(counts)
}

fn directory_writable(directory: &Path) -> bool {
    let probe = directory.join(format!(".droppedneedle-write-probe-{}", random_suffix()));
    let writable = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    writable
}

/// Write to a uniquely named temporary file, then rename over the target:
/// Navidrome may scan mid-write.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("playlist");
    let temporary = path.with_file_name(format!(".{name}.{}.tmp", random_suffix()));
    let written = std::fs::write(&temporary, text).and_then(|()| std::fs::rename(&temporary, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// Delete our exports this run no longer maintains. Candidates come from
/// the folder, not a stored index: each file names its playlist, so a
/// failed delete retries next run. Returns removed and failed counts.
fn remove_stale(
    directory: &Path,
    current_names: &std::collections::HashSet<String>,
    failed: &std::collections::HashSet<String>,
) -> (u64, u64) {
    let Ok(listing) = std::fs::read_dir(directory) else {
        return (0, 0);
    };
    let mut candidates: Vec<PathBuf> = listing
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.ends_with(PLAYLIST_SUFFIX) && !current_names.contains(name)
                    })
        })
        .collect();
    candidates.sort();
    let (mut removed, mut failures) = (0, 0);
    for candidate in candidates {
        let Some(playlist_id) = owned_playlist_id(&candidate) else {
            // Not ours: a hand-made playlist or another tool's.
            continue;
        };
        if failed.contains(&playlist_id) {
            // Its export failed this run, so it is not confirmed gone.
            continue;
        }
        match std::fs::remove_file(&candidate) {
            Ok(()) => removed += 1,
            Err(error) => {
                tracing::warn!(path = %candidate.display(), %error, "playlist export could not remove a stale file");
                failures += 1;
            }
        }
    }
    (removed, failures)
}

/// The playlist id a file declares, or `None` when it is not ours.
fn owned_playlist_id(path: &Path) -> Option<String> {
    use std::io::Read as _;
    let mut head = vec![0u8; MARKER_SCAN_BYTES];
    let read = std::fs::File::open(path)
        .and_then(|mut file| file.read(&mut head))
        .ok()?;
    let head = String::from_utf8_lossy(&head[..read]);
    head.lines()
        .find_map(|line| line.strip_prefix(OWNER_MARKER))
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

/// A portable filename; the id suffix carries uniqueness since names
/// collide.
pub fn safe_playlist_filename(name: &str, playlist_id: &str) -> String {
    let normalized: String = name.nfc().collect();
    let mut stem: String = normalized
        .trim()
        .chars()
        .map(|ch| {
            if matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || ch < ' ' {
                '_'
            } else {
                ch
            }
        })
        .collect();
    stem = stem.trim_matches(|ch| ch == ' ' || ch == '.').to_owned();
    let first = stem.split('.').next().unwrap_or("").to_uppercase();
    if WINDOWS_RESERVED.contains(&first.as_str()) {
        stem = format!("_{stem}");
    }
    if stem.chars().count() > MAX_STEM {
        stem = stem.chars().take(MAX_STEM).collect::<String>();
        stem = stem.trim_end_matches([' ', '.']).to_owned();
    }
    if stem.is_empty() {
        stem = "playlist".to_owned();
    }
    let short_id: String = playlist_id.chars().take(8).collect();
    format!("{stem} [{short_id}]{PLAYLIST_SUFFIX}")
}

/// Render extended M3U. Returns the text plus the counts of entries with
/// no local file and entries whose path cannot be written relative to the
/// playlist folder.
fn render_playlist(
    name: &str,
    playlist_id: &str,
    entries: &[ExportEntry],
    playlist_dir: &Path,
) -> (String, u64, u64) {
    let mut lines = vec![
        "#EXTM3U".to_owned(),
        format!("{OWNER_MARKER}{playlist_id}"),
        format!("#PLAYLIST:{}", one_line(name)),
    ];
    let (mut missing, mut unrepresentable) = (0, 0);
    for entry in entries {
        let Some(track_path) = entry.file_path.as_deref().filter(|path| !path.is_empty()) else {
            missing += 1;
            continue;
        };
        let Some(relative) = relative_entry(Path::new(track_path), playlist_dir) else {
            unrepresentable += 1;
            continue;
        };
        if relative.chars().any(char::is_control) {
            unrepresentable += 1;
            continue;
        }
        let seconds = entry.duration.unwrap_or(-1);
        let artist = one_line(&entry.artist_name);
        let title = one_line(&entry.track_name);
        let label = if artist.is_empty() {
            title
        } else {
            format!("{artist} - {title}")
        };
        lines.push(format!("#EXTINF:{seconds},{label}"));
        lines.push(relative);
    }
    (format!("{}\n", lines.join("\n")), missing, unrepresentable)
}

/// M3U is line-oriented with no escaping: a newline in free text would
/// inject a directive.
fn one_line(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// The track path relative to the playlist folder, `/`-separated. `None`
/// when no relative path exists (a different root or drive). Never an
/// absolute path: that resolves only if Navidrome mounts the library the
/// same way.
fn relative_entry(track: &Path, base: &Path) -> Option<String> {
    if !track.is_absolute() || !base.is_absolute() {
        return None;
    }
    let track: Vec<Component<'_>> = track.components().collect();
    let base: Vec<Component<'_>> = base.components().collect();
    if track.first() != base.first() {
        return None;
    }
    let shared = track
        .iter()
        .zip(base.iter())
        .take_while(|(left, right)| left == right)
        .count();
    let mut parts: Vec<String> = Vec::new();
    for _ in shared..base.len() {
        parts.push("..".to_owned());
    }
    for component in &track[shared..] {
        parts.push(component.as_os_str().to_str()?.to_owned());
    }
    Some(parts.join("/"))
}

fn random_suffix() -> String {
    let mut bytes = [0u8; 4];
    if getrandom::fill(&mut bytes).is_err() {
        bytes = [0x6e, 0x64, 0x6c, 0x21];
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch folder removed on drop.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "dn-playlist-export-{}-{}",
                std::process::id(),
                random_suffix()
            ));
            std::fs::create_dir_all(&path).expect("scratch");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Ours-only removal and relative entries: a stale export of ours goes,
    /// a hand-made playlist stays, and a library file outside the folder is
    /// written as a relative path.
    #[test]
    fn export_writes_relative_entries_and_removes_only_our_stale_files() {
        let dir = Scratch::new();
        let folder = dir.path().join("playlists");
        std::fs::create_dir_all(&folder).expect("folder");
        std::fs::write(folder.join("Mine.m3u8"), "#EXTM3U\nsong.flac\n").expect("hand-made");
        std::fs::write(
            folder.join("Old [deadbeef].m3u8"),
            format!("#EXTM3U\n{OWNER_MARKER}deadbeef-old\n"),
        )
        .expect("stale export");
        let track = dir.path().join("music/A/song.flac");
        let playlists = vec![(
            ExportPlaylist {
                id: "abcdef12-3456".to_owned(),
                name: "Road\nTrip".to_owned(),
            },
            vec![
                ExportEntry {
                    track_name: "Song".to_owned(),
                    artist_name: "Artist".to_owned(),
                    duration: Some(200),
                    file_path: Some(track.display().to_string()),
                },
                ExportEntry {
                    track_name: "Remote".to_owned(),
                    artist_name: "Elsewhere".to_owned(),
                    duration: None,
                    file_path: None,
                },
            ],
        )];
        let report = write_playlists(&folder, playlists, true)
            .expect("export runs")
            .finish();
        assert!(report.success, "{}", report.message);
        assert_eq!(report.written, 1);
        assert_eq!(report.removed, 1);
        assert_eq!(report.tracks_missing_files, 1);
        assert!(folder.join("Mine.m3u8").exists(), "not ours, kept");
        let text = std::fs::read_to_string(folder.join("Road_Trip [abcdef12].m3u8"))
            .expect("export written");
        assert!(text.contains("#PLAYLIST:Road Trip"));
        assert!(text.contains("#EXTINF:200,Artist - Song\n../music/A/song.flac\n"));
    }
}
