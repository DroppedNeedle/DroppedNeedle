//! The library naming template: where an imported file goes.
//!
//! Ports v2's `NamingTemplateEngine.format_path`. A template such as
//! `{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}`
//! names a path relative to the library root:
//!
//! - `{name}` is replaced by the value; `{track:02d}` and `{disc:02d}`
//!   pad numbers. Unknown names render empty.
//! - A value never adds structure: `/` and other characters a filesystem
//!   refuses become `_`; only the template's own `/` separate folders.
//! - A bracket pair around an empty value disappears with it, so a
//!   release without a year gives `Album`, not `Album ()`.
//! - Each folder and file name is NFC-normalized, loses leading and
//!   trailing dots and spaces, steps around Windows device names, and is
//!   cut to 252 bytes without splitting a character.

use unicode_normalization::UnicodeNormalization;

/// Longest name kept, in bytes: a margin under the common 255 limit.
const MAX_COMPONENT_BYTES: usize = 252;

/// What the template's names resolve to.
#[derive(Debug, Clone, Default)]
pub struct NamingFields {
    pub artist: String,
    pub album: String,
    pub album_artist: String,
    pub title: String,
    pub year: Option<i32>,
    pub track: u32,
    pub disc: u32,
    pub genre: String,
    pub release_group_mbid: String,
    pub artist_mbid: String,
    /// Lowercase extension without the dot.
    pub ext: String,
}

/// Render `template` into a relative path (with `/` separators). The file
/// always ends in `.{ext}`, even when the template forgets it.
pub fn render(template: &str, fields: &NamingFields) -> String {
    let segments = segments(template);
    let mut values: Vec<Option<String>> = vec![None; segments.len()];
    let mut dropped = vec![false; segments.len()];
    let mut prefix_end: Vec<usize> = vec![0; segments.len()];
    let mut suffix_start: Vec<Option<usize>> = vec![None; segments.len()];
    for (index, (part, is_token)) in segments.iter().enumerate() {
        if !is_token {
            continue;
        }
        let inner = &part[1..part.len() - 1];
        let (name, format) = inner.split_once(':').unwrap_or((inner, ""));
        let mut value = lookup(name, fields);
        if !format.is_empty() && matches!(name, "track" | "disc") {
            value = pad(&value, format);
        }
        if value.is_empty()
            && let Some((before_start, after_end)) = empty_group(&segments, index)
        {
            dropped[index] = true;
            let before_len = segments[index - 1].0.chars().count();
            let current = suffix_start[index - 1].unwrap_or(before_len);
            suffix_start[index - 1] = Some(current.min(before_start));
            prefix_end[index + 1] = prefix_end[index + 1].max(after_end);
        }
        values[index] = Some(value);
    }
    let mut out = String::new();
    for (index, (part, is_token)) in segments.iter().enumerate() {
        if *is_token {
            if !dropped[index] {
                out.push_str(&replace_invalid(values[index].as_deref().unwrap_or("")));
            }
            continue;
        }
        let chars: Vec<char> = part.chars().collect();
        let end = suffix_start[index].unwrap_or(chars.len());
        let start = prefix_end[index];
        if start < end {
            out.extend(&chars[start..end]);
        }
    }
    let mut components: Vec<String> = out
        .split('/')
        .filter(|part| !part.is_empty())
        .map(clean_component)
        .collect();
    if components.is_empty() {
        components.push("_".to_owned());
    }
    let suffix = format!(".{}", fields.ext);
    if let Some(last) = components.last_mut()
        && !fields.ext.is_empty()
        && !last.to_lowercase().ends_with(&suffix)
    {
        *last = clean_component(&format!("{last}{suffix}"));
    }
    components.join("/")
}

fn lookup(name: &str, fields: &NamingFields) -> String {
    match name {
        "artist" => fields.artist.clone(),
        "album" => fields.album.clone(),
        "albumartist" => {
            if fields.album_artist.is_empty() {
                fields.artist.clone()
            } else {
                fields.album_artist.clone()
            }
        }
        "initial" => initial(if fields.album_artist.is_empty() {
            &fields.artist
        } else {
            &fields.album_artist
        }),
        "year" => fields.year.map(|year| year.to_string()).unwrap_or_default(),
        "title" => fields.title.clone(),
        "ext" => fields.ext.clone(),
        "track" => fields.track.to_string(),
        "disc" => fields.disc.to_string(),
        "genre" => fields.genre.clone(),
        "musicbrainz_id" => fields.release_group_mbid.clone(),
        "artist_mbid" => fields.artist_mbid.clone(),
        _ => String::new(),
    }
}

/// `02d` style padding; anything unreadable renders `00`, as in v2.
fn pad(value: &str, format: &str) -> String {
    let Ok(number) = value.parse::<u64>() else {
        return "00".to_owned();
    };
    let spec = format.strip_suffix('d').unwrap_or(format);
    let width = spec.trim_start_matches('0').parse::<usize>().unwrap_or(0);
    if spec.starts_with('0') {
        format!("{number:0width$}")
    } else {
        format!("{number:width$}")
    }
}

