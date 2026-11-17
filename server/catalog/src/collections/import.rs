//! Playlist import: extended M3U and CSV into entries, and the match
//! against the library's live tracks. No fuzzy matching: a path, or the
//! folded title and artist, or nothing.

use std::collections::HashMap;

use droppedneedle_kernel::db::fold_text;

use super::models::{ImportFormat, ImportUnmatched};
use crate::local::sql::ImportTrack;

/// Largest playlist file one import accepts, bytes.
pub(crate) const MAX_IMPORT_BYTES: usize = 2 * 1024 * 1024;
/// Most entries one import takes; the rest are reported unmatched.
pub(crate) const MAX_IMPORT_ENTRIES: usize = 10_000;
/// The export's own column order, read when a CSV names no known column:
/// title, artist, album, album artist, duration, path.
const EXPORT_COLUMNS: (usize, usize, usize, usize) = (0, 1, 2, 5);
/// Column names that mark the first row as a header.
const KNOWN_COLUMNS: [&str; 6] = [
    "path",
    "title",
    "artist",
    "album",
    "album artist",
    "duration",
];

/// One entry read from a playlist file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedEntry {
    /// Track title, empty when the file names none.
    pub title: String,
    /// Artist, empty when the file names none.
    pub artist: String,
    /// Album, empty when the file names none.
    pub album: String,
    /// The file the entry names, as the file spells it; empty when it
    /// names none.
    pub path: String,
}

/// The entries a file holds, in file order.
pub(crate) fn parse(format: ImportFormat, content: &str) -> Vec<ParsedEntry> {
    match format {
        ImportFormat::M3u8 => parse_m3u(content),
        ImportFormat::Csv => parse_csv(content),
    }
}

/// The report row for one entry that did not become a track.
pub(crate) fn unmatched(entry: &ParsedEntry, reason: &str) -> ImportUnmatched {
    let title = entry.title.trim();
    ImportUnmatched {
        title: if title.is_empty() {
            path_title(&entry.path)
        } else {
            title.to_owned()
        },
        artist: entry.artist.trim().to_owned(),
        reason: reason.to_owned(),
    }
}

/// Extended M3U: an `#EXTINF` line names the next path's artist and title;
/// a path-only file names neither. Other directives are skipped.
fn parse_m3u(content: &str) -> Vec<ParsedEntry> {
    let mut entries = Vec::new();
    let mut pending: Option<(String, String)> = None;
    for raw in content.lines() {
        let line = raw.trim_end_matches('\r').trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            pending = Some(extinf_names(rest));
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let (artist, title) = match pending.take() {
            Some(names) => names,
            None => (String::new(), path_title(line)),
        };
        entries.push(ParsedEntry {
            title,
            artist,
            album: String::new(),
            path: line.to_owned(),
        });
    }
    entries
}

/// The artist and title of an `#EXTINF` tail:
/// `<seconds>,<artist> - <title>`.
fn extinf_names(rest: &str) -> (String, String) {
    let label = rest.split_once(',').map_or(rest, |(_, label)| label).trim();
    match label.split_once(" - ") {
        Some((artist, title)) => (artist.trim().to_owned(), title.trim().to_owned()),
        None => (String::new(), label.to_owned()),
    }
}

/// CSV with a header row when the first row names a known column, else
/// the export's own column order. Columns are matched case-insensitively
/// and may be reordered; other columns are ignored.
fn parse_csv(content: &str) -> Vec<ParsedEntry> {
    let mut rows = csv_rows(content).into_iter();
    let Some(first) = rows.next() else {
        return Vec::new();
    };
    let header: Vec<String> = first
        .iter()
        .map(|cell| cell.trim().to_lowercase())
        .collect();
    if header
        .iter()
        .any(|cell| KNOWN_COLUMNS.contains(&cell.as_str()))
    {
        let column = |name: &str| header.iter().position(|cell| cell == name);
        let (path, title, artist, album) = (
            column("path"),
            column("title"),
            column("artist"),
            column("album"),
        );
        rows.map(|row| csv_entry(&row, path, title, artist, album))
            .collect()
    } else {
        let (title, artist, album, path) = EXPORT_COLUMNS;
        std::iter::once(first)
            .chain(rows)
            .map(|row| csv_entry(&row, Some(path), Some(title), Some(artist), Some(album)))
            .collect()
    }
}

