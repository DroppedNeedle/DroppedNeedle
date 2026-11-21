//! Plain lyrics text and LRC, enhanced LRC's word stamps included.
//!
//! One stamp grammar serves `[..]` and `<..>`: `m:ss` or `m:ss.f`, with
//! one or more minute digits, one or two second digits (0-59) and one to
//! three fraction digits. A `<..>` that does not match is text ("I <3
//! you"), and its inside may not hold another `<`. Valid `<..>` stamps
//! never reach the text, on any path.
//!
//! Word stamps: in a line with one `[t]`, text between `[t]` and the first
//! `<..>` is a piece at `t` when it is not blank, and each `<w>` starts a
//! piece running to the next stamp. A last `<w>` with only whitespace after
//! it marks the line's end. A line with no `[..]` is timed from its first
//! `<..>`, the text before it plain. A line with several `[..]` stamps keeps
//! its text without word timing: word stamps are absolute and fit one copy.
//!
//! `[offset:±n]` (milliseconds, wherever it sits) moves every stamp:
//! `t - n`, never below 0, so a positive offset shows lines sooner.
//! `[lang:xx]` names the language.

use super::line::{Budget, MAX_TIME_MS, Piece, RawLine, RawTrack, build_track, chars_of};
use super::{
    Lyrics, LyricsKind, LyricsRefused, MAX_LYRICS_BYTES, MAX_LYRICS_CHARACTERS, valid_lang,
};

/// Most minute digits a stamp may have: 100 hours is 6000 minutes.
const MAX_MINUTE_DIGITS: usize = 4;
/// Most digits an `[offset:]` may have.
const MAX_OFFSET_DIGITS: usize = 9;

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
        return parse_plain(plain);
    }
    parse_synced(synced)
}

/// Lyrics text of unknown kind: synced when any line has an LRC timestamp
/// or a word stamp, plain otherwise.
pub fn parse_lyrics_text(text: &str) -> Result<Option<Lyrics>, LyricsRefused> {
    let timed = text.lines().any(|line| {
        let line = line.trim();
        !leading_stamps(line).0.is_empty()
            || (!line.starts_with('[') && segments(line).iter().any(Segment::is_stamp))
    });
    if timed {
        parse_lyrics(None, Some(text))
    } else {
        parse_lyrics(Some(text), None)
    }
}

/// Plain text: each non-empty line, word stamps taken out.
fn parse_plain(text: &str) -> Result<Option<Lyrics>, LyricsRefused> {
    let lines = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| RawLine::single(None, None, plain_pieces(&segments(line))))
        .collect();
    let Some(mut lyrics) = build(lines, None, false)? else {
        return Ok(None);
    };
    lyrics.main.lines.retain(|line| !line.text.is_empty());
    Ok(Some(lyrics))
}

/// LRC text. A line stamped many times is copied once per stamp, so the
/// copies are spent from a budget as they are made: text that grows past
/// [`MAX_LYRICS_CHARACTERS`] is refused.
fn parse_synced(text: &str) -> Result<Option<Lyrics>, LyricsRefused> {
    let mut offset = None;
    let mut lang = None;
    for line in text.lines() {
        let line = line.trim();
        if offset.is_none() {
            offset = tag_value(line, "offset").and_then(parse_offset);
        }
        if lang.is_none() {
            lang = tag_value(line, "lang").and_then(valid_lang);
        }
    }
    let shift = |ms: i64| (ms - offset.unwrap_or(0)).clamp(0, MAX_TIME_MS);
    // Each line with the time it sorts by. An unstamped text line sorts
    // with the timed line above it, so it keeps its place.
    let mut keyed: Vec<(i64, RawLine)> = Vec::new();
    let mut above = i64::MIN;
    let mut copies = Budget::new();
    let mut spend = |line: &RawLine, times: usize| {
        let chars = line
            .parts
            .iter()
            .map(|part| chars_of(&part.pieces))
            .sum::<usize>();
        copies
            .spend(chars.saturating_mul(times))
            .map_err(|_| LyricsRefused::TooManyCharacters)
    };
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let (starts, rest) = leading_stamps(line);
        let starts: Vec<i64> = starts.into_iter().map(shift).collect();
        if starts.is_empty() && line.starts_with('[') {
            continue;
        }
        let segments = segments(rest);
        let has_words = segments.iter().any(Segment::is_stamp);
        match starts.as_slice() {
            [] if has_words => {
                let line = word_line(None, &segments, &shift);
                spend(&line, 1)?;
                let start = line.start_ms.unwrap_or(above);
                above = start;
                keyed.push((start, line));
            }
            [] => {
                let line = RawLine::single(None, None, plain_pieces(&segments));
                spend(&line, 1)?;
                keyed.push((above, line));
            }
            [start] if has_words => {
                let line = word_line(Some(*start), &segments, &shift);
                spend(&line, 1)?;
                above = *start;
                keyed.push((*start, line));
            }
            _ => {
                above = starts.first().copied().unwrap_or(above);
                let line = RawLine::single(None, None, plain_pieces(&segments));
                spend(&line, starts.len())?;
                for start in starts {
                    keyed.push((
                        start,
                        RawLine {
                            start_ms: Some(start),
                            ..line.clone()
                        },
                    ));
                }
            }
        }
    }
    keyed.sort_by_key(|(key, _)| *key);
    build(
        keyed.into_iter().map(|(_, line)| line).collect(),
        lang,
        true,
    )
}

