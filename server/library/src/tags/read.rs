//! Tag reads through lofty's generic items.
//!
//! Rule zero (see `save/mod.rs`) cuts both ways: native tags are for
//! writes, and generic items are for reads. The generic `Tag` lofty builds
//! on read is complete for every field the app surfaces (multi-values arrive as
//! separate items, UFID and TXXX spellings included), while the native
//! round-trip mangles known `TXXX` description case and drops follow-on
//! values on several text frames. So reads never touch native tags.
//!
//! One gap: lofty 0.25.4 drops a v2.4 `TDRC` frame from an ID3v2.3 tag
//! when it carries a month or day (`construct_tdrc_from_v3` in
//! `id3/v2/read.rs`), so such a tag with no `TYER` reads with no recording
//! date. Only then does the date come from the tag editor's own parse, so
//! scan, the editor and v2 agree. A tag holding both `TYER` and a full
//! `TDRC` still reads differently in scan and the editor.
//!
//! Two documented divergences from v2: lofty translates numeric ID3 genre
//! references (`(12)` becomes the named genre) where mutagen returned the
//! raw text, and a v2.3 `TYER` now yields a year where mutagen's `TDRC`
//! lookup found none. Both only affect files v2 already handled oddly.
//!
//! AAC carries an APEv2 tag lofty does not read, so this module parses that
//! tag itself (reads only; AAC stays read-only for writes).

use std::fs::File;
use std::io::{BufReader, Read as _, Seek as _};
use std::path::Path;

use lofty::config::{ParseOptions, ParsingMode};
use lofty::error::FileParseError;
use lofty::file::{AudioFile as _, FileType, TaggedFile, TaggedFileExt as _};
use lofty::probe::Probe;
use lofty::tag::{ItemKey, Tag, TagType};

use super::{AudioFormat, TagsError, TrackFile, format_for_path};

/// One format-native artist value. No joined scalar is ever split.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioArtistCredit {
    pub name: String,
    pub credited_name: Option<String>,
    pub sort_name: Option<String>,
    pub musicbrainz_artist_id: Option<String>,
    pub join_phrase: String,
}

/// Tag metadata read from an audio file, with the fields of v2's `AudioTag`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AudioTag {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub track_number: u32,
    pub album_artist: Option<String>,
    pub disc_number: u32,
    pub year: Option<i32>,
    pub genre: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub musicbrainz_release_id: Option<String>,
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_release_track_id: Option<String>,
    pub musicbrainz_artist_id: Option<String>,
    pub musicbrainz_album_artist_id: Option<String>,
    pub acoustid_id: Option<String>,
    pub compilation: bool,
    pub release_type: Option<String>,
    pub title_sort: Option<String>,
    pub artist_sort: Option<String>,
    pub album_sort: Option<String>,
    pub album_artist_sort: Option<String>,
    pub disc_subtitle: Option<String>,
    pub original_release_date: Option<String>,
    pub replaygain_track_gain: Option<f64>,
    pub replaygain_album_gain: Option<f64>,
    pub replaygain_track_peak: Option<f64>,
    pub replaygain_album_peak: Option<f64>,
    pub genres: Vec<String>,
    pub artists: Vec<AudioArtistCredit>,
    pub album_artists: Vec<AudioArtistCredit>,
    pub musicbrainz_artist_ids: Vec<String>,
    pub musicbrainz_album_artist_ids: Vec<String>,
    /// The track artist names came from the multi-valued `ARTISTS` key,
    /// so they line up one for one with `musicbrainz_artist_ids`.
    pub artists_listed: bool,
    /// The same for `ALBUMARTISTS` and `musicbrainz_album_artist_ids`.
    pub album_artists_listed: bool,
    /// Edition hints: `MEDIA`, `BARCODE`, `CATALOGNUMBER`,
    /// `RELEASECOUNTRY`, the disc total and the release date.
    pub edition: EditionTags,
}

/// The tags that tell editions of one album apart.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct EditionTags {
    pub media: Option<String>,
    pub barcode: Option<String>,
    pub catalog_number: Option<String>,
    pub release_country: Option<String>,
    pub total_discs: Option<u32>,
    /// `YYYY[-MM[-DD]]`, read as [`release_date`] describes.
    pub release_date: Option<String>,
    /// The track total of the file's disc, as [`track_total`] reads it.
    pub total_tracks: Option<u32>,
}

/// Written by hand so an absent release date or track total prints
/// nothing. A file's `tag_revision` hashes `AudioTag`'s `Debug` text with
/// these hints empty, and that text stays what it was before either was
/// read, so an unchanged file keeps its revision.
impl std::fmt::Debug for EditionTags {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut out = f.debug_struct("EditionTags");
        out.field("media", &self.media)
            .field("barcode", &self.barcode)
            .field("catalog_number", &self.catalog_number)
            .field("release_country", &self.release_country)
            .field("total_discs", &self.total_discs);
        if let Some(release_date) = &self.release_date {
            out.field("release_date", release_date);
        }
        if let Some(total_tracks) = &self.total_tracks {
            out.field("total_tracks", total_tracks);
        }
        out.finish()
    }
}