/// The artist's first letter, past a leading "The"; `#` for anything
/// that is not a letter.
fn initial(artist: &str) -> String {
    let name: String = artist.nfc().collect::<String>().trim().to_owned();
    let lower = name.to_lowercase();
    let rest = if lower.starts_with("the ") {
        name[4..].trim_start()
    } else {
        name.as_str()
    };
    match rest.chars().next() {
        Some(first) if first.is_alphabetic() => first
            .to_uppercase()
            .find(|ch| ch.is_alphabetic())
            .map(String::from)
            .unwrap_or_else(|| "#".to_owned()),
        _ => "#".to_owned(),
    }
}

/// Literal and `{name}` segments; braces holding nothing stay literal.
fn segments(template: &str) -> Vec<(String, bool)> {
    let chars: Vec<char> = template.chars().collect();
    let mut out = Vec::new();
    let mut literal_start = 0;
    let mut index = 0;
    while index < chars.len() {
        if chars[index] != '{' {
            index += 1;
            continue;
        }
        let mut end = index + 1;
        while end < chars.len() && chars[end] != '{' && chars[end] != '}' {
            end += 1;
        }
        if end >= chars.len() {
            break;
        }
        if chars[end] == '{' {
            index = end;
            continue;
        }
        if end == index + 1 {
            index = end + 1;
            continue;
        }
        out.push((chars[literal_start..index].iter().collect(), false));
        out.push((chars[index..=end].iter().collect(), true));
        index = end + 1;
        literal_start = index;
    }
    out.push((chars[literal_start..].iter().collect(), false));
    out
}

/// For an empty value between two literals, the bracket pair around it
/// as (start of the opening bracket and the spaces before it in the
/// literal before, end of the closing bracket in the literal after).
/// Nested brackets keep theirs.
fn empty_group(segments: &[(String, bool)], token: usize) -> Option<(usize, usize)> {
    if token == 0 || token + 1 >= segments.len() {
        return None;
    }
    let (before, before_token) = &segments[token - 1];
    let (after, after_token) = &segments[token + 1];
    if *before_token || *after_token {
        return None;
    }
    let before: Vec<char> = before.chars().collect();
    let after: Vec<char> = after.chars().collect();
    let opening = before.iter().rposition(|ch| !ch.is_whitespace())?;
    if let Some(outer) = before[..opening].iter().rposition(|ch| !ch.is_whitespace())
        && matches!(before[outer], '(' | '[' | '{')
    {
        return None;
    }
    let closing_char = match before[opening] {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        _ => return None,
    };
    let closing = after.iter().position(|ch| !ch.is_whitespace())?;
    if after[closing] != closing_char {
        return None;
    }
    if let Some(outer) = after[closing + 1..]
        .iter()
        .position(|ch| !ch.is_whitespace())
        && matches!(after[closing + 1 + outer], ')' | ']' | '}')
    {
        return None;
    }
    let mut start = opening;
    while start > 0 && before[start - 1].is_whitespace() {
        start -= 1;
    }
    Some((start, closing + 1))
}

fn invalid(ch: char) -> bool {
    matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || ch.is_control()
}

fn replace_invalid(value: &str) -> String {
    value
        .chars()
        .map(|ch| if invalid(ch) { '_' } else { ch })
        .collect()
}

/// One safe, contained file or folder name.
fn clean_component(component: &str) -> String {
    let normalized: String = component.nfc().collect();
    let replaced = replace_invalid(&normalized);
    let mut cleaned = replaced.trim_matches([' ', '.']).to_owned();
    if cleaned.is_empty() {
        cleaned = "_".to_owned();
    }
    let stem = cleaned
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let reserved = matches!(stem.as_str(), "con" | "prn" | "aux" | "nul")
        || ((stem.starts_with("com") || stem.starts_with("lpt"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    if reserved {
        cleaned = format!("_{cleaned}");
    }
    if cleaned.len() > MAX_COMPONENT_BYTES {
        let mut cut = MAX_COMPONENT_BYTES;
        while !cleaned.is_char_boundary(cut) {
            cut -= 1;
        }
        cleaned.truncate(cut);
        cleaned = cleaned.trim_matches([' ', '.']).to_owned();
        if cleaned.is_empty() {
            cleaned = "_".to_owned();
        }
    }
    cleaned
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: &str = "{albumartist}/{album} ({year})/{disc:02d}{track:02d} {title}.{ext}";

    fn fields() -> NamingFields {
        NamingFields {
            artist: "Portishead".into(),
            album: "Dummy".into(),
            album_artist: "Portishead".into(),
            title: "Sour Times".into(),
            year: Some(1994),
            track: 3,
            disc: 1,
            ext: "flac".into(),
            ..NamingFields::default()
        }
    }

    #[test]
    fn renders_the_default_template() {
        assert_eq!(
            render(DEFAULT, &fields()),
            "Portishead/Dummy (1994)/0103 Sour Times.flac"
        );
    }

    #[test]
    fn empty_groups_and_unsafe_values_fold_away() {
        let mut fields = fields();
        fields.year = None;
        fields.title = "AC/DC: Live?".into();
        fields.album = "..".into();
        assert_eq!(
            render(DEFAULT, &fields),
            "Portishead/_/0103 AC_DC_ Live_.flac"
        );
        assert_eq!(render("{title}", &fields), "AC_DC_ Live_.flac");
        fields.album = "CON".into();
        assert_eq!(
            render("{album}/{title}.{ext}", &fields),
            "_CON/AC_DC_ Live_.flac"
        );
    }
}