fn csv_entry(
    row: &[String],
    path: Option<usize>,
    title: Option<usize>,
    artist: Option<usize>,
    album: Option<usize>,
) -> ParsedEntry {
    let cell = |at: Option<usize>| {
        at.and_then(|at| row.get(at))
            .map(|cell| cell.trim().to_owned())
            .unwrap_or_default()
    };
    ParsedEntry {
        title: cell(title),
        artist: cell(artist),
        album: cell(album),
        path: cell(path),
    }
}

/// CSV rows, RFC 4180: quoted fields carry commas and line breaks, and a
/// doubled quote inside one is a quote. Rows of only blank cells are
/// dropped.
fn csv_rows(content: &str) -> Vec<Vec<String>> {
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut chars = content.chars().peekable();
    while let Some(ch) = chars.next() {
        if quoted {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    field.push('"');
                } else {
                    quoted = false;
                }
            } else {
                field.push(ch);
            }
            continue;
        }
        match ch {
            '"' if field.is_empty() => quoted = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\r' | '\n' => {
                if ch == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            _ => field.push(ch),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    rows.retain(|row| row.iter().any(|cell| !cell.trim().is_empty()));
    rows
}

/// A file name read as a title, for an entry that names none.
fn path_title(path: &str) -> String {
    let name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem.to_owned(),
        _ => name.to_owned(),
    }
}

/// The library side of matching, built once per import. Paths win over
/// names; equal matches keep the first track read.
pub(crate) struct ImportIndex {
    by_path: HashMap<String, String>,
    by_names: HashMap<(String, String), String>,
    by_names_album: HashMap<(String, String, String), String>,
}

impl ImportIndex {
    /// An index over the live tracks, in id order so equal matches pick
    /// the same track every time.
    pub(crate) fn new(mut tracks: Vec<ImportTrack>) -> Self {
        tracks.sort_by(|left, right| left.id.cmp(&right.id));
        let mut by_path = HashMap::with_capacity(tracks.len());
        let mut by_names = HashMap::with_capacity(tracks.len());
        let mut by_names_album = HashMap::with_capacity(tracks.len());
        for track in tracks {
            if let Some(artist) = track.artist_name_folded {
                by_names_album
                    .entry((
                        track.title_folded.clone(),
                        artist.clone(),
                        track.album_title_folded,
                    ))
                    .or_insert_with(|| track.id.clone());
                by_names
                    .entry((track.title_folded, artist))
                    .or_insert_with(|| track.id.clone());
            }
            by_path.entry(track.relative_path).or_insert(track.id);
        }
        Self {
            by_path,
            by_names,
            by_names_album,
        }
    }

    /// The id of the track `entry` names: the exact path, then the import
    /// path ending at a folder boundary with the stored one, then the
    /// folded title, artist and album, then the folded title and artist.
    /// Windows separators read as folders too.
    pub(crate) fn find(&self, entry: &ParsedEntry) -> Option<&str> {
        let path = entry.path.trim().replace('\\', "/");
        if let Some(id) = self.by_path.get(&path) {
            return Some(id);
        }
        let mut rest = path.as_str();
        while let Some((_, tail)) = rest.split_once('/') {
            rest = tail;
            if let Some(id) = self.by_path.get(rest) {
                return Some(id);
            }
        }
        let title = fold_text(entry.title.trim());
        let artist = fold_text(entry.artist.trim());
        if title.is_empty() || artist.is_empty() {
            return None;
        }
        let album = fold_text(entry.album.trim());
        if !album.is_empty() {
            let key = (title.clone(), artist.clone(), album);
            if let Some(id) = self.by_names_album.get(&key) {
                return Some(id);
            }
        }
        self.by_names.get(&(title, artist)).map(String::as_str)
    }
}
