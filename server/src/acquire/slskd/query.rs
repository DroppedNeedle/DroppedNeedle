//! Soulseek query construction: sanitizing, artist wildcards, ladders.
//!
//! Ported from v2's slskd repository (`_sanitize_query`, `_primary_artist`, `_stripped_album_title`,
//! `_wildcard_artist`, `_album_query_ladder`, `_track_query_ladder`).
//! Soulseek ANDs every query word, so a specific query sometimes returns
//! nothing when a broader one returns thousands (verified live), so the
//! ladders escalate from most-specific to broadest, and the scorer narrows
//! back down.

/// Strip Soulseek operators (space-surrounded hyphens, parentheses) that
/// confuse the search, while preserving hyphens inside names like `AC-DC` /
/// `Jay-Z` (v2 `_sanitize_query`). Typographic apostrophes are normalised
/// to the straight ASCII form: MusicBrainz metadata uses ' but shared files
/// are almost always named with `'`.
#[must_use]
pub fn sanitize_query(query: &str) -> String {
    let mut out = String::with_capacity(query.len());
    for ch in query.chars() {
        match ch {
            '\u{2018}' | '\u{2019}' | '\u{201B}' | '\u{02BC}' => out.push('\''),
            '(' | ')' => out.push(' '),
            _ => out.push(ch),
        }
    }
    // Collapse space-surrounded hyphens without touching in-word hyphens.
    let mut collapsed = String::with_capacity(out.len());
    let chars: Vec<char> = out.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let is_spaced_hyphen = chars[index] == '-'
            && index > 0
            && chars[index - 1].is_whitespace()
            && index + 1 < chars.len()
            && chars[index + 1].is_whitespace();
        if is_spaced_hyphen {
            collapsed.push(' ');
            index += 1;
        } else {
            collapsed.push(chars[index]);
            index += 1;
        }
    }
    collapsed.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// First credited artist of a joined credit string (v2 `_primary_artist`).
/// MusicBrainz joins multi-artist credits (`"YMO, ..."`) but Soulseek ANDs
/// every query word, so each extra credit is another term no peer path
/// contains (v2 issue #373). Query rungs use only the primary artist.
#[must_use]
pub fn primary_artist(artist: &str) -> &str {
    let first = artist.split(',').next().unwrap_or(artist).trim();
    if first.is_empty() { artist } else { first }
}

/// Album title without edition parentheticals or a subtitle tail (v2
/// `_stripped_album_title`). `Euphoria (International Edition)` -> `Euphoria`;
/// `Devil May Cry: Season 2 (Soundtrack ...)` -> `Devil May Cry`.
/// Last-resort rungs only: the scorer still narrows the broader result set
/// back down.
#[must_use]
pub fn stripped_album_title(album: &str) -> String {
    let no_parens = strip_balanced(album, '(', ')');
    let no_brackets = strip_balanced(&no_parens, '[', ']');
    let no_subtitle = no_brackets.split(':').next().unwrap_or("").to_owned();
    no_subtitle.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn strip_balanced(text: &str, open: char, close: char) -> String {
    let mut out = String::with_capacity(text.len());
    let mut depth = 0;
    for ch in text.chars() {
        if ch == open {
            depth += 1;
            out.push(' ');
        } else if ch == close {
            if depth > 0 {
                depth -= 1;
            }
            out.push(' ');
        } else if depth == 0 {
            out.push(ch);
        } else {
            out.push(' ');
        }
    }
    out
}

/// Blocked-artist workaround (v2 `_wildcard_artist`): Soulseek's server
/// filters searches containing certain artist terms (DMCA), returning 0
/// results no matter what else is in the query ("Enter Shikari" ->
/// nothing). Replacing the first letter of each word with Soulseek's
/// leading wildcard defeats the filter while matching the same files
/// ("*nter *hikari" -> lots). An apostrophe right after the first letter is
/// absorbed into the wildcard (D'Angelo -> *Angelo) so matching no longer
/// depends on the peer's apostrophe form.
#[must_use]
pub fn wildcard_artist(artist: &str) -> String {
    artist
        .split_whitespace()
        .map(wildcard_word)
        .collect::<Vec<_>>()
        .join(" ")
}

fn wildcard_word(word: &str) -> String {
    let mut chars = word.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    if !first.is_alphabetic() {
        return word.to_owned();
    }
    let mut rest: String = chars.collect();
    if rest.starts_with('\'') || rest.starts_with('\u{2019}') {
        rest = rest.chars().skip(1).collect();
    }
    // Keep short words exact: "*" plus a lone char matches far too much (v2).
    if rest.chars().count() >= 2 {
        format!("*{rest}")
    } else {
        word.to_owned()
    }
}

fn dedupe_queries(queries: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for query in queries {
        if !query.is_empty() && seen.insert(query.clone()) {
            out.push(query);
        }
    }
    out
}

fn build_album_query(artist: &str, album: &str, year: Option<i32>) -> String {
    let mut parts = vec![artist.to_owned(), album.to_owned()];
    if let Some(year) = year {
        parts.push(year.to_string());
    }
    sanitize_query(&parts.join(" - "))
}

fn build_track_query(artist: &str, track: &str, album: Option<&str>) -> String {
    let mut parts = vec![artist.to_owned(), track.to_owned()];
    if let Some(album) = album {
        parts.push(album.to_owned());
    }
    sanitize_query(&parts.join(" - "))
}

/// Most-specific-first album queries (v2 `_album_query_ladder`):
/// artist+album+year -> artist+album -> edition-stripped title rungs ->
/// artist. Every rung queries the primary credited artist only. Each rung
/// is followed by a blocked-artist variant with the artist's first letters
/// wildcarded: exact goes first because wildcards degrade matching on some
/// clients; the wildcard sibling comes before broadening so a blocked artist
/// still gets the most specific query that can return anything (v2).
#[must_use]
pub fn album_query_ladder(artist: &str, album: &str, year: Option<i32>) -> Vec<String> {
    let primary = primary_artist(artist);
    let wildcard = wildcard_artist(&sanitize_query(primary));
    let mut queries = vec![
        build_album_query(primary, album, year),
        build_album_query(&wildcard, album, year),
        build_album_query(primary, album, None),
        build_album_query(&wildcard, album, None),
    ];
    let stripped = stripped_album_title(album);
    if !stripped.is_empty() && stripped != album.split_whitespace().collect::<Vec<_>>().join(" ") {
        queries.extend([
            build_album_query(primary, &stripped, year),
            build_album_query(&wildcard, &stripped, year),
            build_album_query(primary, &stripped, None),
            build_album_query(&wildcard, &stripped, None),
        ]);
    }
    queries.extend([sanitize_query(primary), wildcard]);
    dedupe_queries(queries)
}

/// Most-specific-first track queries (v2 `_track_query_ladder`):
/// artist+track+album -> artist+track. Keeps the track title at every rung
/// so the track matcher can match. Wildcard blocked-artist variants
/// interleave as in [`album_query_ladder`].
#[must_use]
pub fn track_query_ladder(artist: &str, track: &str, album: Option<&str>) -> Vec<String> {
    let primary = primary_artist(artist);
    let wildcard = wildcard_artist(&sanitize_query(primary));
    dedupe_queries(vec![
        build_track_query(primary, track, album),
        build_track_query(&wildcard, track, album),
        build_track_query(primary, track, None),
        build_track_query(&wildcard, track, None),
    ])
}
