//! Playlist files: the extended M3U the Navidrome export job writes, the
//! CSV a playlist download serves, and the portable filename both use.

use unicode_normalization::UnicodeNormalization;

/// File suffix Navidrome imports.
pub const PLAYLIST_SUFFIX: &str = ".m3u8";
/// Ownership marker inside every file the export job writes.
pub const OWNER_MARKER: &str = "#DROPPEDNEEDLE-PLAYLIST-ID:";
/// Longest filename stem.
const MAX_STEM: usize = 120;
/// Names Windows refuses, applied everywhere: the folder is often a share.
const WINDOWS_RESERVED: &[&str] = &[
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];

/// One entry either format writes.
#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    /// Track title.
    pub title: &'a str,
    /// Artist.
    pub artist: &'a str,
    /// Album.
    pub album: &'a str,
    /// Album artist, empty when the file carries none.
    pub album_artist: &'a str,
    /// Whole seconds, truncated; `None` when unknown.
    pub duration_seconds: Option<i64>,
    /// The file relative to the library root, from [`writable_path`].
    pub path: &'a str,
}

/// The path as an M3U line can carry it: non-empty and free of control
/// characters, which a line-oriented format cannot escape.
pub fn writable_path(path: &str) -> Option<&str> {
    (!path.is_empty() && !path.chars().any(char::is_control)).then_some(path)
}

/// Extended M3U: the ownership marker, the playlist name, then one
/// `#EXTINF` line and one path per entry. Callers leave out the entries
/// [`writable_path`] refuses, so every line stays a directive or a path.
pub fn render_m3u(name: &str, playlist_id: &str, entries: &[Entry<'_>]) -> String {
    let mut lines = vec![
        "#EXTM3U".to_owned(),
        format!("{OWNER_MARKER}{playlist_id}"),
        format!("#PLAYLIST:{}", one_line(name)),
    ];
    for entry in entries {
        let seconds = entry.duration_seconds.unwrap_or(-1);
        let artist = one_line(entry.artist);
        let title = one_line(entry.title);
        let label = if artist.is_empty() {
            title
        } else {
            format!("{artist} - {title}")
        };
        lines.push(format!("#EXTINF:{seconds},{label}"));
        lines.push(entry.path.to_owned());
    }
    format!("{}\n", lines.join("\n"))
}

/// CSV with a header row: title, artist, album, album artist, duration in
/// whole seconds, and the file path relative to the library root. Every
/// field is quoted and inner quotes are doubled, so a comma, a quote or a
/// line break in a name never changes a row.
pub fn render_csv(entries: &[Entry<'_>]) -> String {
    let mut lines = vec![csv_row(&[
        "title",
        "artist",
        "album",
        "album artist",
        "duration",
        "path",
    ])];
    for entry in entries {
        let duration = entry
            .duration_seconds
            .map(|seconds| seconds.to_string())
            .unwrap_or_default();
        lines.push(csv_row(&[
            entry.title,
            entry.artist,
            entry.album,
            entry.album_artist,
            &duration,
            entry.path,
        ]));
    }
    format!("{}\n", lines.join("\n"))
}

fn csv_row(fields: &[&str; 6]) -> String {
    fields
        .iter()
        .map(|field| format!("\"{}\"", field.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(",")
}

/// M3U is line-oriented with no escaping: a newline in free text would
/// inject a directive.
pub fn one_line(value: &str) -> String {
    value
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect::<String>()
        .trim()
        .to_owned()
}

/// A portable filename stem: characters Windows refuses become `_`,
/// reserved device names are prefixed, and the result is bounded.
pub fn safe_playlist_stem(name: &str) -> String {
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
    stem
}

/// The export job's filename: a portable stem, the id's first 8 characters
/// since names collide, and the suffix Navidrome imports.
pub fn safe_playlist_filename(name: &str, playlist_id: &str) -> String {
    let short_id: String = playlist_id.chars().take(8).collect();
    format!("{} [{short_id}]{PLAYLIST_SUFFIX}", safe_playlist_stem(name))
}