/// Read tags plus technical info.
pub fn read_tags(at: TrackFile<'_>) -> Result<(AudioTag, super::AudioInfo), TagsError> {
    let format = format_for_path(at.path())?;
    let tag = read_tag_only(at, format)?;
    let info = super::probe(at)?;
    Ok((tag, info))
}

/// Stream properties a scan stores, all from container headers.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HeaderInfo {
    pub duration_seconds: Option<f64>,
    /// Audio bitrate in kbit/s (container average when the stream has none).
    pub bitrate_kbps: Option<u32>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    /// Meaningful bit depth: lossless containers only, as v2 stored it.
    pub bit_depth: Option<u8>,
    /// The duration was measured, not estimated. Only an MP3 can be
    /// estimated: lofty measures one from a Xing, Info or VBRI frame count
    /// ([`super::mpeg_length`]) and otherwise estimates it from the first
    /// frame's bitrate.
    pub duration_measured: bool,
}

/// Tags plus header properties for one file, the way the scan indexes it.
/// Reads only the tag blocks and stream headers the container parser
/// needs and never decodes audio. ADTS has no container header, so its
/// duration comes from counting frames, still without a decode.
pub fn read_scan_metadata(
    at: TrackFile<'_>,
    format: AudioFormat,
) -> Result<(AudioTag, HeaderInfo), TagsError> {
    let path = at.path();
    if format == AudioFormat::Aac {
        let tag = read_ape_tag(at)?;
        let header = match super::probe(at) {
            Ok(info) => HeaderInfo {
                duration_seconds: Some(info.duration_seconds),
                bitrate_kbps: Some(info.bitrate).filter(|rate| *rate > 0),
                sample_rate: Some(info.sample_rate).filter(|rate| *rate > 0),
                channels: Some(info.channels).filter(|count| *count > 0),
                bit_depth: None,
                duration_measured: true,
            },
            Err(_) => HeaderInfo {
                duration_measured: true,
                ..HeaderInfo::default()
            },
        };
        return Ok((tag, header));
    }
    let tagged = read_lenient(at).map_err(|error| {
        // An I/O failure (a file vanishing mid-scan, a contended disk) is
        // worth a retry; anything else is a broken file.
        let io =
            io_source(&error).map(|source| std::io::Error::new(source.kind(), source.to_string()));
        match io {
            Some(source) => TagsError::Io {
                path: path.display().to_string(),
                source,
            },
            None => TagsError::TagRead {
                path: path.display().to_string(),
                reason: error.to_string(),
            },
        }
    })?;
    let tag = audio_tag_from_file(at, &tagged, format);
    let mut header = header_of(&tagged, format);
    header.duration_measured = format != AudioFormat::Mp3
        || super::mpeg_length::length_measured(at).map_err(|source| TagsError::Io {
            path: path.display().to_string(),
            source,
        })?;
    Ok((tag, header))
}

/// [`HeaderInfo`] from a file the caller has open (say, one opened without
/// following a link), read as the container `path`'s extension names, from
/// its start. The shared file position moves, and the duration counts as
/// not measured.
pub fn read_header_from(file: &File, path: &Path) -> Result<HeaderInfo, TagsError> {
    let format = format_for_path(path)?;
    Ok(header_of(&tagged_from(file, path)?, format))
}

/// The stream properties lofty read, with the duration not yet counted as
/// measured.
fn header_of(tagged: &TaggedFile, format: AudioFormat) -> HeaderInfo {
    let properties = tagged.properties();
    let duration = properties.duration();
    let lossless = matches!(
        format,
        AudioFormat::Flac | AudioFormat::Wav | AudioFormat::M4a
    );
    HeaderInfo {
        duration_seconds: (!duration.is_zero()).then_some(duration.as_secs_f64()),
        bitrate_kbps: properties
            .audio_bitrate()
            .or_else(|| properties.overall_bitrate())
            .filter(|rate| *rate > 0),
        sample_rate: properties.sample_rate().filter(|rate| *rate > 0),
        channels: properties
            .channels()
            .map(u32::from)
            .filter(|count| *count > 0),
        bit_depth: if lossless {
            properties.bit_depth().filter(|depth| *depth > 0)
        } else {
            None
        },
        duration_measured: false,
    }
}

