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
//! Text with LRC line or word stamps counts as synced, wherever it came
//! from.
//!
//! Every source becomes the same [`Lyrics`]: a main track and any
//! translation or pronunciation tracks, each a list of lines, and a line
//! may carry word cues. All of it is built by the one line builder
//! (`line`), which holds the rules for text, times, cues and agents.
//!
//! Everything here only reads. The lyrics sweep and the serving path both
//! call [`read_file_lyrics`], so a track marked as having lyrics is one
//! that can be served.

mod line;
mod lrc;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};

use lofty::file::TaggedFileExt as _;
use lofty::tag::{ItemKey, TagType};

pub use line::{LineInput, MAX_TIME_MS, Piece};
pub use lrc::{parse_lyrics, parse_lyrics_text};

use super::{AudioFormat, TrackFile, format_for_path};

/// Largest lyrics payload read, in bytes.
pub const MAX_LYRICS_BYTES: usize = 2 * 1024 * 1024;
/// Largest lyrics payload read, in characters.
pub const MAX_LYRICS_CHARACTERS: usize = 1_000_000;

/// A track's lyrics: the lyrics as sung, and any translations and
/// pronunciations, each a track of its own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lyrics {
    /// The lyrics as sung.
    pub main: LyricsTrack,
    /// Translations and pronunciations, in the order the source lists them.
    pub layers: Vec<LyricsTrack>,
}

/// What a lyrics track holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricsKind {
    /// The lyrics as sung.
    Main,
    /// A translation into another language.
    Translation,
    /// How the words sound, written in another script.
    Pronunciation,
}

/// One track of lines, made only by the line builder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricsTrack {
    kind: LyricsKind,
    lang: Option<String>,
    synced: bool,
    agents: Vec<Agent>,
    lines: Vec<LyricLine>,
}

impl LyricsTrack {
    /// What the track holds.
    #[must_use]
    pub fn kind(&self) -> LyricsKind {
        self.kind
    }

    /// Its language tag, when the source names one.
    #[must_use]
    pub fn lang(&self) -> Option<&str> {
        self.lang.as_deref()
    }

    /// True when the lines carry times.
    #[must_use]
    pub fn synced(&self) -> bool {
        self.synced
    }

    /// The singers its cue lines name. Empty unless a line has more than
    /// one cue line or cue lines name two or more agents; then exactly one
    /// is [`AgentRole::Main`] and every cue line names one.
    #[must_use]
    pub fn agents(&self) -> &[Agent] {
        &self.agents
    }

    /// The lines, in order.
    #[must_use]
    pub fn lines(&self) -> &[LyricLine] {
        &self.lines
    }
}

/// A singer, a group, or background vocals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Agent {
    id: String,
    role: AgentRole,
    name: Option<String>,
}

impl Agent {
    /// Its id, unique in its track.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// What part it sings.
    #[must_use]
    pub fn role(&self) -> AgentRole {
        self.role
    }

    /// Its name, when the source gives one.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }
}

/// What part an agent sings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentRole {
    /// The lead.
    Main,
    /// Another singer.
    Voice,
    /// Background vocals.
    Bg,
    /// A group singing together.
    Group,
}

/// One line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LyricLine {
    text: String,
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    cue_lines: Vec<CueLine>,
}

impl LyricLine {
    /// The line's text: the main part, then any background part after a
    /// space. Empty for a stamped blank line.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// When the line starts, in milliseconds.
    #[must_use]
    pub fn start_ms(&self) -> Option<i64> {
        self.start_ms
    }

    /// When the line ends, in milliseconds: only with a start, never
    /// before it, and never taken from the next line.
    #[must_use]
    pub fn end_ms(&self) -> Option<i64> {
        self.end_ms
    }

    /// Its word-timed parts, the main part first. Empty when the line has
    /// no word timing.
    #[must_use]
    pub fn cue_lines(&self) -> &[CueLine] {
        &self.cue_lines
    }
}

/// One word-timed part of a line, sung by one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CueLine {
    agent: Option<usize>,
    text: String,
    cues: Vec<Cue>,
}

impl CueLine {
    /// Index into its track's [`LyricsTrack::agents`], when it has agents.
    #[must_use]
    pub fn agent(&self) -> Option<usize> {
        self.agent
    }

    /// The part's text, which the cues' byte offsets point into.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Its cues: at least one, in order, each ending at or before the next
    /// one starts.
    #[must_use]
    pub fn cues(&self) -> &[Cue] {
        &self.cues
    }

