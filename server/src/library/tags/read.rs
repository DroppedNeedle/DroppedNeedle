//! Tag reads through lofty's generic items, mirroring v2's field mapping.
//!
//! The stage-1 rule zero cuts both ways: native tags are for writes, and
//! generic items are for reads. The generic `Tag` lofty builds on read is
//! complete for every field v2 surfaces (multi-values arrive as separate
//! items, UFID and TXXX spellings included), while the native round-trip
//! mangles known `TXXX` description case and drops follow-on values on
//! several text frames. So reads never touch native tags.
//!
//! Two documented divergences from v2: lofty translates numeric ID3 genre
//! references (`(12)` becomes the named genre) where mutagen returned the
//! raw text, and a v2.3 `TYER` now yields a year where mutagen's `TDRC`
//! lookup found none. Both only affect files v2 already handled oddly.
//!
//! AAC carries an APEv2 tag lofty does not read, so this slice parses that
//! tag itself (reads only; AAC stays read-only for writes).

use std::path::Path;

use lofty::file::TaggedFileExt as _;
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
}

/// Read tags plus technical info, like v2's `AudioTagger.read_tags`.
pub fn read_tags(path: &Path) -> Result<(AudioTag, super::AudioInfo), TagsError> {
    let format = format_for_path(path)?;
    let tag = read_tag_only(path, format)?;
    let info = super::probe(path)?;
    Ok((tag, info))
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
    let preferred = match format {
        AudioFormat::Mp3 => Some(TagType::Id3v2),
        AudioFormat::Flac | AudioFormat::Ogg | AudioFormat::Opus => Some(TagType::VorbisComments),
        AudioFormat::M4a => Some(TagType::Mp4Ilst),
        AudioFormat::Wav => Some(TagType::Id3v2),
        AudioFormat::Aac => None,
    };
    let tag = preferred
        .and_then(|tag_type| tagged.tag(tag_type))
        .or_else(|| tagged.first_tag());
    Ok(tag.map_or_else(AudioTag::default, audio_tag_from_items))
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
    for item in tag.get_strings(key) {
        for part in item.split('\0') {
            let trimmed = part.trim();
            if !trimmed.is_empty() && !out.contains(&trimmed.to_owned()) {
                out.push(trimmed.to_owned());
            }
        }
    }
    out
}

fn audio_tag_from_items(tag: &Tag) -> AudioTag {
    audio_tag_from_values(|key| values_for(tag, key))
}

fn audio_tag_from_values(values_of: impl Fn(ItemKey) -> Vec<String>) -> AudioTag {
    let first = |key: ItemKey| -> Option<String> { values_of(key).into_iter().next() };
    let genres = values_of(ItemKey::Genre);
    let artist_ids = values_of(ItemKey::MusicBrainzArtistId);
    let album_artist_ids = values_of(ItemKey::MusicBrainzReleaseArtistId);
    let mut artist_names = values_of(ItemKey::TrackArtists);
    if artist_names.is_empty() {
        artist_names = values_of(ItemKey::TrackArtist);
    }
    let mut album_artist_names = values_of(ItemKey::AlbumArtists);
    if album_artist_names.is_empty() {
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
        musicbrainz_release_group_id: first(ItemKey::MusicBrainzReleaseGroupId),
        musicbrainz_release_id: first(ItemKey::MusicBrainzReleaseId),
        musicbrainz_recording_id: first(ItemKey::MusicBrainzRecordingId),
        musicbrainz_release_track_id: first(ItemKey::MusicBrainzTrackId),
        musicbrainz_artist_id: first(ItemKey::MusicBrainzArtistId),
        musicbrainz_album_artist_id: first(ItemKey::MusicBrainzReleaseArtistId),
        acoustid_id: first(ItemKey::AcoustId),
    }
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
    let mut mapped: Vec<(ItemKey, String)> = Vec::new();
    for (key, values) in &items {
        if let Some(item_key) = ItemKey::from_key(TagType::Ape, key) {
            for value in values {
                mapped.push((item_key, value.clone()));
            }
        }
    }
    // APE maps "Track"/"Disc" to both the number and the total keys; the
    // mapper reads the number side.
    Ok(audio_tag_from_values(|key| {
        mapped
            .iter()
            .filter(|(mapped_key, _)| *mapped_key == key)
            .map(|(_, value)| value.clone())
            .collect()
    }))
}

/// Raw APEv2 items: `(key, values)` with NUL-separated multi-values split.
fn parse_apev2(bytes: &[u8]) -> Option<Vec<(String, Vec<String>)>> {
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