/// The file as the catalog reads it. lofty's default mode comes first,
/// so a file that reads there reads exactly as before. Only when that
/// fails on the file's content is it read again in `Relaxed` mode, which
/// drops an item lofty cannot parse (a `TDRL` holding a web address
/// rather than a date) instead of failing the whole file, so the track is
/// still listed. Writes keep the default mode and refuse such a file.
fn read_lenient(at: TrackFile<'_>) -> Result<TaggedFile, lofty::error::FileParseError> {
    let path = at.path();
    let file = at.open()?;
    let read = |options: ParseOptions| -> Result<TaggedFile, FileParseError> {
        let mut reread = file.try_clone()?;
        let mut file = file.try_clone()?;
        file.rewind()?;
        let mut probe = Probe::new(BufReader::new(file)).options(options);
        if let Some(file_type) = FileType::from_path(path) {
            probe = probe.set_file_type(file_type);
        }
        let tagged = probe.read()?;
        confirm_untagged_mp4(&tagged, &mut reread)?;
        Ok(tagged)
    };
    match read(ParseOptions::new()) {
        Err(error) if io_source(&error).is_none() => {
            tracing::debug!(path = %path.display(), %error, "tags read again in relaxed mode");
            read(ParseOptions::new().parsing_mode(ParsingMode::Relaxed))
        }
        read => read,
    }
}

/// lofty walks an MP4's `moov` while each child atom's header reads, so a
/// failed read there ends the walk and the file parses with no tag. On a
/// failing mount that would read as an untagged file. An MP4 that parsed
/// with no tag is only believed once it reads to its end.
pub(super) fn confirm_untagged_mp4<R: std::io::Read + std::io::Seek>(
    tagged: &TaggedFile,
    reader: &mut R,
) -> std::io::Result<()> {
    if tagged.file_type() != FileType::Mp4 || !tagged.tags().is_empty() {
        return Ok(());
    }
    reader.rewind()?;
    std::io::copy(reader, &mut std::io::sink()).map(|_| ())
}

/// The I/O error under a lofty read failure, if that is what it was.
fn io_source(error: &lofty::error::FileParseError) -> Option<&std::io::Error> {
    std::error::Error::source(error).and_then(|source| source.downcast_ref::<std::io::Error>())
}

/// True when a tag read failed because the file could not be read, rather
/// than because of what it holds. A file that ends early reads as damaged.
pub(super) fn is_read_failure(error: &FileParseError) -> bool {
    nested_io_error(error).is_some_and(|source| source.kind() != std::io::ErrorKind::UnexpectedEof)
}

/// The I/O error anywhere under a parse error: lofty nests it under the
/// atom, page or tag it was reading. [`io_source`] stops at the first
/// level on purpose, so the scan's relaxed re-read still runs for a
/// truncated tag.
fn nested_io_error(error: &FileParseError) -> Option<&std::io::Error> {
    let mut source = std::error::Error::source(error);
    while let Some(error) = source {
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            return Some(io);
        }
        source = error.source();
    }
    None
}

/// Read a file that did not parse to its end. lofty drops some I/O errors:
/// looking for MP4's `moov` it stops at any failed read and reports no such
/// atom. Only a file that reads to its end is damaged.
pub(super) fn read_through(file: &File) -> std::io::Result<()> {
    let mut file = file.try_clone()?;
    file.rewind()?;
    std::io::copy(&mut file, &mut std::io::sink()).map(|_| ())
}

/// Read the tag half only. The probe half lives in [`super::probe`].
pub fn read_tag_only(at: TrackFile<'_>, format: AudioFormat) -> Result<AudioTag, TagsError> {
    if format == AudioFormat::Aac {
        return read_ape_tag(at);
    }
    let tagged = read_lenient(at).map_err(|error| TagsError::TagRead {
        path: at.path().display().to_string(),
        reason: error.to_string(),
    })?;
    Ok(audio_tag_from_file(at, &tagged, format))
}

/// The file's preferred tag through the one mapping, with the recording
/// date lofty leaves out of a mixed ID3v2.3 tag. The file is read again
/// only for an MP3 whose ID3 tag gave no date.
fn audio_tag_from_file(at: TrackFile<'_>, tagged: &TaggedFile, format: AudioFormat) -> AudioTag {
    let Some(tag) = select_tag(tagged, format) else {
        return AudioTag::default();
    };
    let date = if format == AudioFormat::Mp3
        && tag.tag_type() == TagType::Id3v2
        && values_for(tag, ItemKey::RecordingDate).is_empty()
    {
        read_mixed_v23_date(at)
    } else {
        None
    };
    audio_tag_from_values(tag.tag_type(), |key| match &date {
        Some(date) if key == ItemKey::RecordingDate => vec![date.clone()],
        _ => values_for(tag, key),
    })
}

/// The tag editor's date for a mixed ID3v2.3 tag. A file that cannot be
/// read again gives no date: lofty has already read it once.
fn read_mixed_v23_date(at: TrackFile<'_>) -> Option<String> {
    let bytes = at.open().and_then(|mut file| id3v23_tag_bytes(&mut file));
    match bytes {
        Ok(bytes) => bytes
            .and_then(|bytes| super::save::mixed_v23_date(&bytes))
            .map(|date| date.trim().to_owned())
            .filter(|date| !date.is_empty()),
        Err(error) => {
            tracing::debug!(path = %at.path().display(), %error, "ID3v2.3 date not read");
            None
        }
    }
}