    /// Its start and end in milliseconds: the first cue's start, and the
    /// later of the line's end (`line_end_ms`) and the last cue's end.
    #[must_use]
    pub fn span(&self, line_end_ms: Option<i64>) -> (i64, i64) {
        let start = self.cues.first().map_or(0, |cue| cue.start_ms);
        let last = self.cues.last().map_or(start, |cue| cue.end_ms);
        (start, line_end_ms.map_or(last, |end| end.max(last)))
    }
}

/// One timed word or syllable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cue {
    start_ms: i64,
    end_ms: i64,
    byte_start: usize,
    byte_end: usize,
}

impl Cue {
    /// When it starts, in milliseconds.
    #[must_use]
    pub fn start_ms(&self) -> i64 {
        self.start_ms
    }

    /// When it ends, in milliseconds.
    #[must_use]
    pub fn end_ms(&self) -> i64 {
        self.end_ms
    }

    /// The first byte of its text in the cue line's text (UTF-8, 0-based).
    #[must_use]
    pub fn byte_start(&self) -> usize {
        self.byte_start
    }

    /// The last byte of its text, inclusive.
    #[must_use]
    pub fn byte_end(&self) -> usize {
        self.byte_end
    }
}

/// A language tag as a source gives it, when it looks like one: 2 to 35
/// ASCII letters, digits and hyphens.
fn valid_lang(value: &str) -> Option<String> {
    ((2..=35).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'))
    .then(|| value.to_owned())
}

/// Where a file's lyrics were found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricsSource {
    /// A lyrics file beside the track.
    Sidecar,
    /// The file's own lyrics tag.
    Tag,
}

droppedneedle_kernel::spelled!(LyricsSource {
    Sidecar => "sidecar",
    Tag => "tag",
});

/// Why a payload was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LyricsRefused {
    #[error("lyrics payload exceeds the byte cap")]
    TooManyBytes,
    #[error("lyrics payload exceeds the character cap")]
    TooManyCharacters,
}

/// The lyrics `audio` carries: its `.lrc` file first, then its tag. `None`
/// when neither gives a line. A parser panic on a malformed file reads as
/// no lyrics instead of taking the caller down. Blocking: run it off the
/// async workers.
#[must_use]
pub fn read_file_lyrics(audio: TrackFile<'_>) -> Option<(LyricsSource, Lyrics)> {
    let sidecars = audio
        .path()
        .parent()
        .map(Sidecars::list)
        .unwrap_or_default();
    read_lyrics_beside(audio, sidecars.for_track(audio.path()))
}

/// [`read_file_lyrics`] with the `.lrc` file already found (from a
/// [`Sidecars`] listing of the track's folder), so a sweep lists each
/// folder once. The `.lrc` file is read beneath the track's folder.
#[must_use]
pub fn read_lyrics_beside(
    audio: TrackFile<'_>,
    sidecar: Option<&Path>,
) -> Option<(LyricsSource, Lyrics)> {
    read_lyrics_checked(audio, sidecar).0
}

/// [`read_lyrics_beside`], and whether a file could not be read: then no
/// lyrics found is not the same as none there.
#[must_use]
pub fn read_lyrics_checked(
    audio: TrackFile<'_>,
    sidecar: Option<&Path>,
) -> (Option<(LyricsSource, Lyrics)>, bool) {
    let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut unread = false;
        if let Some(path) = sidecar {
            match read_sidecar(audio.beside(path)) {
                Ok(Some(lyrics)) => return (Some((LyricsSource::Sidecar, lyrics)), false),
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(path = %path.display(), %error, "lyrics file not read");
                    unread = true;
                }
            }
        }
        match read_tag_lyrics(audio) {
            Ok(lyrics) => (lyrics.map(|lyrics| (LyricsSource::Tag, lyrics)), unread),
            Err(error) => {
                tracing::debug!(path = %audio.path().display(), %error, "lyrics tag not read");
                (None, true)
            }
        }
    }));
    read.unwrap_or_else(|_| {
        tracing::error!(path = %audio.path().display(), "lyrics read panicked; read as none");
        (None, false)
    })
}

