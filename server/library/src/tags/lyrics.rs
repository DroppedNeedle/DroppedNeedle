//! Lyrics a music file carries, and the one parser every lyrics source
//! goes through.
//!
//! A file's lyrics come from, in order:
//!
//! 1. a `.lrc` file in the same folder with the same stem (the extension
//!    in any case), where Library Management writes synced lyrics;
//! 2. the file's lyrics tag: lofty's generic lyrics items (Vorbis
//!    `LYRICS` and `UNSYNCEDLYRICS`, ID3 `USLT`, MP4 `©lyr`). AAC's APEv2
//!    tag is read too: the APE reader maps keys through lofty's APE table,
//!    which already has `Lyrics` and `UnsyncedLyrics`.
//!
//! A source counts only when it parses to at least one non-empty line. A
//! `.lrc` must be a regular file (a link is never followed), at most
//! [`MAX_LYRICS_BYTES`], and UTF-8 (a leading byte order mark is dropped).
//! Text with LRC timestamps counts as synced, wherever it came from.
//!
//! Everything here only reads. The lyrics sweep and the serving path both
//! call [`read_file_lyrics`], so a track marked as having lyrics is one
//! that can be served.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{Read as _, Seek as _, SeekFrom};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use lofty::file::TaggedFileExt as _;
use lofty::tag::{ItemKey, TagType};

use super::{AudioFormat, format_for_path};

/// Largest lyrics payload read, in bytes (v2 `_MAX_LYRICS_BYTES`).
pub const MAX_LYRICS_BYTES: usize = 2 * 1024 * 1024;
/// Largest lyrics payload read, in characters (v2 `_MAX_LYRICS_CHARACTERS`).
pub const MAX_LYRICS_CHARACTERS: usize = 1_000_000;

/// Lyrics parsed into lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lyrics {
    /// Lines in order: text plus the start in milliseconds, when timed.
    pub lines: Vec<(String, Option<i64>)>,
    /// True when the lines carry timestamps.
    pub synced: bool,
}

/// Where a file's lyrics were found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricsSource {
    /// A `.lrc` file beside the track.
    Sidecar,
    /// The file's own lyrics tag.
    Tag,
}

impl LyricsSource {
    /// The stored spelling, as the `local_track_lyrics.source` CHECK lists it.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sidecar => "sidecar",
            Self::Tag => "tag",
        }
    }
}

/// Why a payload was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LyricsRefused {
    #[error("lyrics payload exceeds the byte cap")]
    TooManyBytes,
    #[error("lyrics payload exceeds the character cap")]
    TooManyCharacters,
}

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
        let lines: Vec<(String, Option<i64>)> = plain
            .lines()
            .map(|line| (line.trim().to_owned(), None))
            .filter(|(text, _)| !text.is_empty())
            .collect();
        return Ok((!lines.is_empty()).then_some(Lyrics {
            lines,
            synced: false,
        }));
    }
    // Each line with the time it sorts by. An unstamped text line sorts
    // with the stamped line above it, so it keeps its place.
    let mut keyed: Vec<(i64, String, Option<i64>)> = Vec::new();
    let mut above = i64::MIN;
    for raw in synced.lines() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        match parse_lrc_line(line) {
            Some((text, starts)) => {
                above = starts.first().copied().unwrap_or(above);
                for start in starts {
                    keyed.push((start, text.clone(), Some(start)));
                }
            }
            None => {
                if !line.starts_with('[') {
                    keyed.push((above, line.to_owned(), None));
                }
            }
        }
    }
    if keyed.iter().all(|(_, text, _)| text.is_empty()) {
        return Ok(None);
    }
    keyed.sort_by_key(|(key, _, _)| *key);
    Ok(Some(Lyrics {
        lines: keyed
            .into_iter()
            .map(|(_, text, start)| (text, start))
            .collect(),
        synced: true,
    }))
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
fn parse_lrc_line(line: &str) -> Option<(String, Vec<i64>)> {
    let mut starts = Vec::new();
    let mut rest = line;
    while let Some((start, after)) = parse_lrc_stamp(rest) {
        starts.push(start);
        rest = after.trim_start();
    }
    (!starts.is_empty()).then(|| (rest.trim_end().to_owned(), starts))
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

/// The lyrics `audio` carries: its `.lrc` file first, then its tag. `None`
/// when neither gives a line. A parser panic on a malformed file reads as
/// no lyrics instead of taking the caller down. Blocking: run it off the
/// async workers.
#[must_use]
pub fn read_file_lyrics(audio: &Path) -> Option<(LyricsSource, Lyrics)> {
    let sidecars = audio.parent().map(Sidecars::list).unwrap_or_default();
    read_lyrics_beside(audio, sidecars.for_track(audio))
}

/// [`read_file_lyrics`] with the `.lrc` file already found (from a
/// [`Sidecars`] listing of the track's folder), so a sweep lists each
/// folder once.
#[must_use]
pub fn read_lyrics_beside(audio: &Path, sidecar: Option<&Path>) -> Option<(LyricsSource, Lyrics)> {
    let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if let Some(lyrics) = sidecar.and_then(read_sidecar) {
            return Some((LyricsSource::Sidecar, lyrics));
        }
        read_tag_lyrics(audio).map(|lyrics| (LyricsSource::Tag, lyrics))
    }));
    read.unwrap_or_else(|_| {
        tracing::error!(path = %audio.display(), "lyrics read panicked; read as none");
        None
    })
}