/// The ID3v2 tag at the start of an MP3, header included, when it is
/// v2.3 and no larger than lofty's own allocation limit. `None` for any
/// other file; an oversized tag's body is never read.
fn id3v23_tag_bytes(file: &mut impl std::io::Read) -> std::io::Result<Option<Vec<u8>>> {
    let mut bytes = Vec::with_capacity(10);
    (&mut *file).take(10).read_to_end(&mut bytes)?;
    if bytes.len() < 10 || &bytes[..3] != b"ID3" || bytes[3] != 3 {
        return Ok(None);
    }
    let size = bytes[6..10]
        .iter()
        .fold(0usize, |acc, byte| (acc << 7) | usize::from(byte & 0x7F));
    if size > lofty::config::GlobalOptions::DEFAULT_ALLOCATION_LIMIT {
        return Ok(None);
    }
    file.take(size as u64).read_to_end(&mut bytes)?;
    Ok(Some(bytes))
}

/// Preferred tag for the format, else the first tag lofty parsed.
fn select_tag(tagged: &TaggedFile, format: AudioFormat) -> Option<&Tag> {
    let preferred = match format {
        AudioFormat::Mp3 => Some(TagType::Id3v2),
        AudioFormat::Flac | AudioFormat::Ogg | AudioFormat::Opus => Some(TagType::VorbisComments),
        AudioFormat::M4a => Some(TagType::Mp4Ilst),
        AudioFormat::Wav => Some(TagType::Id3v2),
        AudioFormat::Aac => None,
    };
    preferred
        .and_then(|tag_type| tagged.tag(tag_type))
        .or_else(|| tagged.first_tag())
}

/// First embedded cover-art blob, or `None` when the file has none.
pub fn read_cover_art(at: TrackFile<'_>) -> Result<Option<Vec<u8>>, TagsError> {
    let path = at.path();
    // WMA is not read: the extension is rejected before any reader runs.
    format_for_path(path)?;
    let file = at.open().map_err(|source| TagsError::Io {
        path: path.display().to_string(),
        source,
    })?;
    read_cover_art_from(&file, path)
}

/// [`read_cover_art`] over a file the caller has open (say, one opened
/// without following a link), read as the container `path`'s extension
/// names, from its start. The shared file position moves.
pub fn read_cover_art_from(file: &File, path: &Path) -> Result<Option<Vec<u8>>, TagsError> {
    format_for_path(path)?;
    let tagged = tagged_from(file, path)?;
    Ok(tagged
        .tags()
        .iter()
        .find_map(|tag| tag.pictures().first())
        .map(|picture| picture.data().to_vec()))
}

/// The open file as lofty reads it, from its start.
fn tagged_from(file: &File, path: &Path) -> Result<TaggedFile, TagsError> {
    let failed = |reason: String| TagsError::TagRead {
        path: path.display().to_string(),
        reason,
    };
    let file_type = FileType::from_path(path).ok_or_else(|| failed("unknown container".into()))?;
    let read = |options: Option<ParseOptions>| -> Result<TaggedFile, FileParseError> {
        let mut file = file.try_clone()?;
        file.rewind()?;
        let mut reread = file.try_clone()?;
        let probe = Probe::with_file_type(BufReader::new(file), file_type);
        let tagged = match options {
            Some(options) => probe.options(options).read(),
            None => probe.read(),
        }?;
        confirm_untagged_mp4(&tagged, &mut reread)?;
        Ok(tagged)
    };
    // The same two passes as `read_lenient`.
    match read(None) {
        Err(error) if io_source(&error).is_none() && !is_read_failure(&error) => {
            read(Some(ParseOptions::new().parsing_mode(ParsingMode::Relaxed)))
        }
        read => read,
    }
    .map_err(|error| match nested_io_error(&error) {
        Some(source) if is_read_failure(&error) => TagsError::Io {
            path: path.display().to_string(),
            source: std::io::Error::new(source.kind(), source.to_string()),
        },
        _ => match read_through(file) {
            Err(source) if source.kind() != std::io::ErrorKind::UnexpectedEof => TagsError::Io {
                path: path.display().to_string(),
                source,
            },
            _ => failed(error.to_string()),
        },
    })
}

// ---------------------------------------------------------------------------
// The one mapping: generic items to `AudioTag`.
// ---------------------------------------------------------------------------

/// Collect the values lofty read for one key, split on NUL the way v2
/// splits multi-valued frames, de-duplicated in order.
fn values_for(tag: &Tag, key: ItemKey) -> Vec<String> {
    let mut out = Vec::new();
    collect_values(tag.get_strings(key), &mut out);
    out
}

/// Split each raw item on NUL, trim, and push de-duplicated in order.
/// Compares against the trimmed slice so the check itself allocates
/// nothing on the hot scan path.
fn collect_values<'a>(items: impl Iterator<Item = &'a str>, out: &mut Vec<String>) {
    for item in items {
        for part in item.split('\0') {
            let trimmed = part.trim();
            if !trimmed.is_empty() && !out.iter().any(|seen| seen == trimmed) {
                out.push(trimmed.to_owned());
            }
        }
    }
}

