//! String distance for titles and names, after beets' `string_dist`.
//!
//! Both sides are folded (compatibility decomposition, accents dropped,
//! a few Latin letters without a decomposition spelled out, lowercase,
//! letters and digits only) and compared by normalized Levenshtein
//! distance. CJK and kana stay as written: transliterating them loses
//! more than it gains. Parts that commonly differ between a file's tags
//! and MusicBrainz (a leading "The", "feat." tails, parenthesized and
//! bracketed suffixes, "Part 2" tails, "EP"/"single" markers) cost less
//! than the rest of the string, with beets' weights.

use unicode_normalization::UnicodeNormalization as _;
use unicode_normalization::char::is_combining_mark;

/// Comparable form: no accents, no punctuation, no spaces, lowercase.
pub fn fold(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.nfkd() {
        if is_combining_mark(character) {
            continue;
        }
        match character {
            'æ' | 'Æ' => out.push_str("ae"),
            'œ' | 'Œ' => out.push_str("oe"),
            'ø' | 'Ø' => out.push('o'),
            'ß' => out.push_str("ss"),
            'đ' | 'Đ' | 'ð' | 'Ð' => out.push('d'),
            'ł' | 'Ł' => out.push('l'),
            'þ' | 'Þ' => out.push_str("th"),
            'ı' => out.push('i'),
            other if other.is_alphanumeric() => out.extend(other.to_lowercase()),
            _ => {}
        }
    }
    out
}

/// Distance between two names in `[0, 1]`, plus small extra penalties
/// for the down-weighted parts (so the result can slightly exceed 1).
/// Two blanks are equal; one blank is as far as it gets.
pub fn string_dist(left: &str, right: &str) -> f64 {
    let mut left = prepare(left);
    let mut right = prepare(right);
    let mut base = basic_dist(&left, &right);
    let mut penalty = 0.0;
    for (strip, weight) in PATTERNS {
        let left_case = strip(&left);
        let right_case = strip(&right);
        if left_case == left && right_case == right {
            continue;
        }
        let case_dist = basic_dist(&left_case, &right_case);
        let delta = (base - case_dist).max(0.0);
        if delta == 0.0 {
            continue;
        }
        left = left_case;
        right = right_case;
        base = case_dist;
        penalty += weight * delta;
    }
    base + penalty
}

/// Lowercase, move a trailing ", The" (or "A", "An") to the front, and
/// spell `&` as "and".
fn prepare(value: &str) -> String {
    let mut lowered = value.trim().to_lowercase();
    for article in ["the", "a", "an"] {
        let suffix = format!(", {article}");
        if let Some(head) = lowered.strip_suffix(&suffix) {
            lowered = format!("{article} {head}");
            break;
        }
    }
    lowered.replace('&', "and")
}

fn basic_dist(left: &str, right: &str) -> f64 {
    let left: Vec<char> = fold(left).chars().collect();
    let right: Vec<char> = fold(right).chars().collect();
    if left.is_empty() && right.is_empty() {
        return 0.0;
    }
    levenshtein(&left, &right) as f64 / left.len().max(right.len()) as f64
}

