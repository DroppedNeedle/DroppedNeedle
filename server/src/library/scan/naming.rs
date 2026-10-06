//! Path-derived names for the catalog, ported from v2's album grouper and
//! filename parser: the grouping directory an album keys on (disc folders
//! fold into their parent) and the best-effort artist, album, title, and
//! track number for files whose tags leave them out.

use std::path::Path;

/// True for a disc folder name like `CD1`, `Disc 02`, `(Vol. 3)`, matched
/// case-insensitively with balanced parentheses.
pub fn is_disc_directory(segment: &str) -> bool {
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

    #[test]
    fn disc_folders_fold_into_their_album() {
        assert!(is_disc_directory("CD1"));
        assert!(is_disc_directory("Disc 02"));
        assert!(is_disc_directory("(vol. 3)"));
        assert!(!is_disc_directory("(CD1"));
        assert!(!is_disc_directory("Discography"));
        assert_eq!(grouping_directory("A/Album/CD1/01.flac"), "A/Album");
        assert_eq!(grouping_directory("CD2/01.flac"), ".");
        assert_eq!(grouping_directory("A/Album/01.flac"), "A/Album");
        assert_eq!(grouping_directory("01.flac"), ".");
    }

    #[test]
    fn filename_fallback_parses_the_common_layout() {
        let parsed = parse_names_for_row("Artist/Artist - Album (1999)/CD1/03 - Title.flac");
        assert_eq!(parsed.artist.as_deref(), Some("Artist"));
        assert_eq!(parsed.album.as_deref(), Some("Album"));
        assert_eq!(parsed.title.as_deref(), Some("Title"));
        assert_eq!(parsed.track_number, Some(3));
        assert_eq!(parsed.year, Some(1999));
    }
}