fn audio_tag_from_values(
    tag_type: TagType,
    values_of: impl Fn(ItemKey) -> Vec<String>,
) -> AudioTag {
    let first = |key: ItemKey| -> Option<String> { values_of(key).into_iter().next() };
    let genres = values_of(ItemKey::Genre);
    let artist_ids = values_of(ItemKey::MusicBrainzArtistId);
    let album_artist_ids = values_of(ItemKey::MusicBrainzReleaseArtistId);
    let mut artist_names = values_of(ItemKey::TrackArtists);
    let artists_listed = !artist_names.is_empty();
    if !artists_listed {
        artist_names = values_of(ItemKey::TrackArtist);
    }
    let mut album_artist_names = values_of(ItemKey::AlbumArtists);
    let album_artists_listed = !album_artist_names.is_empty();
    if !album_artists_listed {
        album_artist_names = values_of(ItemKey::AlbumArtist);
    }
    let artist_sort = first(ItemKey::TrackArtistSortOrder);
    let album_artist_sort = first(ItemKey::AlbumArtistSortOrder);
    // APE maps "Track"/"Disc" to both the number and the total keys,
    // and ID3 splits "n/m" the same way; either side may carry the value.
    let track_number = leading_int(&values_of(ItemKey::TrackNumber))
        .or_else(|| leading_int(&values_of(ItemKey::TrackTotal)))
        .unwrap_or(0);
    let disc_number = leading_int(&values_of(ItemKey::DiscNumber))
        .or_else(|| leading_int(&values_of(ItemKey::DiscTotal)))
        .unwrap_or(1);
    AudioTag {
        title: first(ItemKey::TrackTitle).unwrap_or_default(),
        artist: first(ItemKey::TrackArtist).unwrap_or_default(),
        album: first(ItemKey::AlbumTitle).unwrap_or_default(),
        album_artist: first(ItemKey::AlbumArtist),
        track_number,
        disc_number,
        year: year_of(&values_of(ItemKey::RecordingDate)),
        genre: join_all(&genres),
        compilation: first(ItemKey::FlagCompilation).as_deref() == Some("1"),
        release_type: first(ItemKey::MusicBrainzReleaseType),
        title_sort: first(ItemKey::TrackTitleSortOrder),
        artist_sort: artist_sort.clone(),
        album_sort: first(ItemKey::AlbumTitleSortOrder),
        album_artist_sort: album_artist_sort.clone(),
        disc_subtitle: first(ItemKey::SetSubtitle),
        original_release_date: first(ItemKey::OriginalReleaseDate),
        replaygain_track_gain: gain(&values_of(ItemKey::ReplayGainTrackGain), false),
        replaygain_album_gain: gain(&values_of(ItemKey::ReplayGainAlbumGain), false),
        replaygain_track_peak: gain(&values_of(ItemKey::ReplayGainTrackPeak), true),
        replaygain_album_peak: gain(&values_of(ItemKey::ReplayGainAlbumPeak), true),
        genres,
        artists: artist_credits(&artist_names, &artist_ids, artist_sort),
        album_artists: artist_credits(&album_artist_names, &album_artist_ids, album_artist_sort),
        musicbrainz_artist_ids: artist_ids,
        musicbrainz_album_artist_ids: album_artist_ids,
        artists_listed,
        album_artists_listed,
        musicbrainz_release_group_id: first(ItemKey::MusicBrainzReleaseGroupId),
        musicbrainz_release_id: first(ItemKey::MusicBrainzReleaseId),
        musicbrainz_recording_id: first(ItemKey::MusicBrainzRecordingId),
        musicbrainz_release_track_id: first(ItemKey::MusicBrainzTrackId),
        musicbrainz_artist_id: first(ItemKey::MusicBrainzArtistId),
        musicbrainz_album_artist_id: first(ItemKey::MusicBrainzReleaseArtistId),
        acoustid_id: first(ItemKey::AcoustId),
        edition: EditionTags {
            media: first(ItemKey::OriginalMediaType),
            barcode: first(ItemKey::Barcode),
            catalog_number: first(ItemKey::CatalogNumber),
            release_country: first(ItemKey::ReleaseCountry),
            total_discs: total_of(&values_of(ItemKey::DiscTotal)),
            release_date: release_date(tag_type, &values_of),
            total_tracks: track_total(tag_type, &values_of),
        },
    }
}

/// A disc total: the part after the slash of "1/2", else the plain number.
fn total_of(values: &[String]) -> Option<u32> {
    let value = values.first()?;
    let total = match value.split_once('/') {
        Some((_, total)) => total,
        None => value.as_str(),
    };
    total.trim().parse().ok().filter(|total| *total > 0)
}

/// A track total as [`total_of`] reads it. APE has one `Track` item for
/// both the number and the total (see `ape_tag_from_items`), so there
/// only the part after the slash of "3/12" is a total; a plain "3" is the
/// track's number.
fn track_total(tag_type: TagType, values_of: &impl Fn(ItemKey) -> Vec<String>) -> Option<u32> {
    let values = values_of(ItemKey::TrackTotal);
    if tag_type == TagType::Ape && !values.first().is_some_and(|value| value.contains('/')) {
        return None;
    }
    total_of(&values)
}

