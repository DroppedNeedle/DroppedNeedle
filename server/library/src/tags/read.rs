//! Tag reads through lofty's generic items, mirroring v2's field mapping.
//!
//! Rule zero (see `save/mod.rs`) cuts both ways: native tags are for
//! writes, and generic items are for reads. The generic `Tag` lofty builds
//! on read is complete for every field v2 surfaces (multi-values arrive as
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

use std::path::Path;

use lofty::file::{AudioFile as _, TaggedFile, TaggedFileExt as _};
use lofty::tag::{ItemKey, Tag, TagType};

use super::{AudioFormat, TagsError, format_for_path};

/// One format-native artist value. No joined scalar is ever split.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AudioArtistCredit {
    pub name: String,
    pub credited_name: Option<String>,
    pub sort_name: Option<String>,
    pub musicbrainz_artist_id: Option<String>,
    pub join_phrase: String,
}

/// Tag metadata read from an audio file. Field-for-field the v2 `AudioTag`.
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
}

/// Written by hand so an absent release date prints nothing. A file's
/// `tag_revision` hashes `AudioTag`'s `Debug` text with these hints empty,
/// and that text stays what it was before the release date was read, so
/// an unchanged file keeps its revision.
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
        out.finish()
    }
}

/// Read tags plus technical info, like v2's `AudioTagger.read_tags`.
pub fn read_tags(path: &Path) -> Result<(AudioTag, super::AudioInfo), TagsError> {
    let format = format_for_path(path)?;
    let tag = read_tag_only(path, format)?;
    let info = super::probe(path)?;
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
}

/// Tags plus header properties for one file, the way the scan indexes it.
/// Reads only the tag blocks and stream headers the container parser
/// needs and never decodes audio. ADTS has no container header, so its
/// duration comes from counting frames, still without a decode.
pub fn read_scan_metadata(
    path: &Path,
    format: AudioFormat,
) -> Result<(AudioTag, HeaderInfo), TagsError> {
    if format == AudioFormat::Aac {
        let tag = read_ape_tag(path)?;
        let header = match super::probe(path) {
            Ok(info) => HeaderInfo {
                duration_seconds: Some(info.duration_seconds),
                bitrate_kbps: Some(info.bitrate).filter(|rate| *rate > 0),
                sample_rate: Some(info.sample_rate).filter(|rate| *rate > 0),
                channels: Some(info.channels).filter(|count| *count > 0),
                bit_depth: None,
            },
            Err(_) => HeaderInfo::default(),
        };
        return Ok((tag, header));
    }
    let tagged = lofty::read_from_path(path).map_err(|error| {
        // An I/O failure (a file vanishing mid-scan, a contended disk) is
        // worth a retry; anything else is a broken file.
        let io = std::error::Error::source(&error)
            .and_then(|source| source.downcast_ref::<std::io::Error>())
            .map(|source| std::io::Error::new(source.kind(), source.to_string()));
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
    let tag = audio_tag_from_file(path, &tagged, format);
    let properties = tagged.properties();
    let duration = properties.duration();
    let lossless = matches!(
        format,
        AudioFormat::Flac | AudioFormat::Wav | AudioFormat::M4a
    );
    let header = HeaderInfo {
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
    };
    Ok((tag, header))
}

/// Read the tag half only. The probe half lives in [`super::probe`].
pub fn read_tag_only(path: &Path, format: AudioFormat) -> Result<AudioTag, TagsError> {
    if format == AudioFormat::Aac {
        return read_ape_tag(path);
    }
    let tagged = lofty::read_from_path(path).map_err(|error| TagsError::TagRead {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    Ok(audio_tag_from_file(path, &tagged, format))
}

/// The file's preferred tag through the one mapping, with the recording
/// date lofty leaves out of a mixed ID3v2.3 tag. The file is read again
/// only for an MP3 whose ID3 tag gave no date.
fn audio_tag_from_file(path: &Path, tagged: &TaggedFile, format: AudioFormat) -> AudioTag {
    let Some(tag) = select_tag(tagged, format) else {
        return AudioTag::default();
    };
    let date = if format == AudioFormat::Mp3
        && tag.tag_type() == TagType::Id3v2
        && values_for(tag, ItemKey::RecordingDate).is_empty()
    {
        read_mixed_v23_date(path)
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
fn read_mixed_v23_date(path: &Path) -> Option<String> {
    let bytes = std::fs::File::open(path).and_then(|mut file| id3v23_tag_bytes(&mut file));
    match bytes {
        Ok(bytes) => bytes
            .and_then(|bytes| super::save::mixed_v23_date(&bytes))
            .map(|date| date.trim().to_owned())
            .filter(|date| !date.is_empty()),
        Err(error) => {
            tracing::debug!(path = %path.display(), %error, "ID3v2.3 date not read");
            None
        }
    }
}

/// The ID3v2 tag at the start of an MP3, header included, when it is
/// v2.3 and no larger than lofty's own allocation limit. `None` for any
/// other file; an oversized tag's body is never read.
fn id3v23_tag_bytes(file: &mut impl std::io::Read) -> std::io::Result<Option<Vec<u8>>> {
    use std::io::Read as _;
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
pub fn read_cover_art(path: &Path) -> Result<Option<Vec<u8>>, TagsError> {
    // WMA is unrecognized even here: v2 returned None by falling through,
    // v3 rejects the extension before any reader runs.
    format_for_path(path)?;
    let tagged = lofty::read_from_path(path).map_err(|error| TagsError::TagRead {
        path: path.display().to_string(),
        reason: error.to_string(),
    })?;
    for tag in tagged.tags() {
        if let Some(picture) = tag.pictures().first() {
            return Ok(Some(picture.data().to_vec()));
        }
    }
    Ok(None)
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
/// `model/metadata/map_mediafile.go` for step 4, and `ToAlbum` in
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
fn read_ape_tag(path: &Path) -> Result<AudioTag, TagsError> {
    let bytes = std::fs::read(path).map_err(|source| TagsError::Io {
        path: path.display().to_string(),
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
    use super::id3v23_tag_bytes;

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