/// The `.lrc` files of one folder (the extension in any case), regular
/// files only: links are skipped, as the walk skips them.
#[derive(Debug, Default)]
pub struct Sidecars {
    /// By stem. Where two differ only in the extension's case, the first
    /// by name wins.
    by_stem: BTreeMap<OsString, PathBuf>,
    /// The folder, or an entry of it, could not be read.
    unread: bool,
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
                return Self {
                    unread: true,
                    ..Self::default()
                };
            }
        };
        let mut paths = Vec::new();
        let mut unread = false;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    tracing::debug!(%error, dir = %dir.display(), "lyrics: folder entry skipped");
                    unread = true;
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
                    unread = true;
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
        Self { by_stem, unread }
    }

    /// True when the listing may have missed a file: the folder, or an
    /// entry of it, could not be read.
    #[must_use]
    pub fn unread(&self) -> bool {
        self.unread
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

/// The lyrics of a `.lrc` file. `Err` when it could not be read, `None`
/// when what it holds does not count.
fn read_sidecar(at: TrackFile<'_>) -> std::io::Result<Option<Lyrics>> {
    let skip = |why: &dyn std::fmt::Display| {
        tracing::debug!(path = %at.path().display(), reason = %why, "lyrics file skipped");
    };
    let file = at.open()?;
    let mut bytes = Vec::new();
    // One byte past the cap tells an oversized file from one exactly at it.
    let cap = u64::try_from(MAX_LYRICS_BYTES)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    file.take(cap).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_LYRICS_BYTES {
        skip(&LyricsRefused::TooManyBytes);
        return Ok(None);
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            skip(&"not UTF-8");
            return Ok(None);
        }
    };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    Ok(match parse_lyrics_text(text) {
        Ok(lyrics) => lyrics,
        Err(refused) => {
            skip(&refused);
            None
        }
    })
}

/// The first lyrics item of the file's tag that gives a line. `Err` when
/// the file could not be read; a tag that does not parse holds no lyrics.
fn read_tag_lyrics(at: TrackFile<'_>) -> std::io::Result<Option<Lyrics>> {
    let audio = at.path();
    let Ok(format) = format_for_path(audio) else {
        return Ok(None);
    };
    let values = match tag_lyrics_values(at, format) {
        Ok(values) => values,
        Err(TagValuesError::Read(error)) => return Err(error),
        Err(TagValuesError::Parse(error)) => {
            tracing::debug!(path = %audio.display(), %error, "lyrics tag not parsed");
            return Ok(None);
        }
    };
    Ok(values.iter().find_map(|text| tag_text_lyrics(audio, text)))
}

/// The lyrics one tag value holds.
fn tag_text_lyrics(audio: &Path, text: &str) -> Option<Lyrics> {
    match parse_lyrics_text(text) {
        Ok(lyrics) => lyrics,
        Err(refused) => {
            tracing::debug!(path = %audio.display(), reason = %refused, "lyrics tag skipped");
            None
        }
    }
}

/// Why a file's lyrics tags were not had.
enum TagValuesError {
    /// The file could not be read.
    Read(std::io::Error),
    /// It was read and does not parse.
    Parse(String),
}

impl From<std::io::Error> for TagValuesError {
    fn from(error: std::io::Error) -> Self {
        if error.kind() == std::io::ErrorKind::UnexpectedEof {
            Self::Parse(error.to_string())
        } else {
            Self::Read(error)
        }
    }
}

/// Every lyrics value in the file's tags: `Lyrics` items first, then
/// `UnsyncLyrics`.
fn tag_lyrics_values(
    audio: TrackFile<'_>,
    format: AudioFormat,
) -> Result<Vec<String>, TagValuesError> {
    const KEYS: [ItemKey; 2] = [ItemKey::Lyrics, ItemKey::UnsyncLyrics];
    let mut file = audio.open()?;
    if format == AudioFormat::Aac {
        let bytes = ape_tail(&mut file)?;
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
    let opened = file.try_clone()?;
    let tagged = lofty::probe::Probe::new(std::io::BufReader::new(file))
        .options(options)
        .guess_file_type()?
        .read()
        .map_err(|error| {
            if super::read::is_read_failure(&error) {
                TagValuesError::Read(std::io::Error::other(error.to_string()))
            } else {
                match super::read::read_through(&opened) {
                    Err(source) => TagValuesError::from(source),
                    Ok(()) => TagValuesError::Parse(error.to_string()),
                }
            }
        })?;
    super::read::confirm_untagged_mp4(&tagged, &mut opened.try_clone()?)?;
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
