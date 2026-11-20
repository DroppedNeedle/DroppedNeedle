//! Plain lyrics text and LRC.

use super::line::{Budget, Piece, RawLine, RawTrack, build_track, chars_of};
use super::{Lyrics, LyricsKind, LyricsRefused, MAX_LYRICS_BYTES, MAX_LYRICS_CHARACTERS};

/// Shape plain and synced lyrics text into lines. Synced text wins when it
/// has any line. LRC reads as Jellyfin reads it: a line with several stamps
/// (`[00:12][00:45]Chorus`) gives one line per stamp, a stamped line with
/// no text stays as a blank line, and lines come out in time order. Plain
/// empty lines are dropped. `None` when no line has text.
pub fn parse_lyrics(
    plain: Option<&str>,
    synced: Option<&str>,
) -> Result<Option<Lyrics>, LyricsRefused> {
    let bytes = plain
        .map_or(0, str::len)
        .saturating_add(synced.map_or(0, str::len));
    if bytes > MAX_LYRICS_BYTES {
        return Err(LyricsRefused::TooManyBytes);
    }
    let plain = plain.map(str::trim).unwrap_or("");
    let synced = synced.map(str::trim).unwrap_or("");
    let characters = plain.chars().count().saturating_add(synced.chars().count());
    if characters > MAX_LYRICS_CHARACTERS {
        return Err(LyricsRefused::TooManyCharacters);
    }
    if synced.is_empty() && plain.is_empty() {
        return Ok(None);
    }
    if synced.is_empty() {
        let lines = plain
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| RawLine::single(None, None, vec![Piece::plain(line)]))
            .collect();
        return build(lines, false);
    }
    // Each line with the time it sorts by. An unstamped text line sorts
    // with the stamped line above it, so it keeps its place.
    let mut keyed: Vec<(i64, RawLine)> = Vec::new();
    let mut above = i64::MIN;
    // A line stamped many times is copied once per stamp: copies that
    // would pass the character cap are refused before they are made.
    let mut copies = Budget::new();
    for raw in synced.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        match parse_lrc_line(line) {
            Some((text, starts)) => {
                above = starts.first().copied().unwrap_or(above);
                let pieces = vec![Piece::plain(text)];
                copies
                    .spend(chars_of(&pieces).saturating_mul(starts.len()))
                    .map_err(|_| LyricsRefused::TooManyCharacters)?;
                for start in starts {
                    keyed.push((start, RawLine::single(Some(start), None, pieces.clone())));
                }
            }
            None => {
                if !line.starts_with('[') {
                    keyed.push((above, RawLine::single(None, None, vec![Piece::plain(line)])));
                }
            }
        }
    }
    keyed.sort_by_key(|(key, _)| *key);
    build(keyed.into_iter().map(|(_, line)| line).collect(), true)
}

/// The main track of `lines`. `None` when no line has text; refused when
/// the lines' text passes [`MAX_LYRICS_CHARACTERS`].
fn build(lines: Vec<RawLine>, synced: bool) -> Result<Option<Lyrics>, LyricsRefused> {
    let main = build_track(
        RawTrack {
            kind: LyricsKind::Main,
            lang: None,
            synced,
            agents: Vec::new(),
            lines,
        },
        &mut Budget::new(),
    )
    .map_err(|_| LyricsRefused::TooManyCharacters)?;
    Ok(Lyrics::with_text(main, Vec::new()))
}

/// Lyrics text of unknown kind: synced when any line has an LRC timestamp,
/// plain otherwise.
pub fn parse_lyrics_text(text: &str) -> Result<Option<Lyrics>, LyricsRefused> {
    let timed = text
        .lines()
        .any(|line| parse_lrc_line(line.trim()).is_some());
    if timed {
        parse_lyrics(None, Some(text))
    } else {
        parse_lyrics(Some(text), None)
    }
}

/// Parse one LRC line, `[mm:ss.xx]` stamps then text, into the text and
/// each stamp's start in milliseconds (at least one). `None` for tag lines
/// (`[ar:...]`) and a malformed first stamp.
fn parse_lrc_line(line: &str) -> Option<(&str, Vec<i64>)> {
    let mut starts = Vec::new();
    let mut rest = line;
    while let Some((start, after)) = parse_lrc_stamp(rest) {
        starts.push(start);
        rest = after.trim_start();
    }
    (!starts.is_empty()).then(|| (rest.trim_end(), starts))
}

/// One leading `[mm:ss.xx]` stamp: its start in milliseconds and the text
/// after it.
fn parse_lrc_stamp(line: &str) -> Option<(i64, &str)> {
    let inner = line.strip_prefix('[')?;
    let end = inner.find(']')?;
    let stamp = &inner[..end];
    let (minutes_raw, rest) = stamp.split_once(':')?;
    let minutes: i64 = minutes_raw.parse().ok()?;
    let (seconds_raw, fraction_raw) = match rest.split_once('.') {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => match rest.split_once(':') {
            Some(_) => return None,
            None => (rest, None),
        },
    };
    let seconds: i64 = seconds_raw.parse().ok()?;
    if minutes < 0 || !(0..60).contains(&seconds) {
        return None;
    }
    let fraction_ms: i64 = match fraction_raw {
        None => 0,
        Some(digits) => {
            if digits.is_empty() || digits.len() > 3 || !digits.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            let value: i64 = digits.parse().ok()?;
            match digits.len() {
                1 => value * 100,
                2 => value * 10,
                _ => value,
            }
        }
    };
    Some((
        minutes * 60_000 + seconds * 1_000 + fraction_ms,
        &inner[end + 1..],
    ))
}
