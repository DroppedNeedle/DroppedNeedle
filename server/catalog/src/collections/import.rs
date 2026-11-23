//! Playlist import: extended M3U and CSV into entries, and the match
//! against the library's live tracks. No fuzzy matching: a path, or the
//! folded title and artist, or nothing.

use std::collections::HashMap;

use droppedneedle_kernel::db::fold_text;
use droppedneedle_kernel::playlist_files::{FORMULA_GUARD, starts_formula};

use super::models::{ImportFormat, ImportUnmatched, ImportUnmatchedReason};
use crate::local::sql::ImportTrack;

/// Largest playlist file one import accepts, bytes.
pub(crate) const MAX_IMPORT_BYTES: usize = 2 * 1024 * 1024;
/// Most entries one import reads; the rest are only counted.
pub(crate) const MAX_IMPORT_ENTRIES: usize = 10_000;
/// The export's own column order, read when a CSV names no known column:
/// title, artist, album, album artist, duration, path.
const EXPORT_COLUMNS: Columns = Columns {
    title: Some(0),
    artist: Some(1),
    album: Some(2),
    path: Some(5),
    track_number: None,
    disc_number: None,
};
/// Header names each column is read from, lowercased. The header test and
/// the column lookup both read these, so they cannot drift apart.
const TITLE_NAMES: &[&str] = &["title", "track name", "track", "name", "song"];
const ARTIST_NAMES: &[&str] = &["artist", "artist name", "artist name(s)", "artists"];
const ALBUM_NAMES: &[&str] = &["album", "album name"];
const PATH_NAMES: &[&str] = &["path"];
/// Track and disc numbers, read when the export writes them.
const TRACK_NUMBER_NAMES: &[&str] = &["track number", "track no"];
const DISC_NUMBER_NAMES: &[&str] = &["disc number", "disc no", "disc", "disk number"];
/// Other columns the common exports write. They mark a header row but are
/// not read.
const OTHER_NAMES: &[&str] = &["album artist", "duration"];

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
    /// Track number the file gives, when it gives one.
    pub track_number: Option<i32>,
    /// Disc number the file gives, when it gives one.
    pub disc_number: Option<i32>,
}

/// What a file holds: its first [`MAX_IMPORT_ENTRIES`] entries in file
/// order, and how many more it has.
#[derive(Debug, Default)]
pub(crate) struct Parsed {
    /// The entries read.
    pub entries: Vec<ParsedEntry>,
    /// Entries past the limit, counted but not read.
    pub over_limit: usize,
}

impl Parsed {
    /// Keep the entry `read` makes while under the limit, else count it.
    fn take(&mut self, read: impl FnOnce() -> ParsedEntry) {
        if self.entries.len() < MAX_IMPORT_ENTRIES {
            self.entries.push(read());
        } else {
            self.over_limit += 1;
        }
    }
}

/// The entries a file holds, up to the limit. A byte-order mark that starts
/// the file is not content.
pub(crate) fn parse(format: ImportFormat, content: &str) -> Parsed {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    match format {
        ImportFormat::M3u8 => parse_m3u(content),
        ImportFormat::Csv => parse_csv(content),
    }
}

/// The report row for one entry that did not become a track.
pub(crate) fn unmatched(entry: &ParsedEntry, reason: ImportUnmatchedReason) -> ImportUnmatched {
    let title = entry.title.trim();
    ImportUnmatched {
        title: if title.is_empty() {
            path_title(&entry.path)
        } else {
            title.to_owned()
        },
        artist: entry.artist.trim().to_owned(),
        reason,
    }
}

/// Extended M3U: an `#EXTINF` line names the next path's artist and title;
/// a path-only file names neither. Other directives are skipped.
fn parse_m3u(content: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let mut pending: Option<&str> = None;
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix("#EXTINF:") {
            pending = Some(rest);
            continue;
        }
        if line.starts_with('#') {
            continue;
        }
        let extinf = pending.take();
        parsed.take(|| {
            let (artist, title) = match extinf {
                Some(rest) => extinf_names(rest),
                None => (String::new(), path_title(line)),
            };
            ParsedEntry {
                title,
                artist,
                album: String::new(),
                path: line.to_owned(),
                track_number: None,
                disc_number: None,
            }
        });
    }
    parsed
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

/// Where a CSV keeps each name, by column.
#[derive(Debug, Clone, Copy)]
struct Columns {
    title: Option<usize>,
    artist: Option<usize>,
    album: Option<usize>,
    path: Option<usize>,
    track_number: Option<usize>,
    disc_number: Option<usize>,
}

impl Columns {
    /// The columns a header row names, or `None` when the row names no
    /// known column and so is not a header.
    fn of_header(row: &[String]) -> Option<Self> {
        let header: Vec<String> = row.iter().map(|cell| cell.trim().to_lowercase()).collect();
        // The first listed name that some cell holds wins, so a track-number
        // column ahead of the title is not taken for the title.
        let column = |aliases: &[&str]| {
            aliases
                .iter()
                .find_map(|alias| header.iter().position(|cell| cell == alias))
        };
        let names = [
            TITLE_NAMES,
            ARTIST_NAMES,
            ALBUM_NAMES,
            PATH_NAMES,
            TRACK_NUMBER_NAMES,
            DISC_NUMBER_NAMES,
            OTHER_NAMES,
        ];
        let is_header = header
            .iter()
            .any(|cell| names.iter().any(|aliases| aliases.contains(&cell.as_str())));
        if !is_header {
            return None;
        }
        Some(Self {
            title: column(TITLE_NAMES),
            artist: column(ARTIST_NAMES),
            album: column(ALBUM_NAMES),
            path: column(PATH_NAMES),
            track_number: column(TRACK_NUMBER_NAMES),
            disc_number: column(DISC_NUMBER_NAMES),
        })
    }
}