fn join_all(values: &[String]) -> Option<String> {
    if values.is_empty() {
        None
    } else {
        Some(values.join("; "))
    }
}

fn artist_credits(
    names: &[String],
    artist_ids: &[String],
    sort_name: Option<String>,
) -> Vec<AudioArtistCredit> {
    names
        .iter()
        .enumerate()
        .map(|(position, name)| AudioArtistCredit {
            name: name.clone(),
            credited_name: Some(name.clone()),
            sort_name: if names.len() == 1 {
                sort_name.clone()
            } else {
                None
            },
            musicbrainz_artist_id: artist_ids.get(position).cloned(),
            join_phrase: String::new(),
        })
        .collect()
}

/// Leading integer of a `"5"` or `"5/12"` style field.
fn leading_int(values: &[String]) -> Option<u32> {
    values.first()?.split('/').next()?.trim().parse().ok()
}

fn year_of(values: &[String]) -> Option<i32> {
    values.first()?.trim().get(..4)?.parse().ok()
}

/// A file's release date, read the way Navidrome 0.62.0 reads it, so player
/// apps get the `releaseDate` they would get from Navidrome. First match:
///
/// 1. the release date item (ID3 `TDRL`, Vorbis `RELEASEDATE`, MP4
///    `----:com.apple.iTunes:RELEASEDATE`);
/// 2. in an MP4 tag, `©day`, which Navidrome takes as the release date;
/// 3. a Vorbis `YEAR`;
/// 4. the recording date (`DATE`, `TDRC`) when the file also has an
///    original date and the recording date is not earlier: taggers such as
///    Picard write the edition's release date there.
///
/// Verified against the Navidrome 0.62.0 recordings under
/// `tests/reference/compat/navidrome-0.62.0/` (an M4A's `©day` comes back
/// as the album's release date, a FLAC `DATE` or an ID3 `TDRC` alone does
/// not) and its source at tag v0.62.0 (github.com/navidrome/navidrome):
/// `resources/mappings.yaml` for the `releasedate` aliases, `mapDates` in
/// `model/metadata/map_mediafile.go` for how the release date is picked, and
/// `ToAlbum` in
/// `model/mediafile.go`, where an album keeps the value only when every
/// track has the same one. Item keys per lofty 0.25.4's table in
/// `tag/item.rs`: `©day` is `RecordingDate`, Vorbis `YEAR` is `Year`.
fn release_date(tag_type: TagType, values_of: &impl Fn(ItemKey) -> Vec<String>) -> Option<String> {
    let date_of = |key: ItemKey| values_of(key).first().and_then(|value| date_text(value));
    let only_in = |kind: TagType, key: ItemKey| (tag_type == kind).then(|| date_of(key)).flatten();
    date_of(ItemKey::ReleaseDate)
        .or_else(|| only_in(TagType::Mp4Ilst, ItemKey::RecordingDate))
        .or_else(|| only_in(TagType::VorbisComments, ItemKey::Year))
        .or_else(|| {
            let original = date_of(ItemKey::OriginalReleaseDate)?;
            date_of(ItemKey::RecordingDate).filter(|recorded| *recorded >= original)
        })
}

/// `YYYY`, `YYYY-MM` or `YYYY-MM-DD` from the start of a date tag, any time
/// part dropped. `None` when it does not start with a four-digit year.
fn date_text(value: &str) -> Option<String> {
    let digits = |part: &&str| part.bytes().all(|byte| byte.is_ascii_digit());
    let value = value.trim();
    let mut out = value.get(..4).filter(digits)?.to_owned();
    let mut rest = &value[4..];
    for _ in 0..2 {
        let Some(part) = rest
            .strip_prefix('-')
            .and_then(|after| after.get(..2))
            .filter(digits)
        else {
            break;
        };
        out.push('-');
        out.push_str(part);
        rest = &rest[3..];
    }
    Some(out)
}

/// ReplayGain float with v2's `dB`-suffix tolerance and peak clamping.
fn gain(values: &[String], positive: bool) -> Option<f64> {
    let text = values.first()?;
    let lowered = text.to_lowercase();
    let normalized = lowered.strip_suffix("db").unwrap_or(&lowered);
    let parsed: f64 = normalized.trim().parse().ok()?;
    if positive && parsed < 0.0 {
        None
    } else {
        Some(parsed)
    }
}

// ---------------------------------------------------------------------------
// APEv2 reads for AAC (lofty sees no tag there at all).
// ---------------------------------------------------------------------------

/// Parse the APEv2 tag lofty ignores and map it through lofty's own APE
/// key table, so the one mapping above serves AAC too. Unmapped keys and
/// non-text items are skipped; a missing tag reads as untagged.
fn read_ape_tag(at: TrackFile<'_>) -> Result<AudioTag, TagsError> {
    let mut bytes = Vec::new();
    at.open()
        .and_then(|mut file| file.read_to_end(&mut bytes))
        .map_err(|source| TagsError::Io {
            path: at.path().display().to_string(),
            source,
        })?;
    let Some(items) = parse_apev2(&bytes) else {
        return Ok(AudioTag::default());
    };
    Ok(ape_tag_from_items(&items))
}