/// The main track of `lines`. `None` when no line has text; refused when
/// the lines' text passes [`MAX_LYRICS_CHARACTERS`].
fn build(
    lines: Vec<RawLine>,
    lang: Option<String>,
    synced: bool,
) -> Result<Option<Lyrics>, LyricsRefused> {
    let main = build_track(
        RawTrack {
            kind: LyricsKind::Main,
            lang,
            synced,
            agents: Vec::new(),
            lines,
        },
        &mut Budget::new(),
    )
    .map_err(|_| LyricsRefused::TooManyCharacters)?;
    Ok(Lyrics::with_text(main, Vec::new()))
}

/// A run of a line: text, or a word stamp's time (before any offset).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Segment<'a> {
    Text(&'a str),
    Stamp(i64),
}

impl Segment<'_> {
    fn is_stamp(&self) -> bool {
        matches!(self, Segment::Stamp(_))
    }
}

/// A line with word stamps: text before the first stamp is a piece at
/// `start` when not blank (plain without a `start`), each stamp starts a
/// piece, and a last stamp with only whitespace after it is the line's end.
fn word_line(start: Option<i64>, segments: &[Segment<'_>], shift: &dyn Fn(i64) -> i64) -> RawLine {
    let mut pieces: Vec<Piece> = Vec::new();
    let mut current: Option<Piece> = None;
    let mut first_stamp = None;
    for segment in segments {
        match *segment {
            Segment::Text(text) => match current.as_mut() {
                Some(piece) => piece.text.push_str(text),
                None => current = Some(Piece::plain(text)),
            },
            Segment::Stamp(ms) => {
                let ms = shift(ms);
                first_stamp.get_or_insert(ms);
                if let Some(mut piece) = current.take() {
                    if piece.start_ms.is_none() && !piece.text.trim().is_empty() {
                        piece.start_ms = start;
                    }
                    pieces.push(piece);
                }
                current = Some(Piece::timed(String::new(), ms, None));
            }
        }
    }
    let mut end = None;
    if let Some(piece) = current {
        if piece.start_ms.is_some() && piece.text.trim().is_empty() {
            end = piece.start_ms;
        }
        pieces.push(piece);
    }
    RawLine::single(start.or(first_stamp), end, pieces)
}

/// The text of `segments` with the word stamps taken out.
fn plain_pieces(segments: &[Segment<'_>]) -> Vec<Piece> {
    segments
        .iter()
        .filter_map(|segment| match segment {
            Segment::Text(text) => Some(Piece::plain(*text)),
            Segment::Stamp(_) => None,
        })
        .collect()
}

/// Split a line's text at its valid `<..>` stamps.
fn segments(text: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut from = 0;
    let mut at = 0;
    while let Some(found) = text[at..].find('<') {
        let open = at + found;
        let inner_from = open + 1;
        let close = text[inner_from..]
            .find(['<', '>'])
            .map(|index| inner_from + index)
            .filter(|index| text.as_bytes()[*index] == b'>');
        match close.and_then(|close| Some((close, parse_stamp(&text[inner_from..close])?))) {
            Some((close, ms)) => {
                if open > from {
                    out.push(Segment::Text(&text[from..open]));
                }
                out.push(Segment::Stamp(ms));
                from = close + 1;
                at = from;
            }
            None => at = inner_from,
        }
    }
    if from < text.len() {
        out.push(Segment::Text(&text[from..]));
    }
    out
}

/// The leading `[..]` stamps of a line (before any offset) and the text
/// after them. No stamps when the line does not start with one.
fn leading_stamps(line: &str) -> (Vec<i64>, &str) {
    let mut starts = Vec::new();
    let mut rest = line;
    while let Some(inner) = rest.strip_prefix('[') {
        let Some(close) = inner.find(']') else {
            break;
        };
        let Some(ms) = parse_stamp(&inner[..close]) else {
            break;
        };
        starts.push(ms);
        rest = inner[close + 1..].trim_start();
    }
    (starts, rest)
}

/// One stamp's inside, `m:ss` or `m:ss.f`, in milliseconds.
fn parse_stamp(stamp: &str) -> Option<i64> {
    let (minutes, rest) = stamp.split_once(':')?;
    let (seconds, fraction) = match rest.split_once('.') {
        Some((seconds, fraction)) => (seconds, Some(fraction)),
        None => (rest, None),
    };
    let minutes = digits(minutes, 1, MAX_MINUTE_DIGITS)?;
    let seconds = digits(seconds, 1, 2).filter(|seconds| *seconds < 60)?;
    let fraction_ms = match fraction {
        None => 0,
        Some(fraction) => {
            let value = digits(fraction, 1, 3)?;
            match fraction.len() {
                1 => value * 100,
                2 => value * 10,
                _ => value,
            }
        }
    };
    let ms = minutes
        .checked_mul(60_000)?
        .checked_add(seconds * 1_000)?
        .checked_add(fraction_ms)?;
    (ms <= MAX_TIME_MS).then_some(ms)
}

/// `text` as a number when it is `min..=max` ASCII digits.
fn digits(text: &str, min: usize, max: usize) -> Option<i64> {
    if !(min..=max).contains(&text.len()) || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    text.parse().ok()
}

/// The value of an LRC tag line `[name:value]`, name in any case.
fn tag_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let inner = line.strip_prefix('[')?.strip_suffix(']')?;
    let (key, value) = inner.split_once(':')?;
    key.trim()
        .eq_ignore_ascii_case(name)
        .then_some(value.trim())
}

/// An `[offset:]` value in milliseconds: a sign, then digits. One past 100
/// hours is not read.
fn parse_offset(value: &str) -> Option<i64> {
    let (negative, number) = match value.strip_prefix('-') {
        Some(number) => (true, number),
        None => (false, value.strip_prefix('+').unwrap_or(value)),
    };
    let magnitude = digits(number, 1, MAX_OFFSET_DIGITS).filter(|ms| *ms <= MAX_TIME_MS)?;
    Some(if negative { -magnitude } else { magnitude })
}