/// The `.lrc` files of one folder (the extension in any case), regular
/// files only: links are skipped, as the walk skips them.
#[derive(Debug, Default)]
pub struct Sidecars {
    /// By stem. Where two differ only in the extension's case, the first
    /// by name wins.
    by_stem: BTreeMap<OsString, PathBuf>,
}

impl Sidecars {
    /// List `dir` once. A folder or entry that cannot be read is logged and
    /// left out.
    #[must_use]
    pub fn list(dir: &Path) -> Self {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(error) => {
                tracing::debug!(%error, dir = %dir.display(), "lyrics: folder not listed");
                return Self::default();
            }
        };
        let mut paths = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::debug!(%error, dir = %dir.display(), "lyrics: folder entry skipped");
                    continue;
                }
            };
            let path = entry.path();
            if !path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("lrc"))
            {
                continue;
            }
            match entry.file_type() {
                Ok(kind) if kind.is_file() => paths.push(path),
                Ok(_) => {}
                Err(error) => {
                    tracing::debug!(%error, path = %path.display(), "lyrics file skipped");
                }
            }
        }
        paths.sort();
        let mut by_stem = BTreeMap::new();
        for path in paths {
            if let Some(stem) = path.file_stem() {
                by_stem.entry(stem.to_owned()).or_insert(path);
            }
        }
        Self { by_stem }
    }

    /// The `.lrc` file named like `audio`: the same stem, exactly.
    #[must_use]
    pub fn for_track(&self, audio: &Path) -> Option<&Path> {
        self.by_stem.get(audio.file_stem()?).map(PathBuf::as_path)
    }

    /// Every `.lrc` file a track could take, in stem order.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.by_stem.values().map(PathBuf::as_path)
    }
}

/// Open for reading without following a link in the last component.
/// `flags` adds to `O_NOFOLLOW`.
fn open_no_follow(path: &Path, flags: libc::c_int) -> std::io::Result<File> {
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | flags)
        .open(path)
}