fn ape_tag_from_items(items: &[(String, Vec<String>)]) -> AudioTag {
    let mut mapped: Vec<(ItemKey, String)> = Vec::new();
    for (key, values) in items {
        if let Some(item_key) = ItemKey::from_key(TagType::Ape, key) {
            for value in values {
                mapped.push((item_key, value.clone()));
            }
        }
    }
    // APE maps "Track"/"Disc" to both the number and the total keys; the
    // mapper reads the number side.
    audio_tag_from_values(TagType::Ape, |key| {
        mapped
            .iter()
            .filter(|(mapped_key, _)| *mapped_key == key)
            .map(|(_, value)| value.clone())
            .collect()
    })
}

/// Raw APEv2 items: `(key, values)` with NUL-separated multi-values split.
pub(super) fn parse_apev2(bytes: &[u8]) -> Option<Vec<(String, Vec<String>)>> {
    if bytes.len() < 32 || &bytes[bytes.len() - 32..bytes.len() - 24] != b"APETAGEX" {
        return None;
    }
    let footer = &bytes[bytes.len() - 32..];
    let tag_size = u32::from_le_bytes(footer[12..16].try_into().ok()?) as usize;
    let item_count = u32::from_le_bytes(footer[16..20].try_into().ok()?) as usize;
    if tag_size < 32 || tag_size > bytes.len() || item_count > 256 {
        return None;
    }
    // Items run from the tag start to the footer, skipping a header
    // when one is present. The header flag bit is unreliable in the
    // wild, so both offsets are tried and the parse must consume
    // exactly `item_count` items ending at the footer.
    let tag_start = bytes.len() - tag_size;
    let items_end = bytes.len() - 32;
    parse_ape_items(bytes, tag_start, items_end, item_count)
        .or_else(|| parse_ape_items(bytes, tag_start + 32, items_end, item_count))
}

fn parse_ape_items(
    bytes: &[u8],
    start: usize,
    items_end: usize,
    item_count: usize,
) -> Option<Vec<(String, Vec<String>)>> {
    let mut cursor = start;
    let mut items = Vec::new();
    for _ in 0..item_count {
        if cursor + 8 > items_end || cursor < start {
            return None;
        }
        let value_len = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().ok()?) as usize;
        let item_flags = u32::from_le_bytes(bytes[cursor + 4..cursor + 8].try_into().ok()?);
        cursor += 8;
        let key_end = bytes
            .get(cursor..items_end)?
            .iter()
            .position(|byte| *byte == 0)?;
        let key = std::str::from_utf8(&bytes[cursor..cursor + key_end]).ok()?;
        cursor += key_end + 1;
        if cursor + value_len > items_end {
            return None;
        }
        let raw = &bytes[cursor..cursor + value_len];
        cursor += value_len;
        // Only text items (type bits zero); binary and locator items
        // (cover art lives here) are out of the tag surface.
        if item_flags & 0x06 != 0 {
            continue;
        }
        let text = std::str::from_utf8(raw).ok()?;
        let values: Vec<String> = text
            .split('\0')
            .filter_map(|part| {
                let trimmed = part.trim();
                (!trimmed.is_empty()).then(|| trimmed.to_owned())
            })
            .collect();
        items.push((key.to_owned(), values));
    }
    if cursor != items_end {
        return None;
    }
    Some(items)
}

#[cfg(test)]
mod tests {
    use super::{
        FileType, Probe, ape_tag_from_items, audio_tag_from_values, id3v23_tag_bytes,
        is_read_failure,
    };
    use lofty::tag::{ItemKey, TagType};

    /// A file whose reads fail with EIO after `limit` bytes, as on a mount
    /// whose backend went down while stat and open still answer.
    struct FailsAfter {
        inner: std::io::Cursor<&'static [u8]>,
        limit: u64,
    }