fn levenshtein(left: &[char], right: &[char]) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (row, left_char) in left.iter().enumerate() {
        current[0] = row + 1;
        for (column, right_char) in right.iter().enumerate() {
            let substitution = previous[column] + usize::from(left_char != right_char);
            current[column + 1] = substitution
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

type Strip = fn(&str) -> String;

/// beets' `SD_PATTERNS`, in its order, with its weights.
const PATTERNS: [(Strip, f64); 6] = [
    (strip_leading_the, 0.1),
    (strip_release_kind, 0.0),
    (strip_featuring, 0.1),
    (strip_parenthesized, 0.3),
    (strip_bracketed, 0.3),
    (strip_part, 0.2),
];

fn strip_leading_the(value: &str) -> String {
    value.strip_prefix("the ").unwrap_or(value).to_owned()
}

/// Drop "EP" and "single" words, bare or in brackets.
fn strip_release_kind(value: &str) -> String {
    value
        .split(' ')
        .filter(|word| {
            let bare = word.trim_matches(|c| matches!(c, '(' | ')' | '[' | ']'));
            bare != "ep" && bare != "single"
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Drop everything from a "feat." / "ft." / "featuring" word on.
fn strip_featuring(value: &str) -> String {
    let mut offset = 0;
    for word in value.split(' ') {
        let bare = word
            .trim_start_matches(['(', '['])
            .trim_end_matches([':', '.']);
        if matches!(bare, "feat" | "ft" | "featuring") {
            return value[..offset].trim_end().to_owned();
        }
        offset += word.len() + 1;
    }
    value.to_owned()
}

fn strip_enclosed(value: &str, open: char, close: char) -> String {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find(open) {
        let Some(length) = rest[start..].find(close) else {
            break;
        };
        out.push_str(&rest[..start]);
        rest = &rest[start + length + close.len_utf8()..];
    }
    out.push_str(rest);
    out
}

fn strip_parenthesized(value: &str) -> String {
    strip_enclosed(value, '(', ')')
}

fn strip_bracketed(value: &str) -> String {
    strip_enclosed(value, '[', ']')
}

/// Drop a trailing "Part ..." or "Pt. ..." (with an optional ", ").
fn strip_part(value: &str) -> String {
    let mut offset = 0;
    for word in value.split(' ') {
        if (word == "part" || word == "pt.") && offset + word.len() < value.len() {
            return value[..offset].trim_end().trim_end_matches(',').to_owned();
        }
        offset += word.len() + 1;
    }
    value.to_owned()
}

/// Words that name an edition rather than an album (v2's
/// `EDITION_SUFFIXES`); album titles drop them before the title gate.
const EDITION_WORDS: [&str; 22] = [
    "deluxe",
    "remastered",
    "remaster",
    "edition",
    "anniversary",
    "special",
    "expanded",
    "complete",
    "bonus",
    "acoustic",
    "live",
    "demo",
    "extended",
    "instrumental",
    "mono",
    "stereo",
    "explicit",
    "clean",
    "version",
    "single",
    "promo",
    "radio edit",
];

/// An album title without its edition suffix: bracketed parts that name
/// an edition ("Night Shift (Deluxe Edition)", "[Remastered]") and
/// edition words at the end ("Night Shift - Deluxe Edition"). Edition
/// words inside the title proper stay ("Live at Leeds", "Single Life").
pub fn strip_edition_words(title: &str) -> String {
    let lowered = title.to_lowercase();
    let mut kept = String::with_capacity(lowered.len());
    let mut rest = lowered.as_str();
    while let Some(start) = rest.find(['(', '[', '{']) {
        let close = match rest.as_bytes()[start] {
            b'(' => ')',
            b'[' => ']',
            _ => '}',
        };
        let Some(length) = rest[start..].find(close) else {
            break;
        };
        let inner = &rest[start + 1..start + length];
        kept.push_str(&rest[..start]);
        if !names_edition(inner) {
            kept.push(' ');
            kept.push_str(inner);
            kept.push(' ');
        }
        rest = &rest[start + length + 1..];
    }
    kept.push_str(rest);
    let mut words: Vec<&str> = kept.split_whitespace().collect();
    while let Some(last) = words.last() {
        let bare = last.trim_matches(|c: char| !c.is_alphanumeric());
        let pair = words.len() >= 2
            && bare == "edit"
            && words[words.len() - 2].trim_matches(|c: char| !c.is_alphanumeric()) == "radio";
        if pair {
            words.truncate(words.len() - 2);
        } else if bare.is_empty() || EDITION_WORDS.contains(&bare) {
            words.pop();
        } else {
            break;
        }
    }
    words.join(" ")
}

/// True when a bracketed part names an edition: it holds an edition word.
fn names_edition(inner: &str) -> bool {
    let padded = format!(" {inner} ");
    padded.contains(" radio edit ")
        || inner
            .split_whitespace()
            .any(|word| EDITION_WORDS.contains(&word.trim_matches(|c: char| !c.is_alphanumeric())))
}

/// The folded artists in a credit string, split on collaboration
/// separators (`;`, `&`, `,`, `/`, `+`, `×`, and the words feat, ft,
/// featuring, with, vs).
fn artist_tokens(credit: &str) -> std::collections::BTreeSet<String> {
    let mut parts = vec![String::new()];
    for word in credit.split_whitespace() {
        let bare = word
            .trim_end_matches('.')
            .trim_matches(|c| c == '(' || c == ')')
            .to_lowercase();
        if matches!(bare.as_str(), "feat" | "ft" | "featuring" | "with" | "vs") {
            parts.push(String::new());
            continue;
        }
        for character in word.chars() {
            if matches!(character, ';' | '×' | '&' | '+' | ',' | '/') {
                parts.push(String::new());
            } else if let Some(last) = parts.last_mut() {
                last.push(character);
            }
        }
        if let Some(last) = parts.last_mut() {
            last.push(' ');
        }
    }
    parts
        .iter()
        .map(|part| fold(part))
        .filter(|part| !part.is_empty())
        .collect()
}

/// True when one credit's artists contain the other's ("Bad Omens;
/// Poppy" and "Bad Omens"), never for disjoint credits (a tribute band
/// and the band it covers). v2's `_artist_subset_match`.
pub fn artist_subset(left: &str, right: &str) -> bool {
    let left = artist_tokens(left);
    let right = artist_tokens(right);
    !left.is_empty() && !right.is_empty() && (left.is_subset(&right) || right.is_subset(&left))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_and_weighs_like_beets() {
        assert_eq!(string_dist("Sigur Rós", "sigur ros"), 0.0);
        assert_eq!(string_dist("Beatles, The", "The Beatles"), 0.0);
        assert_eq!(string_dist("Simon & Garfunkel", "Simon and Garfunkel"), 0.0);
        assert_eq!(string_dist("", ""), 0.0);
        assert_eq!(string_dist("Abbey Road", ""), 1.0);
        // A parenthesized suffix costs 0.3 of what it would otherwise.
        let suffixed = string_dist("Abbey Road", "Abbey Road (Remastered)");
        assert!(suffixed > 0.0 && suffixed < 0.2, "{suffixed}");
        assert!(string_dist("Abbey Road", "Let It Be") > 0.6);
        // CJK stays as written and still compares.
        assert_eq!(string_dist("東京", "東京"), 0.0);
        assert_eq!(string_dist("東京", "大阪"), 1.0);
        assert_eq!(
            strip_edition_words("Night Shift (Deluxe Edition) [Radio Edit]"),
            "night shift"
        );
        assert_eq!(
            strip_edition_words("Night Shift - Remastered"),
            "night shift"
        );
        assert_eq!(strip_edition_words("Live at Leeds"), "live at leeds");
        assert_eq!(
            strip_edition_words("Songs (From the Basement)"),
            "songs from the basement"
        );
        assert!(artist_subset("Bad Omens feat. Poppy", "Bad Omens"));
        assert!(!artist_subset("The Tribute Band", "Michael Jackson"));
    }
}