fn read_sidecar(path: &Path) -> Option<Lyrics> {
    let skip = |why: &dyn std::fmt::Display| {
        tracing::debug!(path = %path.display(), reason = %why, "lyrics file skipped");
    };
    // Non-blocking, so a FIFO swapped in after the listing cannot hang the
    // read; the regular-file check below then turns it away.
    let file = match open_no_follow(path, libc::O_NONBLOCK) {
        Ok(file) => file,
        Err(error) => {
            skip(&error);
            return None;
        }
    };
    match file.metadata() {
        Ok(meta) if meta.is_file() => {}
        Ok(_) => {
            skip(&"not a regular file");
            return None;
        }
        Err(error) => {
            skip(&error);
            return None;
        }
    }
    let mut bytes = Vec::new();
    // One byte past the cap tells an oversized file from one exactly at it.
    let cap = u64::try_from(MAX_LYRICS_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    if let Err(error) = file.take(cap).read_to_end(&mut bytes) {
        skip(&error);
        return None;
    }
    if bytes.len() > MAX_LYRICS_BYTES {
        skip(&LyricsRefused::TooManyBytes);
        return None;
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            skip(&"not UTF-8");
            return None;
        }
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    match parse_lyrics_text(text) {
        Ok(lyrics) => lyrics,
        Err(refused) => {
            skip(&refused);
            None
        }
    }
}

/// The first lyrics item of the file's tag that gives a line.
fn read_tag_lyrics(audio: &Path) -> Option<Lyrics> {
    let format = format_for_path(audio).ok()?;
    let values = match tag_lyrics_values(audio, format) {
        Ok(values) => values,
        Err(error) => {
            tracing::debug!(path = %audio.display(), %error, "lyrics tag not read");
            return None;
        }
    };
    values
        .iter()
        .find_map(|text| match parse_lyrics_text(text) {
            Ok(lyrics) => lyrics,
            Err(refused) => {
                tracing::debug!(path = %audio.display(), reason = %refused, "lyrics tag skipped");
                None
            }
        })
}

/// Every lyrics value in the file's tags: `Lyrics` items first, then
/// `UnsyncLyrics`.
fn tag_lyrics_values(audio: &Path, format: AudioFormat) -> Result<Vec<String>, String> {
    const KEYS: [ItemKey; 2] = [ItemKey::Lyrics, ItemKey::UnsyncLyrics];
    let mut file = open_no_follow(audio, 0).map_err(|error| error.to_string())?;
    if format == AudioFormat::Aac {
        let bytes = ape_tail(&mut file).map_err(|error| error.to_string())?;
        let items = super::read::parse_apev2(&bytes).unwrap_or_default();
        let mut values = Vec::new();
        for key in KEYS {
            for (name, texts) in &items {
                if ItemKey::from_key(TagType::Ape, name) == Some(key) {
                    values.extend(texts.iter().cloned());
                }
            }
        }
        return Ok(values);
    }
    let options = lofty::config::ParseOptions::new()
        .read_properties(false)
        .read_cover_art(false);
    let tagged = lofty::probe::Probe::new(std::io::BufReader::new(file))
        .options(options)
        .guess_file_type()
        .map_err(|error| error.to_string())?
        .read()
        .map_err(|error| error.to_string())?;
    let mut values = Vec::new();
    for key in KEYS {
        for tag in tagged.tags() {
            values.extend(tag.get_strings(key).map(str::to_owned));
        }
    }
    Ok(values)
}

/// Largest APEv2 tag read from the end of an AAC file. It can hold a
/// cover, so it is allowed more room than the lyrics alone.
const MAX_APE_TAG_BYTES: u64 = 32 * 1024 * 1024;

/// The APEv2 tag at the end of `file`, footer included, or nothing when
/// the file does not end in one. Only the tag is read, never the audio.
fn ape_tail(file: &mut File) -> std::io::Result<Vec<u8>> {
    let length = file.seek(SeekFrom::End(0))?;
    if length < 32 {
        return Ok(Vec::new());
    }
    let mut footer = [0u8; 32];
    file.seek(SeekFrom::Start(length - 32))?;
    file.read_exact(&mut footer)?;
    if &footer[..8] != b"APETAGEX" {
        return Ok(Vec::new());
    }
    let tag_size = u64::from(u32::from_le_bytes([
        footer[12], footer[13], footer[14], footer[15],
    ]));
    // A size that counts the header too is read whole: the parser tries
    // both starts.
    let wanted = tag_size.saturating_add(32).min(length);
    if wanted > MAX_APE_TAG_BYTES {
        return Ok(Vec::new());
    }
    file.seek(SeekFrom::Start(length - wanted))?;
    let mut bytes = Vec::new();
    file.take(wanted).read_to_end(&mut bytes)?;
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_stamps_parse_to_milliseconds() {
        assert_eq!(
            parse_lrc_line("[01:02.34] hello"),
            Some(("hello".to_owned(), vec![62_340]))
        );
        assert_eq!(
            parse_lrc_line("[00:00.5] x"),
            Some(("x".to_owned(), vec![500]))
        );
        assert_eq!(parse_lrc_line("[ar:Someone]"), None);
        assert_eq!(parse_lrc_line("[99:99.99] x"), None);
        assert_eq!(parse_lrc_line("no stamp"), None);
    }

    #[test]
    fn plain_text_drops_empty_lines_and_caps_hold() {
        let plain = parse_lyrics_text("One\n\n  Two  ").unwrap().unwrap();
        assert!(!plain.synced);
        assert_eq!(
            plain.lines,
            [("One".to_owned(), None), ("Two".to_owned(), None)]
        );
        assert_eq!(parse_lyrics_text(" \n[00:01.00]\n").unwrap(), None);
        assert_eq!(
            parse_lyrics(Some(&"x".repeat(MAX_LYRICS_BYTES + 1)), None),
            Err(LyricsRefused::TooManyBytes)
        );
    }
}
