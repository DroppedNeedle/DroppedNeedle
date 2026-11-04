//! Path-derived names for the catalog: the album grouper and
//! filename parser: the grouping directory an album keys on (disc folders
//! fold into their parent) and the best-effort artist, album, title, and
//! track number for files whose tags leave them out. Also the keys artist
//! rows are found by, which the v2 export uses to key carried artists.

use std::path::Path;

use sha2::{Digest as _, Sha256};

/// The catalog's "Various Artists" sentinel (seeded by the baseline).
pub const VARIOUS_ARTISTS_ID: &str = "00000000-0000-4000-8000-000000000001";
/// The catalog's "Unknown Artist" sentinel (seeded by the baseline).
pub const UNKNOWN_ARTIST_ID: &str = "00000000-0000-4000-8000-000000000002";
/// The name a credit with no artist gets, keyed to the sentinel.
pub const UNKNOWN_ARTIST: &str = "Unknown Artist";

/// A stable UUID-shaped id from a namespaced key.
pub(crate) fn stable_id(key: &str) -> String {
    let digest = Sha256::digest(key.as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    uuid::Builder::from_custom_bytes(bytes)
        .into_uuid()
        .to_string()
}

/// The artist row a credit's trimmed name and sort name key to. The sort
/// name is the file's only when its side lists a single artist.
#[must_use]
pub fn artist_key(name: &str, sort_name: Option<&str>) -> String {
    if name == UNKNOWN_ARTIST {
        return UNKNOWN_ARTIST_ID.to_owned();
    }
    stable_id(&format!("artist:{name}:{}", sort_name.unwrap_or("")))
}

/// The artist row for a credit with the lowercased MusicBrainz id `mbid`
/// whose name row is proven to be another artist.
#[must_use]
pub fn artist_mbid_key(mbid: &str) -> String {
    stable_id(&format!("artist-mbid:{mbid}"))
}

/// True for a disc folder name like `CD1`, `Disc 02`, `(Vol. 3)`, matched
/// case-insensitively with balanced parentheses, or a medium format and
/// its number like `12" Vinyl 01`, the folders the multi-disc naming preset
/// writes.
pub fn is_disc_directory(segment: &str) -> bool {
    is_numbered_disc(segment) || is_numbered_medium(segment)
}

/// MusicBrainz medium formats, as [`folded_words`] leaves them. The
/// multi-disc naming preset names a disc folder
/// `{medium_format} {medium_number:02d}`, and path cleaning may replace the
/// `"` of `12" Vinyl` or the spaces, so names are compared as words. Checked
/// 2026-11-03 against <https://musicbrainz.org/doc/Release/Format> and the
/// database's own names as Picard generates them
/// (`picard/const/attributes.py`). "Other" is left out: too plain a word to
/// take a folder for a disc.
const MEDIUM_FORMATS: &[&str] = &[
    "cd",
    "dvd",
    "sacd",
    "dualdisc",
    "laserdisc",
    "minidisc",
    "vinyl",
    "cassette",
    "cartridge",
    "reel to reel",
    "dat",
    "digital media",
    "wax cylinder",
    "piano roll",
    "dcc",
    "hd dvd",
    "dvd audio",
    "dvd video",
    "blu ray",
    "vhs",
    "vcd",
    "svcd",
    "betamax",
    "hdcd",
    "usb flash drive",
    "slotmusic",
    "umd",
    "7 vinyl",
    "10 vinyl",
    "12 vinyl",
    "cd r",
    "8cm cd",
    "blu spec cd",
    "shm cd",
    "hqcd",
    "hybrid sacd",
    "cd g",
    "8cm cd g",
    "cdv",
    "enhanced cd",
    "data cd",
    "dts cd",
    "playbutton",
    "download card",
    "dvdplus",
    "vinyldisc",
    "3 5 floppy disk",
    "edison diamond disc",
    "flexi disc",
    "7 flexi disc",
    "shellac",
    "10 shellac",
    "12 shellac",
    "7 shellac",
    "shm sacd",
    "pathé disc",
    "vhd",
    "ced",
    "copy control cd",
    "sd card",
    "hybrid sacd cd layer",
    "hybrid sacd sacd layer",
    "dualdisc dvd audio side",
    "dualdisc dvd video side",
    "dualdisc cd side",
    "dvdplus dvd audio side",
    "dvdplus dvd video side",
    "dvdplus cd side",
    "8 laserdisc",
    "12 laserdisc",
    "phonograph record",
    "playtape",
    "hipac",
    "floppy disk",
    "zip disk",
    "8 track cartridge",
    "blu ray r",
    "vinyldisc dvd side",
    "vinyldisc vinyl side",
    "vinyldisc cd side",
    "microcassette",
    "sacd 2 channels",
    "sacd multichannel",
    "hybrid sacd sacd layer multichannel",
    "hybrid sacd sacd layer 2 channels",
    "shm sacd multichannel",
    "shm sacd 2 channels",
    "tefifon",
    "5 25 floppy disk",
    "dvd r video",
    "data dvd r",
    "data dvd",
    "kit album",
    "dataplay",
    "mixed mode cd",
    "dualdisc dvd side",
    "betacam sp",
    "microsd",
    "minimax cd",
    "minidvd",
    "minimax dvd",
    "minidvd audio",
    "minidvd video",
    "minimax dvd audio",
    "minimax dvd video",
    "acetate",
    "7 acetate",
    "10 acetate",
    "12 acetate",
    "3 vinyl",
    "rom cartridge",
    "cd i",
    "8cm cd r",
    "ultra hd blu ray",
];

/// Lowercase words: every run of characters that are not letters or
/// digits becomes one space.
fn folded_words(text: &str) -> String {
    text.to_lowercase()
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A medium format followed by a disc number, like `Copy Control CD 02`.
fn is_numbered_medium(segment: &str) -> bool {
    let words = folded_words(segment);
    let Some((format, number)) = words.rsplit_once(' ') else {
        return false;
    };
    number.chars().all(|ch| ch.is_ascii_digit()) && MEDIUM_FORMATS.contains(&format)
}

fn is_numbered_disc(segment: &str) -> bool {
    let mut depth = 0i32;
    for ch in segment.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth < 0 {
                    return false;
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        return false;
    }
    let lower = segment.to_lowercase();
    let mut rest = lower.as_str();
    rest = rest.strip_prefix('(').unwrap_or(rest);
    let Some(after_word) = ["volume", "disc", "disk", "vol", "cd"]
        .iter()
        .find_map(|word| rest.strip_prefix(word))
    else {
        return false;
    };
    rest = after_word.strip_prefix(')').unwrap_or(after_word);
    rest = rest.trim_start_matches(|ch: char| ch.is_whitespace() || matches!(ch, '.' | '_' | '-'));
    rest = rest.strip_prefix('(').unwrap_or(rest);
    let digits = rest.len()
        - rest
            .trim_start_matches(|ch: char| ch.is_ascii_digit())
            .len();
    if digits == 0 {
        return false;
    }
    rest = &rest[digits..];
    rest = rest.strip_prefix(')').unwrap_or(rest);
    rest.is_empty()
}

/// Separator inside name-based album keys; never part of a folded name.
pub const KEY_SEPARATOR: char = '\u{1f}';

/// The key an album is found by wherever its files sit: the release MBID
/// when the file carries one (`mbid:<lowercased>`), else the folded album
/// artist and title when both came from tags. Names parsed from the path
/// (or placeholders) only mean something next to that path, so those keys
/// keep the grouping directory. The scan files tracks by this key, and the
/// v2 export writes it for carried albums.
#[must_use]
pub fn album_grouping_key(
    release_mbid: Option<&str>,
    from_tags: bool,
    directory: &str,
    title_folded: &str,
    artist_folded: &str,
) -> String {
    match release_mbid {
        Some(mbid) => format!("mbid:{}", mbid.to_lowercase()),
        None => album_name_key(from_tags, directory, title_folded, artist_folded),
    }
}

/// The name part of [`album_grouping_key`], without the release MBID.
#[must_use]
pub fn album_name_key(
    from_tags: bool,
    directory: &str,
    title_folded: &str,
    artist_folded: &str,
) -> String {
    if from_tags {
        format!("tag:{artist_folded}{KEY_SEPARATOR}{title_folded}")
    } else {
        format!("{directory}\0{title_folded}\0{artist_folded}")
    }
}

/// Directory an album groups under: the file's parent, with a disc folder
/// folded into its own parent (a root-level disc folder folds to `.`).
pub fn grouping_directory(relative_path: &str) -> String {
    let parent = relative_path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("");
    let (grand, name) = match parent.rsplit_once('/') {
        Some((grand, name)) => (grand, name),
        None => ("", parent),
    };
    if !name.is_empty() && is_disc_directory(name) {
        return if grand.is_empty() {
            ".".to_owned()
        } else {
            grand.to_owned()
        };
    }
    if parent.is_empty() {
        ".".to_owned()
    } else {
        parent.to_owned()
    }
}

/// Names parsed from a path when tags are missing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedNames {
    pub artist: Option<String>,
    pub album: Option<String>,
    pub title: Option<String>,
    pub track_number: Option<u32>,
    pub year: Option<i32>,
}

/// Parse a catalog row's path as if its disc folder were transparent, so
/// `Artist/Album/CD1/01 - Title.flac` parses like its `Artist/Album`
/// siblings.
pub fn parse_names_for_row(relative_path: &str) -> ParsedNames {
    let folded = grouping_directory(relative_path);
    let parent = relative_path
        .rsplit_once('/')
        .map(|(parent, _)| parent)
        .unwrap_or("");
    let name = relative_path.rsplit('/').next().unwrap_or(relative_path);
    if folded != "." && folded != parent {
        return parse_names_from_path(&format!("{folded}/{name}"));
    }
    parse_names_from_path(relative_path)
}

const SPLIT: &str = " - ";

fn norm(value: &str) -> String {
    value
        .to_lowercase()
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect()
}

fn same(a: &str, b: &str) -> bool {
    let left = norm(a);
    !left.is_empty() && left == norm(b)
}

/// A `(1999)` or `[1999]` year inside a folder name: the year plus the
/// byte range it spans.
fn year_span(folder: &str) -> Option<(i32, usize, usize)> {
    let bytes = folder.as_bytes();
    for (start, &open) in bytes.iter().enumerate() {
        if open != b'(' && open != b'[' {
            continue;
        }
        let mut index = start + 1;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        let digits_start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if index - digits_start != 4 {
            continue;
        }
        let year = folder[digits_start..index].parse().ok()?;
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        if index < bytes.len() && (bytes[index] == b')' || bytes[index] == b']') {
            return Some((year, start, index + 1));
        }
    }
    None
}

fn clean_album_folder(folder: &str) -> (Option<String>, Option<i32>) {
    let mut album = folder.to_owned();
    let mut year = None;
    while let Some((found, start, end)) = year_span(&album) {
        year.get_or_insert(found);
        album.replace_range(start..end, " ");
    }
    let collapsed = album.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(|ch| matches!(ch, ' ' | '-' | '_'));
    ((!trimmed.is_empty()).then(|| trimmed.to_owned()), year)
}

/// A leading `01 - `, `1.`, `03_` track number and where the title starts.
fn leading_track(title: &str) -> Option<(u32, usize)> {
    let start = title.len() - title.trim_start().len();
    let rest = &title[start..];
    let digits = rest.len()
        - rest
            .trim_start_matches(|ch: char| ch.is_ascii_digit())
            .len();
    if !(1..=3).contains(&digits) {
        return None;
    }
    let number: u32 = rest[..digits].parse().ok()?;
    let after = &rest[digits..];
    let spaced = after.trim_start();
    let separators = spaced.len() - spaced.trim_start_matches(['.', '-', '_', ')']).len();
    if separators == 0 {
        return None;
    }
    let tail = spaced[separators..].trim_start();
    Some((number, title.len() - tail.len()))
}

/// Best-effort artist, album, title, track, and year from a path:
/// `Artist/Album (Year)/NN - Title.ext`.
pub fn parse_names_from_path(relative_path: &str) -> ParsedNames {
    let path = Path::new(relative_path);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("")
        .to_owned();
    let mut parts: Vec<&str> = relative_path.split('/').collect();
    parts.pop();
    let parent = parts.pop().unwrap_or("");
    let grandparent = parts.pop().unwrap_or("");
    let artist = (!grandparent.trim().is_empty()).then(|| grandparent.trim().to_owned());
    let (mut album, year) = clean_album_folder(parent);
    if let (Some(found), Some(artist)) = (album.clone(), artist.as_deref()) {
        let prefix = format!("{}{SPLIT}", artist.to_lowercase());
        if found.to_lowercase().starts_with(&prefix) {
            album = Some(found[prefix.len()..].trim().to_owned());
        } else if let Some((first, rest)) = found.split_once(SPLIT)
            && same(first, artist)
        {
            album = Some(rest.trim().to_owned());
        }
    }
    let mut track = None;
    let mut title = stem.clone();
    if let Some((number, start)) = leading_track(&title) {
        track = Some(number);
        title = title[start..].trim().to_owned();
    }
    if title.contains(SPLIT) {
        let pieces: Vec<String> = title
            .split(SPLIT)
            .map(|piece| piece.trim().to_owned())
            .collect();
        let mut kept = Vec::new();
        for piece in &pieces {
            if !piece.is_empty() && piece.chars().all(|ch| ch.is_ascii_digit()) {
                if track.is_none() {
                    track = piece.parse().ok();
                }
                continue;
            }
            if artist.as_deref().is_some_and(|artist| same(piece, artist))
                || album.as_deref().is_some_and(|album| same(piece, album))
            {
                continue;
            }
            kept.push(piece.clone());
        }
        title = kept.last().or(pieces.last()).cloned().unwrap_or_default();
    }
    let title = title.trim();
    ParsedNames {
        artist,
        album,
        title: (!title.is_empty()).then(|| title.to_owned()),
        track_number: track,
        year,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Disc folders fold into their album's folder, and a file with no
    /// tags falls back to the common Artist/Artist - Album (Year) layout.
    #[test]
    fn folder_and_file_names_parse() {
        assert!(is_disc_directory("CD1"));
        assert!(is_disc_directory("Disc 02"));
        assert!(is_disc_directory("(vol. 3)"));
        assert!(!is_disc_directory("(CD1"));
        assert!(!is_disc_directory("Discography"));
        // The multi-disc naming preset: `{medium_format} {medium_number:02d}`.
        assert!(is_disc_directory("12\" Vinyl 01"));
        assert!(is_disc_directory("12_ Vinyl 02"));
        assert!(is_disc_directory("Copy Control CD 01"));
        assert!(is_disc_directory("Copy_Control_CD_01"));
        assert!(is_disc_directory("Digital Media 03"));
        assert!(is_disc_directory("Hybrid SACD (CD layer) 01"));
        assert!(!is_disc_directory("Vinyl"));
        assert!(!is_disc_directory("Digital Media"));
        assert!(!is_disc_directory("Vinyl Days 2"));
        assert!(!is_disc_directory("Monstercat 001"));
        assert_eq!(grouping_directory("A/Album/12_ Vinyl 02/01.mp3"), "A/Album");
        assert_eq!(grouping_directory("A/Album/CD1/01.flac"), "A/Album");
        assert_eq!(grouping_directory("CD2/01.flac"), ".");
        assert_eq!(grouping_directory("A/Album/01.flac"), "A/Album");
        assert_eq!(grouping_directory("01.flac"), ".");

        let parsed = parse_names_for_row("Artist/Artist - Album (1999)/CD1/03 - Title.flac");
        assert_eq!(parsed.artist.as_deref(), Some("Artist"));
        assert_eq!(parsed.album.as_deref(), Some("Album"));
        assert_eq!(parsed.title.as_deref(), Some("Title"));
        assert_eq!(parsed.track_number, Some(3));
        assert_eq!(parsed.year, Some(1999));
    }
}