    impl std::io::Read for FailsAfter {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let left = self.limit.saturating_sub(self.inner.position());
            if left == 0 {
                return Err(std::io::Error::from_raw_os_error(libc::EIO));
            }
            let room = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            self.inner.read(&mut buf[..room])
        }
    }

    impl std::io::Seek for FailsAfter {
        fn seek(&mut self, to: std::io::SeekFrom) -> std::io::Result<u64> {
            self.inner.seek(to)
        }
    }

    /// An I/O failure partway through a file is a failed read in every
    /// container, however deep lofty nests it, never a damaged file: the
    /// sweeps clear rows on a damaged file and keep them on a failed read.
    #[test]
    fn an_io_error_inside_a_tag_is_a_read_failure() {
        // Each failure lands inside the tag. Above it lofty and ogg_pager drop
        // a failed read while walking atoms and page headers, which
        // `read_through` catches when the file then does not parse.
        let files: [(FileType, &'static [u8], &[u8], u64); 3] = [
            (
                FileType::Mp4,
                include_bytes!("../../../tests/fixtures/library/management_full.m4a"),
                b"ilst",
                8,
            ),
            (
                FileType::Opus,
                include_bytes!("../../../tests/fixtures/library/management_full.opus"),
                b"OpusTags",
                8,
            ),
            (
                FileType::Mpeg,
                include_bytes!("../../../tests/fixtures/library/management_full.mp3"),
                b"ID3",
                64,
            ),
        ];
        for (file_type, bytes, marker, past) in files {
            let start = bytes
                .windows(marker.len())
                .position(|name| name == marker)
                .expect("the fixture has its tag");
            let reader = FailsAfter {
                inner: std::io::Cursor::new(bytes),
                limit: start as u64 + past,
            };
            let error = Probe::with_file_type(reader, file_type)
                .read()
                .err()
                .unwrap_or_else(|| panic!("{file_type:?} read past the failure"));
            assert!(is_read_failure(&error), "{file_type:?}: {error}");
        }
    }

    /// A failed read of the header of `udta`, which holds an M4A's tags,
    /// ends lofty's `moov` walk without an error and the file parses with
    /// no tag. That read is not believed: the file is read to its end and
    /// the failure surfaces.
    #[test]
    fn an_m4a_that_lost_its_tag_header_is_a_failed_read() {
        let bytes: &'static [u8] =
            include_bytes!("../../../tests/fixtures/library/management_full.m4a");
        let udta = bytes
            .windows(4)
            .position(|name| name == b"udta")
            .expect("the fixture has its udta");
        let mut reader = FailsAfter {
            inner: std::io::Cursor::new(bytes),
            limit: udta as u64 - 2,
        };
        let tagged = Probe::with_file_type(&mut reader, FileType::Mp4)
            .read()
            .expect("lofty drops the failed header read");
        assert!(lofty::file::TaggedFileExt::tags(&tagged).is_empty());
        let error =
            super::confirm_untagged_mp4(&tagged, &mut reader).expect_err("the failure surfaces");
        assert_eq!(error.raw_os_error(), Some(libc::EIO));
    }

    /// The tag `pairs` make for a `tag_type` file.
    fn tag_of(tag_type: TagType, pairs: &'static [(ItemKey, &'static str)]) -> super::AudioTag {
        audio_tag_from_values(tag_type, |key| {
            pairs
                .iter()
                .filter(|(item, _)| *item == key)
                .map(|(_, value)| (*value).to_owned())
                .collect()
        })
    }

    /// A track total is kept as an edition hint, read from its own key or
    /// from the "n/m" pair, and never from a track number alone. A file's
    /// tag revision hashes its tag's `Debug` text with the edition hints
    /// empty: reading the track total must not change that text, or every
    /// unchanged file would read as changed on the next scan.
    #[test]
    fn edition_tags_read_and_keep_their_text() {
        let vorbis = tag_of(
            TagType::VorbisComments,
            &[(ItemKey::TrackNumber, "3"), (ItemKey::TrackTotal, "12")],
        );
        assert_eq!(vorbis.edition.total_tracks, Some(12));
        let none = tag_of(TagType::VorbisComments, &[(ItemKey::TrackNumber, "3")]);
        assert_eq!(none.edition.total_tracks, None);
        let zero = tag_of(TagType::VorbisComments, &[(ItemKey::TrackTotal, "0")]);
        assert_eq!(zero.edition.total_tracks, None);

        let ape = |value: &str| ape_tag_from_items(&[("Track".to_owned(), vec![value.to_owned()])]);
        assert_eq!(ape("3/12").edition.total_tracks, Some(12));
        assert_eq!(ape("3/12").track_number, 3);
        assert_eq!(ape("3").edition.total_tracks, None);
        assert_eq!(ape("3").track_number, 3);

        let empty = format!("{:?}", super::EditionTags::default());
        assert_eq!(
            empty,
            "EditionTags { media: None, barcode: None, catalog_number: None, \
             release_country: None, total_discs: None }"
        );
    }

    /// Serves an ID3v2.3 header whose tag size is one byte over 16 MiB,
    /// then zeros, counting every byte it hands out.
    struct Oversized {
        served: usize,
    }

    impl std::io::Read for Oversized {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            const HEADER: &[u8] = b"ID3\x03\x00\x00\x08\x00\x00\x01";
            for byte in buf.iter_mut() {
                *byte = HEADER.get(self.served).copied().unwrap_or(0);
                self.served += 1;
            }
            Ok(buf.len())
        }
    }

    /// A tag declared larger than the cap gives no date, and its body is
    /// never read, so a hostile file cannot make a scan allocate it.
    #[test]
    fn oversized_id3v23_tag_is_not_read() {
        let mut reader = Oversized { served: 0 };
        assert!(id3v23_tag_bytes(&mut reader).unwrap().is_none());
        assert_eq!(reader.served, 10);
    }
}