/// CSV with a header row when the first row names a known column, else
/// the export's own column order. Columns are matched case-insensitively
/// and may be reordered; other columns are ignored.
fn parse_csv(content: &str) -> Parsed {
    let mut parsed = Parsed::default();
    let mut columns: Option<Columns> = None;
    for_each_csv_row(content, |row| {
        let columns = match columns {
            Some(columns) => columns,
            None => match Columns::of_header(&row) {
                Some(header) => {
                    columns = Some(header);
                    return;
                }
                None => *columns.insert(EXPORT_COLUMNS),
            },
        };
        parsed.take(|| csv_entry(&row, columns));
    });
    parsed
}

fn csv_entry(row: &[String], columns: Columns) -> ParsedEntry {
    let cell = |at: Option<usize>| {
        at.and_then(|at| row.get(at))
            .map(|cell| unguarded(cell).trim().to_owned())
            .unwrap_or_default()
    };
    ParsedEntry {
        title: cell(columns.title),
        artist: cell(columns.artist),
        album: cell(columns.album),
        path: cell(columns.path),
        track_number: number(row, columns.track_number),
        disc_number: number(row, columns.disc_number),
    }
}

/// A number cell, also read as `1/12` when an export writes the pair.
fn number(row: &[String], at: Option<usize>) -> Option<i32> {
    let cell = row.get(at?)?.trim();
    let first = cell.split('/').next()?.trim();
    first.parse().ok()
}

/// A CSV field without the quote an export puts before a field a
/// spreadsheet would run as a formula.
fn unguarded(cell: &str) -> &str {
    match cell.strip_prefix(FORMULA_GUARD) {
        Some(rest) if starts_formula(rest) => rest,
        _ => cell,
    }
}

/// Each CSV row in order, RFC 4180: quoted fields carry commas and line
/// breaks, and a doubled quote inside one is a quote. Rows of only blank
/// cells are skipped.
fn for_each_csv_row(content: &str, mut each: impl FnMut(Vec<String>)) {
    let mut emit = |row: Vec<String>| {
        if row.iter().any(|cell| !cell.trim().is_empty()) {
            each(row);
        }
    };
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
                emit(std::mem::take(&mut row));
            }
            _ => field.push(ch),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        emit(row);
    }
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
    /// The longest stored path, bytes: no longer path can match one.
    max_path_len: usize,
}

impl ImportIndex {
    /// An index over the live tracks, in id order so equal matches pick
    /// the same track every time.
    pub(crate) fn new(mut tracks: Vec<ImportTrack>) -> Self {
        tracks.sort_by(|left, right| left.id.cmp(&right.id));
        let mut by_path = HashMap::with_capacity(tracks.len());
        let mut by_names = HashMap::with_capacity(tracks.len());
        let mut by_names_album = HashMap::with_capacity(tracks.len());
        let mut max_path_len = 0;
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
            max_path_len = max_path_len.max(track.relative_path.len());
            by_path.entry(track.relative_path).or_insert(track.id);
        }
        Self {
            by_path,
            by_names,
            by_names_album,
            max_path_len,
        }
    }

    /// The id of the track `entry` names: the exact path, then the import
    /// path ending at a folder boundary with the stored one, then the
    /// folded title, artist and album, then the folded title and artist.
    /// A credit that names several artists is tried once more under its
    /// first one, with its album. Windows separators read as folders too.
    /// Only a path or tail no longer than the longest stored path is
    /// looked up, so a long path costs one pass over it rather than a hash
    /// of every tail.
    pub(crate) fn find(&self, entry: &ParsedEntry) -> Option<&str> {
        let path = entry.path.trim().replace('\\', "/");
        let mut rest = path.as_str();
        loop {
            if rest.len() <= self.max_path_len
                && let Some(id) = self.by_path.get(rest)
            {
                return Some(id);
            }
            match rest.split_once('/') {
                Some((_, tail)) => rest = tail,
                None => break,
            }
        }
        let title = fold_text(entry.title.trim());
        let artist = fold_text(entry.artist.trim());
        if title.is_empty() || artist.is_empty() {
            return None;
        }
        let album = fold_text(entry.album.trim());
        if let Some(id) = self.by_names(&title, &artist, &album) {
            return Some(id);
        }
        // The album must match too, so a same-title song by another artist
        // is not taken for this one.
        let first_credit = entry.artist.split([',', ';']).next().unwrap_or_default();
        let first = fold_text(first_credit.trim());
        if album.is_empty() || first.is_empty() || first == artist {
            return None;
        }
        self.by_names_album
            .get(&(title, first, album))
            .map(String::as_str)
    }

    /// The track with this folded title and artist, preferring one on the
    /// folded album when an album is given.
    fn by_names(&self, title: &str, artist: &str, album: &str) -> Option<&str> {
        if !album.is_empty() {
            let key = (title.to_owned(), artist.to_owned(), album.to_owned());
            if let Some(id) = self.by_names_album.get(&key) {
                return Some(id.as_str());
            }
        }
        let key = (title.to_owned(), artist.to_owned());
        self.by_names.get(&key).map(String::as_str)
    }
}
