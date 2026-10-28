//! Tag briefs: read, probe, fingerprint, save wrapper.
//!
//! Reads, probes, and fingerprints run against the committed real-audio
//! fixtures in `server/tests/fixtures/library/`. Every save test copies
//! its fixture into a scratch dir first; the committed files are never
//! written. Nothing here touches the network.

use droppedneedle_library::publish::TagTarget;
use droppedneedle_library::tags;

use crate::common::ScratchDir;
use std::path::{Path, PathBuf};

use lofty::file::TaggedFileExt as _;
use lofty::tag::TagType;
use tags::save::{Refusal, inspect_id3_bytes, mixed_v23_detail};
use tags::{
    AudioArtistCredit, AudioFormat, AudioInfo, AudioTag, Fingerprint, SaveReport, TagEdit,
    TagField, TagsError, format_for_path,
};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn fixture(name: &str) -> PathBuf {
    Path::new("tests/fixtures/library").join(name)
}

/// Copy a fixture into a scratch dir removed when the guard drops. Save
/// tests only ever write to these copies.
fn temp_copy(name: &str, test: &str) -> (ScratchDir, PathBuf) {
    let dir = ScratchDir::new(&format!("tags-{test}"));
    let dest = dir.join(name);
    std::fs::copy(fixture(name), &dest).unwrap();
    (dir, dest)
}

fn id3_work(path: &Path) -> Option<String> {
    // WORK is a known description, so the generic tag carries it while
    // the native round-trip would emit the invalid WORK frame (#732).
    let tagged = lofty::read_from_path(path).unwrap();
    tagged
        .tag(TagType::Id3v2)
        .and_then(|tag| tag.get_string(lofty::tag::ItemKey::Work))
        .map(str::to_owned)
}

fn id3_user_text(path: &Path, description: &str) -> Vec<String> {
    let tagged = lofty::read_from_path(path).unwrap();
    let Some(tag) = tagged.tag(TagType::Id3v2).cloned() else {
        return Vec::new();
    };
    let native = lofty::id3::v2::Id3v2Tag::from(tag);
    native
        .iter()
        .filter_map(|frame| match frame {
            lofty::id3::v2::Frame::UserText(text)
                if text.description.eq_ignore_ascii_case(description) =>
            {
                Some(text.content.to_string())
            }
            _ => None,
        })
        .collect()
}

pub(crate) fn vorbis_values(path: &Path, key: &str) -> Vec<String> {
    // Raw pairs, not lofty's view: the generic round-trip drops unknown
    // fields, so only the bytes tell the truth here.
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[..4], b"fLaC");
    let mut offset = 4;
    let payload = loop {
        let header = bytes[offset];
        let len = u32::from_be_bytes([0, bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
            as usize;
        if header & 0x7F == 4 {
            break &bytes[offset + 4..offset + 4 + len];
        }
        offset += 4 + len;
        assert_eq!(header & 0x80, 0, "comment block missing");
    };
    let mut cursor = 0;
    let vendor_len = u32::from_le_bytes(payload[cursor..cursor + 4].try_into().unwrap()) as usize;
    cursor += 4 + vendor_len;
    let count = u32::from_le_bytes(payload[cursor..cursor + 4].try_into().unwrap()) as usize;
    cursor += 4;
    let mut out = Vec::new();
    for _ in 0..count {
        let len = u32::from_le_bytes(payload[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4;
        let pair = std::str::from_utf8(&payload[cursor..cursor + len]).unwrap();
        cursor += len;
        if let Some((pair_key, value)) = pair.split_once('=')
            && pair_key.eq_ignore_ascii_case(key)
        {
            out.push(value.to_owned());
        }
    }
    out
}

fn vorbis_vendor(path: &Path) -> String {
    let tagged = lofty::read_from_path(path).unwrap();
    let tag = tagged.tag(TagType::VorbisComments).cloned().unwrap();
    lofty::ogg::tag::VorbisComments::from(tag)
        .vendor()
        .to_owned()
}

/// MP3 audio tail: bytes from the first MPEG frame sync after the ID3 tag.
pub(crate) fn mp3_audio_tail(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    if bytes.len() > 10 && &bytes[..3] == b"ID3" {
        let size = bytes[6..10]
            .iter()
            .fold(0usize, |acc, byte| (acc << 7) | usize::from(byte & 0x7F));
        start = 10 + size;
    }
    let mut offset = start;
    while offset + 1 < bytes.len() {
        if bytes[offset] == 0xFF && bytes[offset + 1] & 0xE0 == 0xE0 {
            return &bytes[offset..];
        }
        offset += 1;
    }
    &bytes[start..]
}

/// FLAC audio tail: bytes after the last metadata block.
fn flac_audio_tail(bytes: &[u8]) -> &[u8] {
    assert_eq!(&bytes[..4], b"fLaC");
    let mut offset = 4;
    loop {
        let header = bytes[offset];
        let len = u32::from_be_bytes([0, bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
            as usize;
        offset += 4 + len;
        if header & 0x80 != 0 {
            return &bytes[offset..];
        }
    }
}

/// M4A audio payload: the bytes of the `mdat` box.
fn m4a_mdat(bytes: &[u8]) -> &[u8] {
    let mut offset = 0;
    while offset + 8 <= bytes.len() {
        let mut size = u32::from_be_bytes([
            bytes[offset],
            bytes[offset + 1],
            bytes[offset + 2],
            bytes[offset + 3],
        ]) as usize;
        let kind = &bytes[offset + 4..offset + 8];
        let mut header = 8;
        if size == 1 {
            size = u64::from_be_bytes(bytes[offset + 8..offset + 16].try_into().unwrap()) as usize;
            header = 16;
        }
        if kind == b"mdat" {
            return &bytes[offset + header..offset + size];
        }
        if size == 0 {
            return &bytes[offset + header..];
        }
        offset += size;
    }
    panic!("no mdat box found");
}

/// Splice raw frames into the ID3 tag at the start of `path` (v2.4 tag
/// assumed, like the management fixture). Used to plant unknown frames
/// without going through lofty's save, which cannot write them while the
/// WORK bug is open.
pub(crate) fn id3_inject_frames(path: &Path, frames: &[(&str, &[u8])]) {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[..3], b"ID3");
    assert_eq!(bytes[3], 4);
    let size = bytes[6..10]
        .iter()
        .fold(0usize, |acc, byte| (acc << 7) | usize::from(byte & 0x7F));
    let mut injected = Vec::new();
    for (id, payload) in frames {
        injected.extend_from_slice(id.as_bytes());
        let len = payload.len();
        injected.extend_from_slice(&[
            ((len >> 21) & 0x7F) as u8,
            ((len >> 14) & 0x7F) as u8,
            ((len >> 7) & 0x7F) as u8,
            (len & 0x7F) as u8,
        ]);
        injected.extend_from_slice(&[0, 0]);
        injected.extend_from_slice(payload);
    }
    let new_size = size + injected.len();
    let mut out = bytes[..6].to_vec();
    out.extend_from_slice(&[
        ((new_size >> 21) & 0x7F) as u8,
        ((new_size >> 14) & 0x7F) as u8,
        ((new_size >> 7) & 0x7F) as u8,
        (new_size & 0x7F) as u8,
    ]);
    out.extend_from_slice(&injected);
    out.extend_from_slice(&bytes[10..]);
    std::fs::write(path, out).unwrap();
}

/// Inject raw pairs into a FLAC's Vorbis comment block.
pub(crate) fn flac_inject_pairs(path: &Path, extra: &[(&str, &str)]) {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[..4], b"fLaC");
    let mut blocks: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut offset = 4;
    loop {
        let header = bytes[offset];
        let len = u32::from_be_bytes([0, bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
            as usize;
        blocks.push((header & 0x7F, bytes[offset + 4..offset + 4 + len].to_vec()));
        offset += 4 + len;
        if header & 0x80 != 0 {
            break;
        }
    }
    let audio = bytes[offset..].to_vec();
    let comments = blocks.iter_mut().find(|(kind, _)| *kind == 4).unwrap();
    let mut pairs = Vec::new();
    let mut cursor = 0;
    let payload = &comments.1;
    let vendor_len = u32::from_le_bytes(payload[cursor..cursor + 4].try_into().unwrap()) as usize;
    cursor += 4;
    let vendor = payload[cursor..cursor + vendor_len].to_vec();
    cursor += vendor_len;
    let count = u32::from_le_bytes(payload[cursor..cursor + 4].try_into().unwrap()) as usize;
    cursor += 4;
    for _ in 0..count {
        let len = u32::from_le_bytes(payload[cursor..cursor + 4].try_into().unwrap()) as usize;
        cursor += 4;
        pairs.push(payload[cursor..cursor + len].to_vec());
        cursor += len;
    }
    for (key, value) in extra {
        pairs.push(format!("{key}={value}").into_bytes());
    }
    let mut rebuilt = Vec::new();
    rebuilt.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    rebuilt.extend_from_slice(&vendor);
    rebuilt.extend_from_slice(&(pairs.len() as u32).to_le_bytes());
    for pair in &pairs {
        rebuilt.extend_from_slice(&(pair.len() as u32).to_le_bytes());
        rebuilt.extend_from_slice(pair);
    }
    comments.1 = rebuilt;
    let mut out = b"fLaC".to_vec();
    for (index, (kind, payload)) in blocks.iter().enumerate() {
        let last = index + 1 == blocks.len();
        out.push(kind | if last { 0x80 } else { 0 });
        let len = payload.len() as u32;
        out.extend_from_slice(&len.to_be_bytes()[1..]);
        out.extend_from_slice(payload);
    }
    out.extend_from_slice(&audio);
    std::fs::write(path, out).unwrap();
}

/// Drop the Vorbis comment block from a FLAC entirely (audio and picture
/// blocks stay), leaving a genuinely untagged file.
fn flac_strip_comments(path: &Path) {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[..4], b"fLaC");
    let mut blocks: Vec<(u8, Vec<u8>)> = Vec::new();
    let mut offset = 4;
    loop {
        let header = bytes[offset];
        let len = u32::from_be_bytes([0, bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
            as usize;
        if header & 0x7F != 4 {
            blocks.push((header & 0x7F, bytes[offset + 4..offset + 4 + len].to_vec()));
        }
        offset += 4 + len;
        if header & 0x80 != 0 {
            break;
        }
    }
    let audio = bytes[offset..].to_vec();
    let mut out = b"fLaC".to_vec();
    for (index, (kind, payload)) in blocks.iter().enumerate() {
        let last = index + 1 == blocks.len();
        out.push(kind | if last { 0x80 } else { 0 });
        let len = payload.len() as u32;
        out.extend_from_slice(&len.to_be_bytes()[1..]);
        out.extend_from_slice(payload);
    }
    out.extend_from_slice(&audio);
    std::fs::write(path, out).unwrap();
}

// ---------------------------------------------------------------------------
// Extension routing: WMA unrecognized everywhere
// ---------------------------------------------------------------------------

#[test]
fn routing_recognizes_v2_set_minus_wma() {
    for (name, format) in [
        ("a.flac", AudioFormat::Flac),
        ("a.mp3", AudioFormat::Mp3),
        ("a.ogg", AudioFormat::Ogg),
        ("a.opus", AudioFormat::Opus),
        ("a.m4a", AudioFormat::M4a),
        ("a.aac", AudioFormat::Aac),
        ("a.wav", AudioFormat::Wav),
    ] {
        assert_eq!(format_for_path(Path::new(name)).unwrap(), format);
    }
    assert!(format_for_path(Path::new("a.WMA")).is_err());
    assert!(format_for_path(Path::new("a.wma")).is_err());
    assert!(format_for_path(Path::new("a.txt")).is_err());
}

#[test]
fn wma_is_unrecognized_on_every_entry() {
    let path = fixture("management_full.wma");
    let error = tags::read_tags(&path).unwrap_err();
    assert!(error.is_unrecognized());
    assert!(matches!(
        tags::probe(&path),
        Err(TagsError::UnrecognizedExtension { .. })
    ));
    assert!(matches!(
        tags::generate_fingerprint(&path),
        Err(TagsError::UnrecognizedExtension { .. })
    ));
    assert!(matches!(
        tags::read_cover_art(&path),
        Err(TagsError::UnrecognizedExtension { .. })
    ));
    assert!(matches!(
        tags::save_tags(&path, &[TagEdit::set_title("x")]),
        Err(TagsError::UnrecognizedExtension { .. })
    ));
}

// ---------------------------------------------------------------------------
// Tag reads
// ---------------------------------------------------------------------------

#[test]
fn read_flac_full() {
    let (tag, _): (AudioTag, _) = tags::read_tags(&fixture("flac_full_01.flac")).unwrap();
    assert_eq!(tag.title, "Airbag");
    assert_eq!(tag.artist, "Radiohead");
    assert_eq!(tag.album, "OK Computer");
    assert_eq!(tag.album_artist.as_deref(), Some("Radiohead"));
    assert_eq!(tag.track_number, 1);
    assert_eq!(tag.disc_number, 1);
    assert_eq!(tag.year, Some(1997));
    assert_eq!(tag.genre.as_deref(), Some("Alternative Rock"));
    assert_eq!(
        tag.musicbrainz_release_group_id.as_deref(),
        Some("b1392450-e666-3926-a536-22c65f834433")
    );
    assert_eq!(
        tag.musicbrainz_release_id.as_deref(),
        Some("0da3b3e3-1111-4444-8888-aaaaaaaaaaaa")
    );
    assert_eq!(
        tag.musicbrainz_recording_id.as_deref(),
        Some("rec-airbag-0001")
    );
    assert_eq!(
        tag.musicbrainz_artist_id.as_deref(),
        Some("a74b1b7f-71a5-4011-9441-d0b5e4122711")
    );
    assert_eq!(tag.acoustid_id.as_deref(), Some("ac-airbag-0001"));
    assert!(!tag.compilation);
    // A DATE with no original date is not a release date.
    assert_eq!(tag.edition.release_date, None);
}

#[test]
fn read_flac_edge_cases() {
    let (cjk, _) = tags::read_tags(&fixture("flac_cjk_01.flac")).unwrap();
    assert_eq!(cjk.title, "桃源へ");
    assert_eq!(cjk.artist, "ユキ");
    assert_eq!(cjk.album, "望厚");

    let (compilation, _) = tags::read_tags(&fixture("flac_compilation_01.flac")).unwrap();
    assert!(compilation.compilation);
    assert_eq!(compilation.album_artist.as_deref(), Some("Various Artists"));

    let (only_release, _) = tags::read_tags(&fixture("flac_only_release_mbid.flac")).unwrap();
    assert_eq!(
        only_release.musicbrainz_release_id.as_deref(),
        Some("rel-only-0001")
    );
    assert_eq!(only_release.musicbrainz_release_group_id, None);

    let (bare, _) = tags::read_tags(&fixture("flac_no_tags.flac")).unwrap();
    assert_eq!(bare.title, "");
    assert_eq!(bare.artist, "");
    assert_eq!(bare.album, "");
    assert_eq!(bare.track_number, 0);
    assert_eq!(bare.disc_number, 1);
}

#[test]
fn read_mp3_full() {
    let (tag, _) = tags::read_tags(&fixture("mp3_full_01.mp3")).unwrap();
    assert_eq!(tag.title, "One");
    assert_eq!(tag.artist, "U2");
    assert_eq!(tag.album, "Achtung Baby");
    assert_eq!(tag.track_number, 3);
    assert_eq!(tag.year, Some(1991));
    assert_eq!(tag.edition.release_date, None);
    assert_eq!(tag.genre.as_deref(), Some("Rock"));
    assert_eq!(
        tag.musicbrainz_recording_id.as_deref(),
        Some("rec-one-0003")
    );
    assert_eq!(
        tag.musicbrainz_release_group_id.as_deref(),
        Some("rg-achtung-0001")
    );
}

#[test]
fn read_m4a_full() {
    let (tag, _) = tags::read_tags(&fixture("m4a_full_01.m4a")).unwrap();
    assert_eq!(tag.title, "Teardrop");
    assert_eq!(tag.artist, "Massive Attack");
    assert_eq!(tag.album, "Mezzanine");
    assert_eq!(tag.track_number, 4);
    assert_eq!(tag.year, Some(1998));
    // An MP4 ©day is the release date.
    assert_eq!(tag.edition.release_date.as_deref(), Some("1998"));
    assert_eq!(
        tag.musicbrainz_recording_id.as_deref(),
        Some("rec-teardrop-0004")
    );
    assert_eq!(
        tag.musicbrainz_release_group_id.as_deref(),
        Some("rg-mezzanine-0001")
    );
}

#[test]
fn read_management_mp3() {
    let (tag, _) = tags::read_tags(&fixture("management_full.mp3")).unwrap();
    assert_eq!(tag.title, "Management Track");
    assert_eq!(tag.artist, "Alpha feat. Beta");
    assert_eq!(tag.album, "Management Album");
    assert_eq!(tag.album_artist.as_deref(), Some("Alpha"));
    assert_eq!(tag.track_number, 2);
    assert_eq!(tag.disc_number, 1);
    assert_eq!(tag.year, Some(2024));
    assert!(tag.compilation);
    assert_eq!(tag.genres, vec!["Electronic", "Ambient"]);
    assert_eq!(tag.genre.as_deref(), Some("Electronic; Ambient"));
    assert_eq!(tag.title_sort.as_deref(), Some("Management Track, The"));
    assert_eq!(tag.artist_sort.as_deref(), Some("Alpha, The"));
    assert_eq!(tag.disc_subtitle.as_deref(), Some("Main Programme"));
    assert_eq!(tag.original_release_date.as_deref(), Some("2020"));
    // Picard's layout: the date beside an earlier original date.
    assert_eq!(tag.edition.release_date.as_deref(), Some("2024-03-02"));
    assert_eq!(tag.artists.len(), 2);
    let credit: &AudioArtistCredit = &tag.artists[0];
    assert_eq!(credit.name, "Alpha");
    assert_eq!(
        credit.musicbrainz_artist_id.as_deref(),
        Some("10000000-0000-4000-8000-000000000005")
    );
    assert_eq!(
        tag.musicbrainz_recording_id.as_deref(),
        Some("10000000-0000-4000-8000-000000000003")
    );
    assert_eq!(
        tag.musicbrainz_release_track_id.as_deref(),
        Some("10000000-0000-4000-8000-000000000004")
    );
    assert_eq!(
        id3_work(&fixture("management_full.mp3")).as_deref(),
        Some("Example Work")
    );
}

#[test]
fn read_management_flac() {
    let (tag, _) = tags::read_tags(&fixture("management_full.flac")).unwrap();
    assert_eq!(tag.title, "Management Track");
    assert_eq!(tag.artists.len(), 2);
    assert_eq!(tag.genres, vec!["Electronic", "Ambient"]);
    assert_eq!(tag.track_number, 2);
    assert_eq!(tag.year, Some(2024));
    assert!(tag.compilation);
    assert_eq!(tag.release_type.as_deref(), Some("Album"));
    assert_eq!(tag.edition.release_date.as_deref(), Some("2024-03-02"));
    assert_eq!(
        tag.musicbrainz_recording_id.as_deref(),
        Some("10000000-0000-4000-8000-000000000003")
    );
}

#[test]
fn read_management_ogg_opus() {
    for name in ["management_full.ogg", "management_full.opus"] {
        let (tag, _) = tags::read_tags(&fixture(name)).unwrap();
        assert_eq!(tag.title, "Management Track", "{name}");
        assert_eq!(tag.artists.len(), 2, "{name}");
        assert_eq!(tag.genres, vec!["Electronic", "Ambient"], "{name}");
        assert_eq!(
            tag.edition.release_date.as_deref(),
            Some("2024-03-02"),
            "{name}"
        );
        assert_eq!(
            tag.musicbrainz_recording_id.as_deref(),
            Some("10000000-0000-4000-8000-000000000003"),
            "{name}"
        );
    }
}

#[test]
fn read_management_m4a() {
    let (tag, _) = tags::read_tags(&fixture("management_full.m4a")).unwrap();
    assert_eq!(tag.title, "Management Track");
    assert_eq!(tag.artist, "Alpha feat. Beta");
    assert_eq!(tag.track_number, 2);
    assert_eq!(tag.disc_number, 1);
    assert_eq!(tag.year, Some(2024));
    assert!(tag.compilation);
    assert_eq!(tag.artists.len(), 2);
    assert_eq!(tag.genres, vec!["Electronic", "Ambient"]);
    assert_eq!(tag.artist_sort.as_deref(), Some("Alpha, The"));
    assert_eq!(tag.disc_subtitle.as_deref(), Some("Main Programme"));
    assert_eq!(tag.edition.release_date.as_deref(), Some("2024-03-02"));
    assert_eq!(
        tag.musicbrainz_recording_id.as_deref(),
        Some("10000000-0000-4000-8000-000000000003")
    );
    assert_eq!(
        tag.musicbrainz_release_track_id.as_deref(),
        Some("10000000-0000-4000-8000-000000000004")
    );
}

/// The reader says whether a file's artist names came from the list keys
/// (ARTISTS, ALBUMARTISTS), the only names that line up with the
/// MusicBrainz artist ids one for one. An ID3v2.3 file holds a list as one
/// joined value, so two artists there do not line up.
#[test]
fn artist_lists_say_where_their_names_came_from() {
    let uuid = |id: &String| id.len() == 36 && uuid::Uuid::parse_str(id).is_ok();
    for name in [
        "management_full.flac",
        "management_full.mp3",
        "management_full.m4a",
    ] {
        let (tag, _) = tags::read_tags(&fixture(name)).unwrap();
        assert!(tag.artists_listed, "{name}");
        assert_eq!(tag.artists.len(), 2, "{name}");
        assert_eq!(tag.musicbrainz_artist_ids.len(), 2, "{name}");
        assert!(tag.musicbrainz_artist_ids.iter().all(uuid), "{name}");
        // The fixtures carry ALBUMARTIST but no ALBUMARTISTS list.
        assert!(!tag.album_artists_listed, "{name}");
    }

    let (plain, _) = tags::read_tags(&fixture("flac_full_01.flac")).unwrap();
    assert!(!plain.artists_listed);
    assert!(!plain.album_artists_listed);

    let (v23, _) = tags::read_tags(&fixture("management_full_v23.mp3")).unwrap();
    let aligned = v23.artists_listed
        && v23.artists.len() == v23.musicbrainz_artist_ids.len()
        && v23.musicbrainz_artist_ids.iter().all(uuid);
    assert!(
        !aligned,
        "{:?} / {:?}",
        v23.artists, v23.musicbrainz_artist_ids
    );
}

#[test]
fn read_management_wav_pair() {
    let (id3, _) = tags::read_tags(&fixture("management_full.wav")).unwrap();
    assert_eq!(id3.title, "Management Track");
    assert_eq!(id3.track_number, 2);
    assert_eq!(
        id3.musicbrainz_recording_id.as_deref(),
        Some("10000000-0000-4000-8000-000000000003")
    );

    let (riff, _) = tags::read_tags(&fixture("management_full_riff.wav")).unwrap();
    assert_eq!(riff.title, "RIFF Management Track");
    assert_eq!(riff.artist, "Alpha feat. Beta");
    assert_eq!(riff.album, "RIFF Management Album");
    assert_eq!(riff.track_number, 2);
    assert_eq!(riff.year, Some(2024));
    assert_eq!(riff.genre.as_deref(), Some("Electronic"));
}

#[test]
fn read_management_aac_best_effort() {
    // AAC tag reads are best effort: core fields only.
    let (tag, _) = tags::read_tags(&fixture("management_full.aac")).unwrap();
    assert_eq!(tag.title, "Management Track");
    assert_eq!(tag.artist, "Alpha feat. Beta");
    assert_eq!(tag.album, "Management Album");
    assert_eq!(tag.track_number, 2);
    assert_eq!(tag.disc_number, 1);
}

#[test]
fn read_cover_art() {
    for name in [
        "management_full.mp3",
        "management_full.flac",
        "management_full.m4a",
        "management_full.ogg",
        "management_full.opus",
    ] {
        let art = tags::read_cover_art(&fixture(name)).unwrap();
        assert!(art.is_some(), "{name} should carry cover art");
        assert!(!art.unwrap().is_empty(), "{name}");
    }
    assert_eq!(
        tags::read_cover_art(&fixture("flac_no_tags.flac")).unwrap(),
        None
    );
}

// ---------------------------------------------------------------------------
// Probe
// ---------------------------------------------------------------------------

/// Mutagen-oracle durations for the fixtures (seconds).
const ORACLE_DURATIONS: &[(&str, f64)] = &[
    ("management_full.flac", 0.30),
    ("management_full.mp3", 0.3396),
    ("management_full.ogg", 0.30),
    ("management_full.opus", 0.30),
    ("management_full.m4a", 0.3232),
    ("management_full.wav", 0.30),
    ("flac_full_01.flac", 0.30),
    ("mp3_full_01.mp3", 0.3396),
    ("m4a_full_01.m4a", 0.3232),
];

#[test]
fn probe_matches_oracle_durations() {
    for (name, oracle) in ORACLE_DURATIONS {
        let info = tags::probe(&fixture(name)).unwrap();
        let drift = (info.duration_seconds - oracle).abs() / oracle;
        assert!(
            drift < 0.25,
            "{name}: {}s vs oracle {oracle}s",
            info.duration_seconds
        );
        assert_eq!(info.channels, 2, "{name}");
    }
}

#[test]
fn probe_rates_and_depths() {
    let flac: AudioInfo = tags::probe(&fixture("management_full.flac")).unwrap();
    assert_eq!(flac.sample_rate, 44100);
    assert_eq!(flac.bit_depth, Some(16));
    assert_eq!(flac.file_format, "flac");

    let opus = tags::probe(&fixture("management_full.opus")).unwrap();
    assert_eq!(opus.sample_rate, 48000);
    assert_eq!(opus.bit_depth, None);

    let wav = tags::probe(&fixture("management_full.wav")).unwrap();
    assert_eq!(wav.bit_depth, Some(16));

    // Lossy containers suppress bit depth, including AAC-backed M4A.
    for name in [
        "management_full.mp3",
        "management_full.m4a",
        "management_full.ogg",
        "management_full.aac",
    ] {
        let info = tags::probe(&fixture(name)).unwrap();
        assert_eq!(info.bit_depth, None, "{name}");
        assert!(info.bitrate > 0, "{name}");
    }
    let size = std::fs::metadata(fixture("management_full.flac"))
        .unwrap()
        .len();
    assert_eq!(flac.file_size_bytes, size);
}

#[test]
fn probe_adts_uses_demux_count() {
    // Mutagen cannot even parse this file (length 0); the demux-count
    // fallback must still land in a sane band.
    let info = tags::probe(&fixture("management_full.aac")).unwrap();
    assert_eq!(info.sample_rate, 44100);
    assert_eq!(info.channels, 2);
    assert!(
        (0.2..0.5).contains(&info.duration_seconds),
        "ADTS duration {}",
        info.duration_seconds
    );
}

// ---------------------------------------------------------------------------
// Fingerprint
// ---------------------------------------------------------------------------

#[test]
fn fingerprint_is_deterministic_and_well_formed() {
    for name in [
        "management_full.flac",
        "management_full.mp3",
        "management_full.m4a",
        "management_full.ogg",
        "management_full.opus",
        "management_full.wav",
        "management_full.aac",
    ] {
        let path = fixture(name);
        let first: Fingerprint = tags::generate_fingerprint(&path).unwrap();
        let second = tags::generate_fingerprint(&path).unwrap();
        assert_eq!(first, second, "{name} must be deterministic");
        assert!(!first.fingerprint.is_empty(), "{name}");
        // URL-safe base64 without padding: never `+`, `/`, or `=`.
        assert!(
            first.fingerprint.bytes().all(|byte| matches!(
                byte,
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_'
            )),
            "{name}: {}",
            first.fingerprint
        );
        // test2 preset: compressed form opens with algorithm byte 1.
        assert!(
            first.fingerprint.starts_with("AQ"),
            "{name}: {}",
            first.fingerprint
        );
        assert!(!first.partial_decode, "{name}");
        let info = tags::probe(&path).unwrap();
        assert_eq!(
            first.duration_seconds, info.duration_seconds as u32,
            "{name}"
        );
    }
}

// ---------------------------------------------------------------------------
// Save wrapper: preservation round-trips
// ---------------------------------------------------------------------------

#[test]
fn save_mp3_preserves_everything_but_the_title() {
    let (_scratch, path) = temp_copy("management_full.mp3", "mp3-roundtrip");
    let before = std::fs::read(&path).unwrap();
    let audio_before = mp3_audio_tail(&before).to_vec();

    // Plant unknown frames by splicing
    // raw frames (lofty's own save cannot plant them while #732 is open).
    let mut txxx = vec![3u8];
    txxx.extend_from_slice(b"ZZZ_UNHEARD_OF\0mystery");
    let mut wxxx = vec![3u8];
    wxxx.extend_from_slice(b"ZZZ_LINK\0https://example.invalid/");
    let mut priv_frame = b"com.example\0".to_vec();
    priv_frame.extend_from_slice(b"opaque");
    id3_inject_frames(
        &path,
        &[("TXXX", &txxx), ("PRIV", &priv_frame), ("WXXX", &wxxx)],
    );

    let report: SaveReport = tags::save_tags(&path, &[TagEdit::set_title("WRAP TITLE")]).unwrap();
    assert_eq!(report.id3_version.as_deref(), Some("2.4"));

    let (tag, _) = tags::read_tags(&path).unwrap();
    assert_eq!(tag.title, "WRAP TITLE");
    assert_eq!(tag.artist, "Alpha feat. Beta");
    assert_eq!(tag.artists.len(), 2);
    assert_eq!(tag.genres, vec!["Electronic", "Ambient"]);
    assert_eq!(
        tag.musicbrainz_recording_id.as_deref(),
        Some("10000000-0000-4000-8000-000000000003")
    );
    // Sentinels: WORK, CUSTOM_KEEP, unknowns, multi-values, desc case.
    assert_eq!(id3_work(&path).as_deref(), Some("Example Work"));
    assert_eq!(
        id3_user_text(&path, "CUSTOM_KEEP"),
        vec!["opaque local value".to_owned()]
    );
    assert_eq!(
        id3_user_text(&path, "ZZZ_UNHEARD_OF"),
        vec!["mystery".to_owned()]
    );
    // Multi-values and description case on the wire: lofty's native
    // view renames known descriptions and truncates several text
    // frames, so only the bytes tell the truth here.
    let saved = std::fs::read(&path).unwrap();
    for needle in [
        b"Artists\0Alpha\0Beta".as_slice(),
        b"Alpha, The\0Beta, The".as_slice(),
        b"WORK\0Example Work".as_slice(),
        b"CUSTOM_KEEP\0opaque local value".as_slice(),
    ] {
        assert!(
            saved.windows(needle.len()).any(|window| window == needle),
            "missing {needle:?}"
        );
    }
    let report_bytes = std::fs::read(&path).unwrap();
    let byte_report = inspect_id3_bytes(&report_bytes).unwrap();
    assert!(byte_report.frame_ids.contains(&"PRIV".to_owned()));
    assert!(byte_report.frame_ids.contains(&"WXXX".to_owned()));
    // Cover and audio untouched.
    let art_before = tags::read_cover_art(&fixture("management_full.mp3")).unwrap();
    assert_eq!(tags::read_cover_art(&path).unwrap(), art_before);
    assert_eq!(mp3_audio_tail(&std::fs::read(&path).unwrap()), audio_before);
}

#[test]
fn save_flac_preserves_vendor_spelling_and_unknowns() {
    let (_scratch, path) = temp_copy("management_full.flac", "flac-roundtrip");
    let audio_before = flac_audio_tail(&std::fs::read(&path).unwrap()).to_vec();
    let vendor_before = vorbis_vendor(&path);

    // Plant an unknown field by splicing the raw pair
    // (a lofty save would drop the unknowns it cannot see).
    flac_inject_pairs(&path, &[("ZZZ_UNHEARD_OF_FIELD", "mystery")]);

    tags::save_tags(&path, &[TagEdit::set_title("WRAP TITLE")]).unwrap();

    let (tag, _) = tags::read_tags(&path).unwrap();
    assert_eq!(tag.title, "WRAP TITLE");
    assert_eq!(tag.artists.len(), 2);
    assert_eq!(
        vorbis_values(&path, "CUSTOM_KEEP"),
        vec!["opaque local value"]
    );
    assert_eq!(
        vorbis_values(&path, "ZZZ_UNHEARD_OF_FIELD"),
        vec!["mystery"]
    );
    // TOTALTRACKS/TOTALDISCS spelling, not lofty's TRACKTOTAL/DISCTOTAL.
    assert_eq!(vorbis_values(&path, "TOTALTRACKS"), vec!["9"]);
    assert_eq!(vorbis_values(&path, "TOTALDISCS"), vec!["2"]);
    assert!(vorbis_values(&path, "TRACKTOTAL").is_empty());
    assert_eq!(vorbis_vendor(&path), vendor_before);
    assert_eq!(
        flac_audio_tail(&std::fs::read(&path).unwrap()),
        audio_before
    );
}

#[test]
fn save_ogg_opus_m4a_round_trip() {
    for name in [
        "management_full.ogg",
        "management_full.opus",
        "management_full.m4a",
    ] {
        let (_scratch, path) = temp_copy(name, "roundtrip");
        let info_before = tags::probe(&path).unwrap();
        tags::save_tags(&path, &[TagEdit::set_title("WRAP TITLE")]).unwrap();
        let (tag, _) = tags::read_tags(&path).unwrap();
        assert_eq!(tag.title, "WRAP TITLE", "{name}");
        assert_eq!(tag.artists.len(), 2, "{name}");
        // CUSTOM_KEEP bytes still in the file.
        let bytes = std::fs::read(&path).unwrap();
        let needle = b"opaque local value";
        assert!(
            bytes.windows(needle.len()).any(|window| window == needle),
            "{name} lost CUSTOM_KEEP"
        );
        let info_after = tags::probe(&path).unwrap();
        assert_eq!(info_after.sample_rate, info_before.sample_rate, "{name}");
        assert_eq!(info_after.channels, info_before.channels, "{name}");
        assert!(
            (info_after.duration_seconds - info_before.duration_seconds).abs() < 0.01,
            "{name}"
        );
    }
}

#[test]
fn save_m4a_audio_bytes_identical() {
    let (_scratch, path) = temp_copy("management_full.m4a", "m4a-audio");
    let audio_before = m4a_mdat(&std::fs::read(&path).unwrap()).to_vec();
    tags::save_tags(&path, &[TagEdit::set_title("WRAP TITLE")]).unwrap();
    assert_eq!(m4a_mdat(&std::fs::read(&path).unwrap()), audio_before);
}

// ---------------------------------------------------------------------------
// Save wrapper: an ID3v2.3 tag holding ID3v2.4 frames is saved as v2.4
// ---------------------------------------------------------------------------

const MIXED_V23: &str = "management_full_v23.mp3";

/// One ID3 frame as compared here: user text keyed `TXXX:<description>`
/// and text frames by their values, any other frame by its payload.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum FrameBody {
    Text(Vec<String>),
    Raw(Vec<u8>),
}

/// A file's ID3 version and its frames, sorted, as [`FrameBody`] sees them.
fn id3_view(path: &Path) -> (u8, Vec<(String, FrameBody)>) {
    let (major, frames) = id3_frames(path);
    let mut view: Vec<(String, FrameBody)> = frames
        .into_iter()
        .map(|(id, payload)| {
            if id == "TXXX" {
                let mut strings = id3_strings(&payload);
                let description = strings.remove(0);
                (format!("TXXX:{description}"), FrameBody::Text(strings))
            } else if id.starts_with('T') && id != "TIPL" && id != "TMCL" {
                let values = id3_strings(&payload);
                (id, FrameBody::Text(values))
            } else {
                (id, FrameBody::Raw(payload))
            }
        })
        .collect();
    view.sort();
    (major, view)
}

/// The view without the frames keyed `skip`.
fn view_without(view: &[(String, FrameBody)], skip: &[&str]) -> Vec<(String, FrameBody)> {
    view.iter()
        .filter(|(key, _)| !skip.contains(&key.as_str()))
        .cloned()
        .collect()
}

/// Everything after the ID3v2 tag: the audio and any ID3v1 trailer.
fn after_id3v2(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap();
    let size = bytes[6..10]
        .iter()
        .fold(0usize, |acc, byte| (acc << 7) | usize::from(byte & 0x7F));
    bytes[10 + size..].to_vec()
}

/// A Latin-1 text payload holding these strings.
fn latin1(parts: &[&str]) -> Vec<u8> {
    let mut payload = vec![0];
    payload.extend_from_slice(parts.join("\0").as_bytes());
    payload
}

/// A copy of the mixed v2.3 fixture with `frames` written into its tag's
/// padding as v2.3 frames (big-endian size, no flags). The tag size and
/// the audio stay as they were.
fn mixed_v23_with(test: &str, frames: &[(&str, &[u8])]) -> (ScratchDir, PathBuf) {
    let (dir, path) = temp_copy(MIXED_V23, test);
    let mut bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes[3], 3);
    let tag_end = 10
        + bytes[6..10]
            .iter()
            .fold(0usize, |acc, byte| (acc << 7) | usize::from(byte & 0x7F));
    let mut offset = 10;
    while bytes[offset] != 0 {
        let size = u32::from_be_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        offset += 10 + size;
    }
    for (id, payload) in frames {
        let mut frame = id.as_bytes().to_vec();
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(&[0, 0]);
        frame.extend_from_slice(payload);
        assert!(offset + frame.len() < tag_end, "the padding holds {id}");
        bytes[offset..offset + frame.len()].copy_from_slice(&frame);
        offset += frame.len();
    }
    std::fs::write(&path, bytes).unwrap();
    (dir, path)
}

/// What the mixed v2.3 fixture reads as: its v2.4 frames count.
fn mixed_v23_values() -> Vec<(TagField, Vec<String>)> {
    vec![
        (TagField::Date, texts(&["2024-03-02"])),
        (TagField::OriginalDate, texts(&["2020"])),
        (TagField::AlbumSort, texts(&["Management Album, The"])),
        (TagField::ArtistSort, texts(&["Alpha, The; Beta, The"])),
        (TagField::TitleSort, texts(&["Management Track, The"])),
        (TagField::DiscSubtitle, texts(&["Main Programme"])),
        (TagField::Producer, texts(&["Example Producer"])),
        (TagField::Isrc, texts(&["GBABC2400001"])),
    ]
}

fn assert_mixed_values(fields: &std::collections::BTreeMap<TagField, Vec<String>>) {
    for (field, values) in mixed_v23_values() {
        assert_eq!(fields.get(&field), Some(&values), "{field:?}");
    }
}

/// A mixed v2.3 tag reads by the v2.4 rules wherever it holds a v2.4
/// frame, from the file and from tags captured away from it.
#[test]
fn mixed_v23_reads_its_v24_frames() {
    assert_mixed_values(&tags::read_fields(&fixture(MIXED_V23)).unwrap());
    let bytes = std::fs::read(fixture(MIXED_V23)).unwrap();
    let size = bytes[6..10]
        .iter()
        .fold(0usize, |acc, byte| (acc << 7) | usize::from(byte & 0x7F));
    let captured = tags::CapturedTags::Id3(bytes[..10 + size].to_vec());
    let document = tags::document_from_captured(AudioFormat::Mp3, &captured).unwrap();
    assert_mixed_values(&document.values);
}

/// A title edit saves the mixed fixture as ID3v2.4: every other frame
/// keeps its payload or values, and everything after the tag, an ID3v1
/// trailer included, stays byte for byte. A second save stays v2.4.
#[test]
fn mixed_v23_is_saved_as_v24() {
    let (_scratch, path) = temp_copy(MIXED_V23, "mixed-upgrade");
    let mut trailer = b"TAG".to_vec();
    trailer.resize(128, b'v');
    let mut bytes = std::fs::read(&path).unwrap();
    bytes.extend_from_slice(&trailer);
    std::fs::write(&path, &bytes).unwrap();
    let tail = after_id3v2(&path);
    let (_, before) = id3_view(&path);

    let report = tags::save_tags(&path, &[TagEdit::set_title("WRAP TITLE")]).unwrap();
    assert_eq!(report.id3_version.as_deref(), Some("2.4"));
    let (major, after) = id3_view(&path);
    assert_eq!(major, 4);
    assert_eq!(
        view_without(&after, &["TIT2"]),
        view_without(&before, &["TIT2"])
    );
    for id in ["UFID", "APIC", "TIPL"] {
        assert!(after.iter().any(|(key, _)| key == id), "{id}");
    }
    assert_eq!(id3_text_values(&path, "TIT2"), texts(&["WRAP TITLE"]));
    assert_eq!(after_id3v2(&path), tail);
    let fields = tags::read_fields(&path).unwrap();
    assert_mixed_values(&fields);
    assert_eq!(fields[&TagField::Title], texts(&["WRAP TITLE"]));

    let report = tags::save_tags(&path, &[TagEdit::set_title("Again")]).unwrap();
    assert_eq!(report.id3_version.as_deref(), Some("2.4"));
    let (major, again) = id3_view(&path);
    assert_eq!(major, 4);
    assert_eq!(
        view_without(&again, &["TIT2"]),
        view_without(&after, &["TIT2"])
    );
    assert_eq!(id3_text_values(&path, "TIT2"), texts(&["Again"]));
    assert_eq!(after_id3v2(&path), tail);
}

/// The v2.3 frames with a v2.4 home move there: the date and time to
/// `TDRC`, the original year to `TDOR`, the credits to `TIPL`, a sort
/// name to its `TSO*` frame, and an ISRC list joined with '/' splits.
#[test]
fn mixed_v23_frames_move_to_their_v24_homes() {
    let (_scratch, path) = v23_file(
        "tags-mixed-moves",
        &[
            ("TIT2", &latin1(&["Moves"])),
            ("TYER", &latin1(&["2019"])),
            ("TDAT", &latin1(&["0103"])),
            ("TORY", &latin1(&["2018"])),
            ("IPLS", &latin1(&["arranger", "X"])),
            ("TXXX", &latin1(&["ARTISTSORT", "S"])),
            ("TSRC", &latin1(&["AA/BB"])),
            ("TSOA", &latin1(&["Z"])),
        ],
    );
    let audio = mp3_audio_tail(&std::fs::read(&path).unwrap()).to_vec();
    let check = |fields: std::collections::BTreeMap<TagField, Vec<String>>| {
        assert_eq!(fields[&TagField::Date], texts(&["2019-03-01"]));
        assert_eq!(fields[&TagField::OriginalDate], texts(&["2018"]));
        assert_eq!(fields[&TagField::Arranger], texts(&["X"]));
        assert_eq!(fields[&TagField::ArtistSort], texts(&["S"]));
        assert_eq!(fields[&TagField::Isrc], texts(&["AA", "BB"]));
        assert_eq!(fields[&TagField::AlbumSort], texts(&["Z"]));
    };
    check(tags::read_fields(&path).unwrap());

    tags::save_tags(&path, &[TagEdit::set_title("Moved")]).unwrap();
    check(tags::read_fields(&path).unwrap());
    let (major, view) = id3_view(&path);
    assert_eq!(major, 4);
    assert_eq!(id3_text_values(&path, "TDRC"), texts(&["2019-03-01"]));
    assert_eq!(id3_text_values(&path, "TDOR"), texts(&["2018"]));
    assert_eq!(id3_people(&path, "TIPL"), vec![pair("arranger", "X")]);
    assert_eq!(id3_text_values(&path, "TSOP"), texts(&["S"]));
    assert_eq!(id3_text_values(&path, "TSRC"), texts(&["AA", "BB"]));
    assert_eq!(id3_text_values(&path, "TSOA"), texts(&["Z"]));
    for gone in ["TYER", "TDAT", "TORY", "IPLS", "TXXX:ARTISTSORT"] {
        assert!(view.iter().all(|(key, _)| key != gone), "{gone}");
    }
    assert_eq!(mp3_audio_tail(&std::fs::read(&path).unwrap()), audio);
}

/// A v2.3 date that agrees with the v2.4 one (its year) gives way to it.
#[test]
fn mixed_v23_agreeing_dates_give_way() {
    let (_scratch, path) = mixed_v23_with(
        "mixed-agree",
        &[("TYER", &latin1(&["2024"])), ("TORY", &latin1(&["2020"]))],
    );
    tags::save_tags(&path, &[TagEdit::set_title("Agreed")]).unwrap();
    let (major, view) = id3_view(&path);
    assert_eq!(major, 4);
    for gone in ["TYER", "TORY"] {
        assert!(view.iter().all(|(key, _)| key != gone), "{gone}");
    }
    assert_eq!(id3_text_values(&path, "TDRC"), texts(&["2024-03-02"]));
    assert_eq!(id3_text_values(&path, "TDOR"), texts(&["2020"]));
}

/// Two different dates both stay, and the date reads the v2.4 one.
#[test]
fn mixed_v23_conflicting_dates_both_stay() {
    let (_scratch, path) = mixed_v23_with("mixed-conflict", &[("TYER", &latin1(&["2023"]))]);
    tags::save_tags(&path, &[TagEdit::set_title("Conflict")]).unwrap();
    assert_eq!(id3_frames(&path).0, 4);
    assert_eq!(id3_text_values(&path, "TDRC"), texts(&["2024-03-02"]));
    assert_eq!(id3_text_values(&path, "TYER"), texts(&["2023"]));
    assert_eq!(
        tags::read_fields(&path).unwrap()[&TagField::Date],
        texts(&["2024-03-02"])
    );
}

/// What the preview says about a mixed tag: that it is saved as v2.4,
/// and each value it holds twice.
#[test]
fn mixed_v23_upgrade_notes() {
    let first = "Its ID3v2.3 tag also holds newer ID3v2.4 tags, so it is saved as ID3v2.4.";
    let (_scratch, path) = mixed_v23_with("mixed-notes", &[("TYER", &latin1(&["2023"]))]);
    assert_eq!(
        tags::save::id3_upgrade_notes(&path).unwrap(),
        vec![
            first.to_owned(),
            "It holds two different recording dates (TDRC 2024-03-02, TYER 2023). It now \
             reads as 2024-03-02; the other value stays in its own frame, which most players \
             ignore."
                .to_owned(),
        ]
    );
    let (_scratch, path) = mixed_v23_with(
        "mixed-notes-agree",
        &[("TYER", &latin1(&["2024"])), ("TORY", &latin1(&["2020"]))],
    );
    assert_eq!(
        tags::save::id3_upgrade_notes(&path).unwrap(),
        vec![first.to_owned()]
    );
    assert!(
        tags::save::id3_upgrade_notes(&fixture("management_full.mp3"))
            .unwrap()
            .is_empty()
    );
}

/// Frames with no v2.4 home, and the frames lofty writes again itself,
/// survive the upgrade with the same payload or values.
#[test]
fn mixed_v23_keeps_frames_it_does_not_move() {
    let rvad: &[u8] = &[0x03, 0x10, 0, 1, 0, 2, 0, 0, 0, 0];
    let poss: &[u8] = &[2, 0, 0, 0, 10];
    let rva2: &[u8] = b"track\0\x01\x00\x10\x00";
    let mut uslt = vec![1];
    uslt.extend_from_slice(b"deu");
    for part in ["d", "Worte"] {
        uslt.extend_from_slice(&[0xFF, 0xFE]);
        for unit in part.encode_utf16() {
            uslt.extend_from_slice(&unit.to_le_bytes());
        }
        uslt.extend_from_slice(&[0, 0]);
    }
    let private: &[u8] = b"com.example\0opaque";
    let geob: &[u8] = b"\0text/plain\0a.txt\0obj\0data";
    let wxxx: &[u8] = b"\0link\0https://example.invalid/";
    let woar: &[u8] = b"https://example.invalid/artist";
    let popm: &[u8] = b"a@example.invalid\0\x80\0\0\0\x05";
    let comm: &[u8] = b"\0franote\0bonjour";
    let raw: [(&str, &[u8]); 10] = [
        ("RVAD", rvad),
        ("POSS", poss),
        ("RVA2", rva2),
        ("USLT", &uslt),
        ("PRIV", private),
        ("GEOB", geob),
        ("WXXX", wxxx),
        ("WOAR", woar),
        ("POPM", popm),
        ("COMM", comm),
    ];
    let mut planted: Vec<(&str, &[u8])> = raw.to_vec();
    let size = latin1(&["123"]);
    let station = latin1(&["Radio"]);
    let mood = latin1(&["Calm"]);
    planted.extend([
        ("TSIZ", size.as_slice()),
        ("TRSN", station.as_slice()),
        ("TMOO", mood.as_slice()),
    ]);
    let (_scratch, path) = mixed_v23_with("mixed-kept", &planted);
    let (_, before) = id3_view(&path);

    tags::save_tags(&path, &[TagEdit::set_title("Kept")]).unwrap();
    let (major, after) = id3_view(&path);
    assert_eq!(major, 4);
    for (id, payload) in raw {
        assert!(
            after.contains(&(id.to_owned(), FrameBody::Raw(payload.to_vec()))),
            "{id} kept byte for byte"
        );
    }
    assert_eq!(id3_text_values(&path, "TSIZ"), texts(&["123"]));
    assert_eq!(id3_text_values(&path, "TRSN"), texts(&["Radio"]));
    assert_eq!(id3_text_values(&path, "TMOO"), texts(&["Calm"]));
    assert_eq!(
        view_without(&after, &["TIT2"]),
        view_without(&before, &["TIT2"])
    );
}

/// The v2.3 credits join the `TIPL` pairs, an exact duplicate once, and
/// every credit reads as before. An edit of one role keeps the others.
#[test]
fn mixed_v23_credits_merge_into_tipl() {
    let frames: [(&str, Vec<u8>); 4] = [
        ("TIT2", latin1(&["Credits"])),
        ("TIPL", latin1(&["producer", "Z"])),
        (
            "IPLS",
            latin1(&[
                "arranger", "X", "producer", "Z", "guitar", "W", "engineer", "E",
            ]),
        ),
        ("TSOP", latin1(&["S"])),
    ];
    let frames: Vec<(&str, &[u8])> = frames
        .iter()
        .map(|(id, payload)| (*id, payload.as_slice()))
        .collect();
    let credits = |path: &Path| {
        let fields = tags::read_fields(path).unwrap();
        assert_eq!(fields[&TagField::Performer], texts(&["W (guitar)"]));
        assert_eq!(fields[&TagField::Producer], texts(&["Z"]));
        assert_eq!(fields[&TagField::Arranger], texts(&["X"]));
    };

    let (_scratch, path) = v23_file("tags-mixed-credits", &frames);
    credits(&path);
    tags::save_tags(&path, &[TagEdit::set_title("Merged")]).unwrap();
    assert_eq!(id3_frames(&path).0, 4);
    assert_eq!(
        id3_people(&path, "TIPL"),
        vec![
            pair("producer", "Z"),
            pair("arranger", "X"),
            pair("guitar", "W"),
            pair("engineer", "E"),
        ]
    );
    assert!(id3_people(&path, "IPLS").is_empty());
    credits(&path);

    let (_scratch, path) = v23_file("tags-mixed-credits-edit", &frames);
    tags::save_tags(&path, &[TagEdit::new(TagField::Producer, texts(&["P2"]))]).unwrap();
    assert_eq!(
        id3_people(&path, "TIPL"),
        vec![
            pair("producer", "P2"),
            pair("arranger", "X"),
            pair("guitar", "W"),
            pair("engineer", "E"),
        ]
    );
}

/// An edit of a moved field writes its v2.4 home only.
#[test]
fn mixed_v23_edits_write_v24_homes() {
    let (_scratch, path) = temp_copy(MIXED_V23, "mixed-edit");
    tags::save_tags(
        &path,
        &[
            TagEdit::new(TagField::ArtistSort, texts(&["New"])),
            TagEdit::new(TagField::Date, texts(&["2025"])),
        ],
    )
    .unwrap();
    assert_eq!(id3_text_values(&path, "TSOP"), texts(&["New"]));
    assert_eq!(id3_text_values(&path, "TDRC"), texts(&["2025"]));
    assert!(id3_txxx_values(&path, "ARTISTSORT").is_empty());
    let (_, view) = id3_view(&path);
    for gone in ["TYER", "TDAT", "TIME"] {
        assert!(view.iter().all(|(key, _)| key != gone), "{gone}");
    }
}

#[test]
fn save_pure_v23_stays_v23() {
    // A genuinely pure v2.3 tag (synthetic header over real MP3 audio):
    // the title edit lands, the version stays pinned, TYER survives.
    let audio = mp3_audio_tail(&std::fs::read(fixture("management_full.mp3")).unwrap()).to_vec();
    let mut tag = id3v2_bytes(
        3,
        &[
            ("TIT2", b"Management Track"),
            ("TPE1", b"Alpha feat. Beta"),
            ("TALB", b"Management Album"),
            ("TYER", b"2024"),
            ("TRCK", b"2"),
        ],
    );
    tag.extend_from_slice(&audio);
    let dir = ScratchDir::new("tags-purev23");
    let path = dir.join("pure_v23.mp3");
    std::fs::write(&path, &tag).unwrap();

    let report = tags::save_tags(&path, &[TagEdit::set_title("WRAP TITLE")]).unwrap();
    assert_eq!(report.id3_version.as_deref(), Some("2.3"));
    let (tag, _) = tags::read_tags(&path).unwrap();
    assert_eq!(tag.title, "WRAP TITLE");
    let bytes = std::fs::read(&path).unwrap();
    let byte_report = inspect_id3_bytes(&bytes).unwrap();
    assert_eq!(byte_report.version_major, 3);
    assert!(byte_report.frame_ids.contains(&"TYER".to_owned()));
    assert_eq!(mp3_audio_tail(&bytes), audio);

    // The full tag set keeps it a clean v2.3 tag: the date split over
    // TYER/TDAT, the original year in TORY, album artist sort in TSO2 and
    // the other sort names in TXXX, multiple values joined with '/'.
    let report = tags::save_tags(&path, &picard_edits()).unwrap();
    assert_eq!(report.id3_version.as_deref(), Some("2.3"));
    let bytes = std::fs::read(&path).unwrap();
    let byte_report = inspect_id3_bytes(&bytes).unwrap();
    assert_eq!(mixed_v23_detail(&byte_report), None);
    for id in ["TYER", "TDAT", "TORY", "TSO2"] {
        assert!(byte_report.frame_ids.contains(&id.to_owned()), "{id}");
    }
    let fields = tags::read_fields(&path).unwrap();
    assert_eq!(fields[&TagField::Date], vec!["2024-05-10".to_owned()]);
    assert_eq!(fields[&TagField::OriginalDate], vec!["2019".to_owned()]);
    assert_eq!(
        fields[&TagField::ArtistSort],
        vec!["Lanterns, The & Guest".to_owned()]
    );
    // A later save keeps the clean v2.3 tag writable.
    tags::save_tags(&path, &[TagEdit::set_title("Again")]).unwrap();
    assert_eq!(
        fields[&TagField::Artists],
        vec!["The Lanterns/Guest".to_owned()]
    );
    assert_eq!(fields[&TagField::TrackTotal], vec!["12".to_owned()]);
    assert_eq!(mp3_audio_tail(&bytes), audio);
}

#[test]
fn save_refuses_fixture_with_empty_values() {
    // flac_no_tags.flac is not tag-free: it carries empty TITLE/ARTIST/
    // ALBUM values, which lofty would drop on save.
    let (refusal, before, after) = save_refused(
        "flac_no_tags.flac",
        "no-tags",
        &[TagEdit::set_title("Fresh Title")],
    );
    assert!(
        matches!(refusal, Refusal::EmptyVorbisValue { .. }),
        "{refusal:?}"
    );
    assert_eq!(before, after);
}

#[test]
fn save_untagged_flac_gains_a_title() {
    let (_scratch, path) = temp_copy("management_full.flac", "untagged");
    flac_strip_comments(&path);
    tags::save_tags(&path, &[TagEdit::set_title("Fresh Title")]).unwrap();
    let (tag, _) = tags::read_tags(&path).unwrap();
    assert_eq!(tag.title, "Fresh Title");
}

// ---------------------------------------------------------------------------
// Save wrapper: loud refusals, originals untouched
// ---------------------------------------------------------------------------

fn save_refused(name: &str, test: &str, edits: &[TagEdit]) -> (Refusal, Vec<u8>, Vec<u8>) {
    let (_scratch, path) = temp_copy(name, test);
    let before = std::fs::read(&path).unwrap();
    let refusal = match tags::save_tags(&path, edits) {
        Err(TagsError::SaveRefused { refusal, .. }) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    };
    let after = std::fs::read(&path).unwrap();
    (refusal, before, after)
}

/// A total the file spells both ways is updated under both spellings;
/// a file using neither gets Picard's.
#[test]
fn vorbis_totals_update_every_spelling() {
    let (_scratch, path) = temp_copy("management_full.flac", "totals");
    flac_inject_pairs(&path, &[("TRACKTOTAL", "9")]);
    tags::save_tags(
        &path,
        &[
            TagEdit::new(TagField::TrackTotal, vec!["12".to_owned()]),
            TagEdit::new(TagField::DiscTotal, vec!["3".to_owned()]),
        ],
    )
    .unwrap();
    assert_eq!(vorbis_values(&path, "TOTALTRACKS"), vec!["12"]);
    assert_eq!(vorbis_values(&path, "TRACKTOTAL"), vec!["12"]);
    assert_eq!(vorbis_values(&path, "TOTALDISCS"), vec!["3"]);
    assert!(vorbis_values(&path, "DISCTOTAL").is_empty());
}

/// A file whose total spellings disagree records each one, and replaying
/// that document puts each spelling back to its own value.
#[test]
fn vorbis_spellings_restore_their_own_values() {
    use droppedneedle_library::publish::staging;
    let (_scratch, path) = temp_copy("management_full.flac", "spellings");
    flac_inject_pairs(&path, &[("TOTALTRACKS", "12"), ("TRACKTOTAL", "9")]);
    let baseline = staging::document_from_file(&path).unwrap();
    assert_eq!(
        baseline
            .managed
            .get(&TagTarget::Spelling(TagField::TrackTotal, "TRACKTOTAL")),
        Some(&vec!["9".to_owned()])
    );
    let totals = vorbis_values(&path, "TOTALTRACKS");
    tags::save_tags(
        &path,
        &[TagEdit::new(TagField::TrackTotal, vec!["15".to_owned()])],
    )
    .unwrap();
    assert_eq!(vorbis_values(&path, "TRACKTOTAL"), vec!["15"]);
    let edits = staging::tag_edits(&baseline.managed);
    tags::save_tags(&path, &edits).unwrap();
    assert_eq!(vorbis_values(&path, "TOTALTRACKS"), totals);
    assert_eq!(vorbis_values(&path, "TRACKTOTAL"), vec!["9"]);
}

/// A restore removes fields the baseline lacked, except ones the file
/// holds opaquely now; an older-shaped baseline removes nothing.
#[test]
fn restore_updates_spare_opaque_and_older_baselines() {
    use droppedneedle_library::publish::{TagDocument, staging};
    let mut baseline = TagDocument::empty();
    baseline
        .managed
        .insert(TagTarget::Field(TagField::Title), vec!["Kept".to_owned()]);
    let mut current = TagDocument::empty();
    current.opaque.push("date".to_owned());
    let updates = staging::restore_updates(&baseline, &current, "a.flac");
    assert_eq!(
        updates.field(TagField::Title),
        Some(&vec!["Kept".to_owned()])
    );
    assert_eq!(updates.field(TagField::Artist), Some(&Vec::new()));
    assert!(updates.field(TagField::Date).is_none());
    baseline.version = 1;
    let updates = staging::restore_updates(&baseline, &current, "a.flac");
    assert_eq!(updates, baseline.managed);
}

/// A restore from a document recorded before the credit fields existed
/// leaves them as the file holds them: a version 4 document listed only
/// some as native items (never the TIPL and TMCL pairs or the movement
/// frames), and a version 2 document lists none.
#[test]
fn older_baselines_keep_the_credit_tags() {
    use droppedneedle_library::publish::staging;
    let (_scratch, path) = temp_copy("management_full.mp3", "old-baseline");
    id3_inject_frames(
        &path,
        &[
            ("TMCL", b"\x03guitar\x00A\x00"),
            ("MVNM", b"\x03Allegro\x00"),
            ("MVIN", b"\x032/4\x00"),
        ],
    );
    let held = |path: &Path| {
        (
            id3_people(path, "TIPL"),
            id3_people(path, "TMCL"),
            id3_text_values(path, "MVNM"),
            id3_text_values(path, "MVIN"),
            id3_text_values(path, "TCOM"),
        )
    };
    let original = held(&path);
    assert_eq!(original.0, vec![pair("producer", "Example Producer")]);
    assert_eq!(original.1, vec![pair("guitar", "A")]);
    assert_eq!((original.2.len(), original.3.len()), (1, 1));
    assert!(!original.4.is_empty());
    // The document as the reader before these fields recorded it.
    let mut old = staging::document_from_file(&path).unwrap();
    let new_fields = &TagField::ALL[TagField::ALL
        .iter()
        .position(|field| *field == TagField::AlbumArtists)
        .unwrap()..];
    for field in new_fields {
        let Some(values) = old.managed.remove(&TagTarget::Field(*field)) else {
            continue;
        };
        let key = match field {
            TagField::Composer => "id3:TCOM",
            TagField::Lyricist => "id3:TEXT",
            TagField::Work => "id3:TXXX:WORK",
            TagField::MusicBrainzWorkId => "id3:TXXX:MusicBrainz Work Id",
            TagField::AcoustIdId => "id3:TXXX:Acoustid Id",
            TagField::AcoustIdFingerprint => "id3:TXXX:Acoustid Fingerprint",
            // Key-value and movement frames were never listed.
            _ => continue,
        };
        old.custom.insert(key.to_owned(), values);
    }
    for version in [4, 2] {
        let mut baseline = old.clone();
        baseline.version = version;
        if version < 4 {
            baseline.custom.clear();
        }
        tags::save_tags(&path, &[TagEdit::set_title("Later")]).unwrap();
        let current = staging::document_from_file(&path).unwrap();
        let updates = staging::restore_updates(&baseline, &current, "a.mp3");
        let edits = staging::tag_edits(&updates);
        tags::save_tags(&path, &edits).unwrap();
        assert_eq!(held(&path), original, "version {version}");
        assert_eq!(id3_text_values(&path, "TIT2"), texts(&["Management Track"]));
    }
}

/// A restore from a version 5 document, recorded before engineer, mixer,
/// DJ-mixer, writer and director were fields, never removes their tags,
/// whatever spelling the file holds: it listed their Vorbis, `TXXX` and
/// MP4 tags as native items and their `TIPL` and `IPLS` pairs not at
/// all. A native item a later run changed goes back to its value.
#[test]
fn version_5_baselines_keep_the_other_role_tags() {
    use droppedneedle_library::publish::staging;
    let other_roles = [
        TagField::Engineer,
        TagField::Mixer,
        TagField::DjMixer,
        TagField::Writer,
        TagField::Director,
    ];
    // Restores `path` from its document as the version 5 reader recorded
    // it (`native` names the items it listed), after `later` was saved.
    let restore = |path: &Path, native: &[(TagField, &str)], later: &[TagEdit]| {
        let mut baseline = staging::document_from_file(path).unwrap();
        for field in other_roles {
            let values = baseline.managed.remove(&TagTarget::Field(field));
            if let Some((_, key)) = native.iter().find(|(owned, _)| *owned == field) {
                baseline
                    .custom
                    .insert((*key).to_owned(), values.expect("the file holds it"));
            }
        }
        baseline.version = 5;
        let mut edits = vec![TagEdit::set_title("Later")];
        edits.extend_from_slice(later);
        tags::save_tags(path, &edits).unwrap();
        let current = staging::document_from_file(path).unwrap();
        let name = path.file_name().unwrap().to_str().unwrap();
        let updates = staging::restore_updates(&baseline, &current, name);
        let edits = staging::tag_edits(&updates);
        tags::save_tags(path, &edits).unwrap();
    };

    // MP3 v2.4: TIPL engineer and mix pairs, and Picard's TXXX:Writer.
    let (_scratch, mp3) = temp_copy("management_full.mp3", "v5-other-roles");
    let audio = mp3_audio_tail(&std::fs::read(&mp3).unwrap()).to_vec();
    let mut frames: Vec<(String, Vec<u8>)> = id3_frames(&mp3)
        .1
        .into_iter()
        .filter(|(id, _)| id != "TIPL")
        .collect();
    frames.push((
        "TIPL".to_owned(),
        b"\x03producer\x00Example Producer\x00engineer\x00Eve Engineer\x00mix\x00Max Mixer\x00"
            .to_vec(),
    ));
    frames.push(("TXXX".to_owned(), b"\x03Writer\x00Wanda Writer".to_vec()));
    let refs: Vec<(&str, &[u8])> = frames
        .iter()
        .map(|(id, payload)| (id.as_str(), payload.as_slice()))
        .collect();
    let mut bytes = id3v24_raw(&refs);
    bytes.extend_from_slice(&audio);
    std::fs::write(&mp3, &bytes).unwrap();
    let tipl = id3_people(&mp3, "TIPL");
    restore(&mp3, &[(TagField::Writer, "id3:TXXX:Writer")], &[]);
    assert_eq!(id3_people(&mp3, "TIPL"), tipl);
    assert_eq!(id3_txxx_values(&mp3, "Writer"), texts(&["Wanda Writer"]));
    assert_eq!(id3_text_values(&mp3, "TIT2"), texts(&["Management Track"]));

    // Another tagger's TXXX:WRITER.
    let (_scratch, upper) = temp_copy("management_full.mp3", "v5-other-roles-upper");
    id3_inject_frames(&upper, &[("TXXX", b"\x03WRITER\x00Other Writer")]);
    restore(&upper, &[(TagField::Writer, "id3:TXXX:WRITER")], &[]);
    assert_eq!(id3_txxx_values(&upper, "WRITER"), texts(&["Other Writer"]));

    // MP3 v2.3: IPLS engineer pairs beside a musician credit.
    let ipls = latin1(&["engineer", "Eve Engineer", "guitar", "A"]);
    let (_scratch, v23) = v23_file(
        "v5-other-roles-v23",
        &[("TIT2", b"\x00Old Title"), ("IPLS", &ipls)],
    );
    restore(&v23, &[], &[]);
    assert_eq!(
        id3_people(&v23, "IPLS"),
        vec![pair("engineer", "Eve Engineer"), pair("guitar", "A")]
    );

    // FLAC: ENGINEER, which a later run changed, and WRITER.
    let (_scratch, flac) = temp_copy("management_full.flac", "v5-other-roles");
    flac_inject_pairs(
        &flac,
        &[("ENGINEER", "Eve Engineer"), ("WRITER", "Wanda Writer")],
    );
    restore(
        &flac,
        &[
            (TagField::Engineer, "vorbis:ENGINEER"),
            (TagField::Writer, "vorbis:WRITER"),
        ],
        &[TagEdit::new(TagField::Engineer, texts(&["Changed"]))],
    );
    assert_eq!(
        vorbis_key_values(&flac, "ENGINEER"),
        texts(&["Eve Engineer"])
    );
    assert_eq!(vorbis_key_values(&flac, "WRITER"), texts(&["Wanda Writer"]));

    // M4A: ----:ENGINEER and ©dir.
    let (_scratch, m4a) = temp_copy("management_full.m4a", "v5-other-roles");
    tags::save_tags(
        &m4a,
        &[
            TagEdit::new(TagField::Engineer, texts(&["Eve Engineer"])),
            TagEdit::new(TagField::Director, texts(&["Dora Director"])),
        ],
    )
    .unwrap();
    restore(
        &m4a,
        &[
            (TagField::Engineer, "mp4:----:ENGINEER"),
            (TagField::Director, "mp4:\u{a9}dir"),
        ],
        &[],
    );
    assert_eq!(mp4_freeform(&m4a, "ENGINEER"), texts(&["Eve Engineer"]));
    assert_eq!(mp4_text(&m4a, b"\xa9dir"), texts(&["Dora Director"]));
}

/// Values no new edit could carry (a `3/12` track number, a free-text
/// date) are written back exactly when replayed verbatim.
#[test]
fn verbatim_values_round_trip() {
    let (_scratch, path) = temp_copy("management_full.flac", "verbatim");
    let raw = [
        (TagField::TrackNumber, "3/12"),
        (TagField::Date, "circa 1970"),
    ];
    assert!(!tags::save::accepts(&TagEdit::new(
        raw[0].0,
        vec![raw[0].1.to_owned()]
    )));
    let edits: Vec<TagEdit> = raw
        .iter()
        .map(|(field, value)| TagEdit::verbatim(*field, vec![(*value).to_owned()]))
        .collect();
    tags::save_tags(&path, &edits).unwrap();
    let fields = tags::read_fields(&path).unwrap();
    for (field, value) in raw {
        assert_eq!(fields[&field], vec![value.to_owned()], "{field:?}");
    }
}

#[test]
fn save_refuses_values_a_field_cannot_hold() {
    let cases = [
        (
            "timestamp",
            TagEdit::new(TagField::Date, vec!["FUZZ".to_owned()]),
        ),
        (
            "count",
            TagEdit::new(TagField::TrackNumber, vec!["abc".to_owned()]),
        ),
        (
            "single",
            TagEdit::new(
                TagField::MusicBrainzReleaseId,
                vec!["one".to_owned(), "two".to_owned()],
            ),
        ),
        ("blank", TagEdit::new(TagField::Title, vec![String::new()])),
    ];
    for (test, edit) in cases {
        let (refusal, before, after) = save_refused("management_full.mp3", test, &[edit]);
        assert!(
            matches!(refusal, Refusal::UnencodableItem { .. }),
            "{test}: {refusal:?}"
        );
        assert_eq!(before, after, "{test}");
    }
}

/// Picard's tag set as the publisher writes it for one release track.
fn picard_edits() -> Vec<TagEdit> {
    let one = |field: TagField, value: &str| TagEdit::new(field, vec![value.to_owned()]);
    vec![
        one(TagField::Title, "Blue Hour"),
        one(TagField::TitleSort, "Blue Hour, The"),
        one(TagField::AlbumSort, "Night Shift (sorted)"),
        one(TagField::Compilation, "1"),
        one(TagField::DiscSubtitle, "The Late Side"),
        one(TagField::Artist, "The Lanterns & Guest"),
        TagEdit::new(
            TagField::Artists,
            vec!["The Lanterns".to_owned(), "Guest".to_owned()],
        ),
        one(TagField::ArtistSort, "Lanterns, The & Guest"),
        one(TagField::Album, "Night Shift"),
        one(TagField::AlbumArtist, "The Lanterns"),
        one(TagField::AlbumArtistSort, "Lanterns, The"),
        one(TagField::Genre, "Indie"),
        one(TagField::TrackNumber, "3"),
        one(TagField::TrackTotal, "12"),
        one(TagField::DiscNumber, "2"),
        one(TagField::DiscTotal, "2"),
        one(TagField::Date, "2024-05-10"),
        one(TagField::OriginalDate, "2019-03-08"),
        one(TagField::ReleaseStatus, "official"),
        one(TagField::ReleaseCountry, "XW"),
        TagEdit::new(
            TagField::ReleaseType,
            vec!["album".to_owned(), "compilation".to_owned()],
        ),
        one(TagField::Media, "Digital Media"),
        TagEdit::new(
            TagField::Label,
            vec!["Harbour Lights".to_owned(), "Night Bus".to_owned()],
        ),
        one(TagField::CatalogNumber, "LNT001"),
        one(TagField::Barcode, "5051083139822"),
        one(TagField::Isrc, "GBAYE0601498"),
        one(TagField::Asin, "B07NQ6ZJ4X"),
        one(
            TagField::MusicBrainzRecordingId,
            "c0ffee00-0000-4000-8000-00000000d005",
        ),
        one(
            TagField::MusicBrainzReleaseTrackId,
            "c0ffee00-0000-4000-8000-00000000e102",
        ),
        one(
            TagField::MusicBrainzReleaseId,
            "c0ffee00-0000-4000-8000-00000000c002",
        ),
        one(
            TagField::MusicBrainzReleaseGroupId,
            "c0ffee00-0000-4000-8000-00000000b001",
        ),
        TagEdit::new(
            TagField::MusicBrainzArtistId,
            vec![
                "c0ffee00-0000-4000-8000-00000000a001".to_owned(),
                "c0ffee00-0000-4000-8000-00000000a002".to_owned(),
            ],
        ),
        one(
            TagField::MusicBrainzAlbumArtistId,
            "c0ffee00-0000-4000-8000-00000000a001",
        ),
        one(TagField::ReplayGainTrackGain, "-6.52 dB"),
        one(TagField::ReplayGainTrackPeak, "0.988525"),
        one(TagField::ReplayGainAlbumGain, "-7.01 dB"),
        one(TagField::ReplayGainAlbumPeak, "1.000000"),
        one(TagField::Lyrics, "First line\nSecond line"),
        one(TagField::Engineer, "Eve Engineer"),
        one(TagField::Mixer, "Max Mixer"),
        one(TagField::DjMixer, "Dee Jay"),
        one(TagField::Writer, "Wanda Writer"),
        one(TagField::Director, "Dora Director"),
    ]
    .into_iter()
    .chain(credit_edits())
    .collect()
}

/// The full tag set lands in every writable format through the safe
/// save, reads back field for field (and through the scan's reader),
/// and a field with no values is removed.
#[test]
fn picard_tag_set_round_trips_per_format() {
    let edits = picard_edits();
    assert_eq!(edits.len(), TagField::ALL.len());
    for name in [
        "management_full.flac",
        "management_full.mp3",
        "management_full.ogg",
        "management_full.opus",
        "management_full.m4a",
    ] {
        let (_scratch, path) = temp_copy(name, "picard");
        // MP4 has no home for performers or arrangers.
        let edits: Vec<TagEdit> = edits
            .iter()
            .filter(|edit| {
                !name.ends_with(".m4a")
                    || !matches!(edit.field, TagField::Performer | TagField::Arranger)
            })
            .cloned()
            .collect();
        tags::save_tags(&path, &edits).unwrap_or_else(|error| panic!("{name}: {error}"));
        let fields = tags::read_fields(&path).unwrap();
        for edit in &edits {
            assert_eq!(
                fields.get(&edit.field),
                Some(&edit.values),
                "{name}: {:?}",
                edit.field
            );
        }
        let (tag, _) = tags::read_tags(&path).unwrap();
        assert_eq!((tag.track_number, tag.disc_number), (3, 2), "{name}");
        assert_eq!(tag.album_artist.as_deref(), Some("The Lanterns"), "{name}");
        assert_eq!(
            tag.musicbrainz_recording_id.as_deref(),
            Some("c0ffee00-0000-4000-8000-00000000d005"),
            "{name}"
        );
        assert_eq!(
            tag.musicbrainz_release_id.as_deref(),
            Some("c0ffee00-0000-4000-8000-00000000c002"),
            "{name}"
        );
        assert_eq!(
            tag.musicbrainz_release_track_id.as_deref(),
            Some("c0ffee00-0000-4000-8000-00000000e102"),
            "{name}"
        );

        tags::save_tags(&path, &[TagEdit::new(TagField::Barcode, Vec::new())])
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let fields = tags::read_fields(&path).unwrap();
        assert!(!fields.contains_key(&TagField::Barcode), "{name}");
        assert_eq!(fields.len(), edits.len() - 1, "{name}");
    }
    // WAV and AAC stay read-only: nothing there for a publish to write.
    for name in ["management_full.wav", "management_full.aac"] {
        assert!(
            tags::read_fields(&fixture(name)).unwrap().is_empty(),
            "{name}"
        );
    }
}

/// Native items outside the managed fields are listed, removed and put
/// back verbatim: what a scrub and its undo do.
#[test]
fn native_items_scrub_and_restore() {
    for (name, key) in [
        ("management_full.flac", "vorbis:MOOD"),
        ("management_full.mp3", "id3:TBPM"),
        ("management_full.m4a", "mp4:----:MOOD"),
    ] {
        let (_scratch, path) = temp_copy(name, "native");
        let values = vec!["Someone".to_owned()];
        let native = |path: &Path| tags::save::read_document(path).unwrap().native;
        tags::save_tags(&path, &[TagEdit::native(key, values.clone())])
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(native(&path).get(key), Some(&values), "{name}");
        tags::save_tags(&path, &[TagEdit::native(key, Vec::new())])
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert!(!native(&path).contains_key(key), "{name}");
        tags::save_tags(&path, &[TagEdit::native(key, values.clone())])
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        assert_eq!(native(&path).get(key), Some(&values), "{name}");
    }
}

/// ReplayGain analysis decodes each writable format (the fixtures are
/// digital silence, so they measure no loudness and no peak; a lossy
/// decoder may leave float dust far below one 24-bit step).
#[test]
fn loudness_measures_the_fixtures() {
    use tags::loudness::analyse;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    for name in [
        "management_full.flac",
        "management_full.mp3",
        "management_full.ogg",
        "management_full.opus",
        "management_full.m4a",
    ] {
        let measured = analyse(&fixture(name), deadline).expect(name);
        assert_eq!(measured.loudness(), None, "{name}");
        assert!(measured.peak < 1e-9, "{name}: {}", measured.peak);
        assert_eq!(tags::loudness::peak_text(measured.peak), "0.000000");
    }
}

/// Embedded pictures are replaced as a set and read back byte for byte.
#[test]
fn pictures_replace_and_read_back() {
    use tags::save::{PictureSpec, read_document, save_tags_with_pictures};
    let png = {
        let mut bytes = Vec::new();
        image::RgbImage::new(4, 4)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    };
    let front = PictureSpec {
        kind: 3,
        mime: "image/png".to_owned(),
        description: String::new(),
        data: png,
        info: None,
    };
    for name in [
        "management_full.flac",
        "management_full.mp3",
        "management_full.m4a",
    ] {
        let (_scratch, path) = temp_copy(name, "pictures");
        let before = read_document(&path).unwrap().pictures;
        save_tags_with_pictures(&path, &[], Some(std::slice::from_ref(&front)))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let after = read_document(&path).unwrap().pictures;
        assert_eq!(after.len(), 1, "{name}");
        assert_eq!(after[0].data, front.data, "{name}");
        // MP4 has no picture types: its first cover reads as the front.
        assert_eq!(after[0].kind, 3, "{name}");
        save_tags_with_pictures(&path, &[], Some(&before))
            .unwrap_or_else(|error| panic!("{name}: {error}"));
        let restored = read_document(&path).unwrap().pictures;
        if name.ends_with(".m4a") {
            let data = |list: &[PictureSpec]| {
                list.iter()
                    .map(|picture| picture.data.clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(data(&restored), data(&before), "{name}");
        } else {
            // Type, MIME type, description, bytes and FLAC picture facts.
            assert_eq!(restored, before, "{name}");
        }
    }
}

/// FLAC picture-block facts go back as they were, not recomputed.
#[test]
fn flac_picture_facts_survive_a_restore() {
    use tags::save::{PictureSpec, read_document, save_tags_with_pictures};
    let png = {
        let mut bytes = Vec::new();
        image::RgbImage::new(4, 4)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        bytes
    };
    let odd = PictureSpec {
        kind: 4,
        mime: "image/png".to_owned(),
        description: "back".to_owned(),
        data: png,
        info: Some([7, 9, 16, 0]),
    };
    let (_scratch, path) = temp_copy("management_full.flac", "picture-facts");
    save_tags_with_pictures(&path, &[], Some(std::slice::from_ref(&odd))).unwrap();
    assert_eq!(read_document(&path).unwrap().pictures, vec![odd]);
}

/// A frame over the listing cap is never listed, so a scrub of its id
/// removes only the small sibling, and undo brings that one back while
/// the large one stays put throughout.
#[test]
fn id3_large_frame_survives_scrub_and_undo() {
    use tags::save::read_document;
    let (_scratch, path) = temp_copy("management_full.mp3", "large-geob");
    let encode =
        |bytes: &[u8]| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    let geob = |desc: &str, size: usize| {
        let mut payload = b"\x00application/octet-stream\x00blob.bin\x00".to_vec();
        payload.extend_from_slice(desc.as_bytes());
        payload.push(0);
        payload.extend((0..size).map(|index| (index % 251) as u8));
        payload
    };
    let large = geob("large", (1 << 20) + 4096);
    let small = encode(&geob("small", 64));
    tags::save_tags(
        &path,
        &[TagEdit::native(
            "id3:GEOB",
            vec![encode(&large), small.clone()],
        )],
    )
    .unwrap();
    let listed = read_document(&path).unwrap().native;
    assert_eq!(
        listed.get("id3:GEOB"),
        Some(&vec![small.clone()]),
        "only the small frame is listed"
    );
    let geob_payloads = |path: &Path| -> Vec<usize> {
        let tag = lofty::id3::v2::Id3v2Tag::from(
            lofty::read_from_path(path)
                .unwrap()
                .tag(lofty::tag::TagType::Id3v2)
                .unwrap()
                .clone(),
        );
        let mut sizes: Vec<usize> = tag
            .iter()
            .filter(|frame| frame.id().as_str() == "GEOB")
            .map(|frame| match frame {
                lofty::id3::v2::Frame::Binary(binary) => binary.as_bytes().len(),
                other => panic!("unexpected GEOB shape {other:?}"),
            })
            .collect();
        sizes.sort_unstable();
        sizes
    };
    let both = geob_payloads(&path);
    assert_eq!(both.len(), 2);
    // Scrub: the listed (small) frame goes, the large one stays.
    tags::save_tags(&path, &[TagEdit::native("id3:GEOB", Vec::new())]).unwrap();
    assert_eq!(geob_payloads(&path), vec![both[1]], "the large frame stays");
    assert!(
        !read_document(&path)
            .unwrap()
            .native
            .contains_key("id3:GEOB")
    );
    // Undo: the small one is back beside it.
    tags::save_tags(&path, &[TagEdit::native("id3:GEOB", vec![small.clone()])]).unwrap();
    assert_eq!(geob_payloads(&path), both, "both frames after undo");
    assert_eq!(
        read_document(&path).unwrap().native.get("id3:GEOB"),
        Some(&vec![small])
    );
}

/// A Vorbis key spelled two ways is never listed, and an edit of one
/// spelling is refused rather than touching the other.
#[test]
fn vorbis_case_variants_are_left_alone() {
    use tags::save::read_document;
    let (_scratch, path) = temp_copy("management_full.flac", "vorbis-case");
    flac_inject_pairs(&path, &[("Mood", "Calm"), ("MOOD", "Bright")]);
    let native = read_document(&path).unwrap().native;
    assert!(
        native
            .keys()
            .all(|key| !key.eq_ignore_ascii_case("vorbis:mood")),
        "{native:?}"
    );
    let before = std::fs::read(&path).unwrap();
    assert!(tags::save_tags(&path, &[TagEdit::native("vorbis:MOOD", Vec::new())]).is_err());
    assert_eq!(before, std::fs::read(&path).unwrap());
}

/// ID3 comment, URL and private frames are native items: a scrub can
/// remove them and a restore writes the same bytes back.
#[test]
fn id3_raw_frames_scrub_and_restore() {
    use tags::save::read_document;
    let (_scratch, path) = temp_copy("management_full.mp3", "raw-frames");
    let encode =
        |bytes: &[u8]| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    let comment = encode(b"\x03engnote\x00a note");
    let private = encode(b"owner.example\x00\x01\x02\x03");
    let link = encode(b"https://example.org/artist");
    tags::save_tags(
        &path,
        &[
            TagEdit::native("id3:COMM", vec![comment.clone()]),
            TagEdit::native("id3:PRIV", vec![private.clone()]),
            TagEdit::native("id3:WOAR", vec![link.clone()]),
        ],
    )
    .unwrap();
    let held = read_document(&path).unwrap().native;
    assert_eq!(held.get("id3:COMM"), Some(&vec![comment.clone()]));
    assert_eq!(held.get("id3:PRIV"), Some(&vec![private.clone()]));
    assert_eq!(held.get("id3:WOAR"), Some(&vec![link.clone()]));
    tags::save_tags(
        &path,
        &[
            TagEdit::native("id3:PRIV", Vec::new()),
            TagEdit::native("id3:WOAR", Vec::new()),
        ],
    )
    .unwrap();
    let scrubbed = read_document(&path).unwrap().native;
    assert!(!scrubbed.contains_key("id3:PRIV"));
    assert!(!scrubbed.contains_key("id3:WOAR"));
    assert_eq!(scrubbed.get("id3:COMM"), Some(&vec![comment]));
    tags::save_tags(
        &path,
        &[
            TagEdit::native("id3:PRIV", vec![private.clone()]),
            TagEdit::native("id3:WOAR", vec![link.clone()]),
        ],
    )
    .unwrap();
    let restored = read_document(&path).unwrap().native;
    assert_eq!(restored.get("id3:PRIV"), Some(&vec![private]));
    assert_eq!(restored.get("id3:WOAR"), Some(&vec![link]));
}

#[test]
fn save_refuses_empty_vorbis_values() {
    let (_scratch, path) = temp_copy("management_full.flac", "empty-vorbis");
    flac_inject_pairs(&path, &[("ZZZ_EMPTY_TEST", "")]);
    let before = std::fs::read(&path).unwrap();
    let refusal = match tags::save_tags(&path, &[TagEdit::set_title("WRAP TITLE")]) {
        Err(TagsError::SaveRefused { refusal, .. }) => refusal,
        other => panic!("expected EmptyVorbisValue, got {other:?}"),
    };
    assert!(
        matches!(refusal, Refusal::EmptyVorbisValue { .. }),
        "{refusal:?}"
    );
    assert_eq!(before, std::fs::read(&path).unwrap());
}

#[test]
fn save_refuses_read_only_containers() {
    for name in ["management_full.aac", "management_full.wav"] {
        let (refusal, before, after) =
            save_refused(name, "readonly", &[TagEdit::set_title("WRAP TITLE")]);
        assert!(
            matches!(refusal, Refusal::ReadOnlyFormat { .. }),
            "{name}: {refusal:?}"
        );
        assert_eq!(before, after, "{name}");
    }
}

#[test]
fn mixed_v23_detector() {
    // Synthetic pure v2.3: TYER + TDAT, no complaint.
    let pure = id3v2_bytes(3, &[("TYER", b"2024"), ("TDAT", b"0203")]);
    let report = inspect_id3_bytes(&pure).unwrap();
    assert_eq!(report.version_major, 3);
    assert_eq!(mixed_v23_detail(&report), None);

    // Synthetic mixed v2.3: a literal TDRC plus v2.4 sort frames.
    let mixed = id3v2_bytes(3, &[("TDRC", b"2024-03-02"), ("TSOP", b"Alpha")]);
    let report = inspect_id3_bytes(&mixed).unwrap();
    let detail = mixed_v23_detail(&report).unwrap();
    assert!(detail.contains("TDRC"), "{detail}");

    // lofty's v2.3 writer drops TDEN and TDTG too.
    let mixed = id3v2_bytes(3, &[("TDEN", b"2024"), ("TDTG", b"2024")]);
    let detail = mixed_v23_detail(&inspect_id3_bytes(&mixed).unwrap()).unwrap();
    assert!(
        detail.contains("TDEN") && detail.contains("TDTG"),
        "{detail}"
    );

    // iTunes' and the legacy sort frames alone are not v2.4 frames.
    let itunes = id3v2_bytes(
        3,
        &[
            ("TSO2", b"Alpha"),
            ("TCMP", b"1"),
            ("GRP1", b"Group"),
            ("XSOP", b"Alpha"),
        ],
    );
    assert_eq!(mixed_v23_detail(&inspect_id3_bytes(&itunes).unwrap()), None);

    // v2.4 tags are never "mixed".
    let modern = id3v2_bytes(4, &[("TDRC", b"2024-03-02"), ("TSOP", b"Alpha")]);
    let report = inspect_id3_bytes(&modern).unwrap();
    assert_eq!(mixed_v23_detail(&report), None);
}

/// Minimal synthetic ID3v2 tag: header plus text frames with empty flags.
fn id3v2_bytes(major: u8, frames: &[(&str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (id, payload) in frames {
        // 1 encoding byte + payload.
        let size = payload.len() + 1;
        body.extend_from_slice(id.as_bytes());
        if major == 4 {
            body.extend_from_slice(&[
                ((size >> 21) & 0x7F) as u8,
                ((size >> 14) & 0x7F) as u8,
                ((size >> 7) & 0x7F) as u8,
                (size & 0x7F) as u8,
            ]);
        } else {
            body.extend_from_slice(&(size as u32).to_be_bytes());
        }
        body.extend_from_slice(&[0, 0]);
        body.push(3);
        body.extend_from_slice(payload);
    }
    let size = body.len();
    let mut out = vec![b'I', b'D', b'3', major, 0, 0];
    out.extend_from_slice(&[
        ((size >> 21) & 0x7F) as u8,
        ((size >> 14) & 0x7F) as u8,
        ((size >> 7) & 0x7F) as u8,
        (size & 0x7F) as u8,
    ]);
    out.extend_from_slice(&body);
    out
}

// ---------------------------------------------------------------------------
// Credits, work, comment and AcoustID fields
// ---------------------------------------------------------------------------

/// Every ID3 frame of a file in order: id and payload, read from the
/// bytes.
pub(crate) fn id3_frames(path: &Path) -> (u8, Vec<(String, Vec<u8>)>) {
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[..3], b"ID3");
    let major = bytes[3];
    let syncsafe = |part: &[u8]| {
        part.iter()
            .fold(0usize, |acc, byte| (acc << 7) | usize::from(byte & 0x7F))
    };
    let end = 10 + syncsafe(&bytes[6..10]);
    let mut frames = Vec::new();
    let mut offset = 10;
    while offset + 10 <= end && bytes[offset] != 0 {
        let id = String::from_utf8(bytes[offset..offset + 4].to_vec()).unwrap();
        let size = if major == 4 {
            syncsafe(&bytes[offset + 4..offset + 8])
        } else {
            u32::from_be_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize
        };
        frames.push((id, bytes[offset + 10..offset + 10 + size].to_vec()));
        offset += 10 + size;
    }
    (major, frames)
}

/// The strings of an ID3 text payload (encoding byte first), without the
/// trailing terminator.
pub(crate) fn id3_strings(payload: &[u8]) -> Vec<String> {
    let (encoding, body) = payload.split_first().unwrap();
    let mut out: Vec<String> = match encoding {
        0 => body
            .split(|byte| *byte == 0)
            .map(|part| part.iter().map(|byte| char::from(*byte)).collect())
            .collect(),
        3 => body
            .split(|byte| *byte == 0)
            .map(|part| String::from_utf8(part.to_vec()).unwrap())
            .collect(),
        1 | 2 => {
            let units: Vec<u16> = body
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            // A string with no byte-order mark keeps the one before's.
            let mut order = false;
            units
                .split(|unit| *unit == 0)
                .map(|part| {
                    let (part, swap) = match part.first() {
                        Some(0xFEFF) => (&part[1..], false),
                        Some(0xFFFE) => (&part[1..], true),
                        _ => (part, order),
                    };
                    order = swap;
                    let units: Vec<u16> = part
                        .iter()
                        .map(|unit| if swap { unit.swap_bytes() } else { *unit })
                        .collect();
                    String::from_utf16(&units).unwrap()
                })
                .collect()
        }
        other => panic!("encoding {other}"),
    };
    while out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}

/// The values of one text frame.
pub(crate) fn id3_text_values(path: &Path, id: &str) -> Vec<String> {
    id3_frames(path)
        .1
        .iter()
        .find(|(frame, _)| frame == id)
        .map(|(_, payload)| id3_strings(payload))
        .unwrap_or_default()
}

/// The values of the `TXXX` frame with this description.
pub(crate) fn id3_txxx_values(path: &Path, description: &str) -> Vec<String> {
    id3_frames(path)
        .1
        .iter()
        .filter(|(id, _)| id == "TXXX")
        .map(|(_, payload)| id3_strings(payload))
        .find(|strings| strings.first().map(String::as_str) == Some(description))
        .map(|strings| strings[1..].to_vec())
        .unwrap_or_default()
}

/// The role and name pairs of a `TIPL`, `TMCL` or `IPLS` frame.
pub(crate) fn id3_people(path: &Path, id: &str) -> Vec<(String, String)> {
    let frames: Vec<Vec<u8>> = id3_frames(path)
        .1
        .into_iter()
        .filter(|(frame, _)| frame == id)
        .map(|(_, payload)| payload)
        .collect();
    assert!(frames.len() <= 1, "several {id} frames");
    frames
        .first()
        .map(|payload| {
            id3_strings(payload)
                .chunks(2)
                .map(|pair| (pair[0].clone(), pair[1].clone()))
                .collect()
        })
        .unwrap_or_default()
}

/// Every `COMM` frame: language, description, text.
pub(crate) fn id3_comments(path: &Path) -> Vec<(String, String, String)> {
    id3_frames(path)
        .1
        .iter()
        .filter(|(id, _)| id == "COMM")
        .map(|(_, payload)| {
            let language = String::from_utf8(payload[1..4].to_vec()).unwrap();
            let mut rest = vec![payload[0]];
            rest.extend_from_slice(&payload[4..]);
            let mut strings = id3_strings(&rest);
            strings.resize(2, String::new());
            (language, strings[0].clone(), strings[1].clone())
        })
        .collect()
}

/// A raw ID3v2.3 tag holding these frames in this order.
pub(crate) fn id3v23_raw(frames: &[(&str, &[u8])]) -> Vec<u8> {
    let mut body = Vec::new();
    for (id, payload) in frames {
        body.extend_from_slice(id.as_bytes());
        body.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(payload);
    }
    let size = body.len();
    let mut out = vec![b'I', b'D', b'3', 3, 0, 0];
    out.extend_from_slice(&[
        ((size >> 21) & 0x7F) as u8,
        ((size >> 14) & 0x7F) as u8,
        ((size >> 7) & 0x7F) as u8,
        (size & 0x7F) as u8,
    ]);
    out.extend_from_slice(&body);
    out
}

/// A UTF-16 people payload: each string big-endian with its own
/// byte-order mark (Picard, mutagen), or, as lofty 0.25's v2.3 writer
/// lays one out, a single little-endian mark at the start.
fn utf16_people(pairs: &[(&str, &str)], one_mark: bool) -> Vec<u8> {
    let mut payload = vec![1];
    for (index, part) in pairs
        .iter()
        .flat_map(|(role, name)| [*role, *name])
        .enumerate()
    {
        if one_mark {
            if index == 0 {
                payload.extend_from_slice(&[0xFF, 0xFE]);
            }
            for unit in part.encode_utf16() {
                payload.extend_from_slice(&unit.to_le_bytes());
            }
        } else {
            payload.extend_from_slice(&[0xFE, 0xFF]);
            for unit in part.encode_utf16() {
                payload.extend_from_slice(&unit.to_be_bytes());
            }
        }
        payload.extend_from_slice(&[0, 0]);
    }
    payload
}

/// A v2.3 file holding these frames over the fixture's audio.
fn v23_file(test: &str, frames: &[(&str, &[u8])]) -> (ScratchDir, PathBuf) {
    let audio = mp3_audio_tail(&std::fs::read(fixture("management_full.mp3")).unwrap()).to_vec();
    let mut bytes = id3v23_raw(frames);
    bytes.extend_from_slice(&audio);
    let dir = ScratchDir::new(test);
    let path = dir.join("v23.mp3");
    std::fs::write(&path, &bytes).unwrap();
    (dir, path)
}

/// A raw ID3v2.4 tag holding these frames in this order.
fn id3v24_raw(frames: &[(&str, &[u8])]) -> Vec<u8> {
    let syncsafe = |len: usize| {
        [
            ((len >> 21) & 0x7F) as u8,
            ((len >> 14) & 0x7F) as u8,
            ((len >> 7) & 0x7F) as u8,
            (len & 0x7F) as u8,
        ]
    };
    let mut body = Vec::new();
    for (id, payload) in frames {
        body.extend_from_slice(id.as_bytes());
        body.extend_from_slice(&syncsafe(payload.len()));
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(payload);
    }
    let mut out = vec![b'I', b'D', b'3', 4, 0, 0];
    out.extend_from_slice(&syncsafe(body.len()));
    out.extend_from_slice(&body);
    out
}

/// The packets of the first logical stream of an Ogg file.
fn ogg_packets(bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut packets = Vec::new();
    let mut assembly = Vec::new();
    let mut serial = None;
    let mut offset = 0;
    while offset + 27 <= bytes.len() {
        assert_eq!(&bytes[offset..offset + 4], b"OggS");
        let page_serial = u32::from_le_bytes(bytes[offset + 14..offset + 18].try_into().unwrap());
        let segments = bytes[offset + 26] as usize;
        let table = &bytes[offset + 27..offset + 27 + segments];
        let mut data = offset + 27 + segments;
        let ours = *serial.get_or_insert(page_serial) == page_serial;
        for len in table {
            let len = *len as usize;
            if ours {
                assembly.extend_from_slice(&bytes[data..data + len]);
                if len < 255 {
                    packets.push(std::mem::take(&mut assembly));
                }
            }
            data += len;
        }
        offset = data;
    }
    packets
}

/// A Vorbis comment payload's pairs in order.
fn vorbis_comment_pairs(payload: &[u8]) -> Vec<(String, String)> {
    let word = |at: usize| u32::from_le_bytes(payload[at..at + 4].try_into().unwrap()) as usize;
    let mut cursor = 4 + word(0);
    let count = word(cursor);
    cursor += 4;
    let mut out = Vec::new();
    for _ in 0..count {
        let len = word(cursor);
        cursor += 4;
        let pair = std::str::from_utf8(&payload[cursor..cursor + len]).unwrap();
        cursor += len;
        let (key, value) = pair.split_once('=').unwrap();
        out.push((key.to_owned(), value.to_owned()));
    }
    out
}

/// Every Vorbis comment pair of a FLAC, Ogg Vorbis or Opus file in order.
fn vorbis_pairs(path: &Path) -> Vec<(String, String)> {
    let bytes = std::fs::read(path).unwrap();
    if &bytes[..4] == b"fLaC" {
        let mut offset = 4;
        loop {
            let header = bytes[offset];
            let len =
                u32::from_be_bytes([0, bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
                    as usize;
            if header & 0x7F == 4 {
                return vorbis_comment_pairs(&bytes[offset + 4..offset + 4 + len]);
            }
            offset += 4 + len;
            assert_eq!(header & 0x80, 0, "comment block missing");
        }
    }
    let packet = &ogg_packets(&bytes)[1];
    let magic = if packet.starts_with(b"OpusTags") {
        8
    } else {
        7
    };
    vorbis_comment_pairs(&packet[magic..])
}

pub(crate) fn vorbis_key_values(path: &Path, key: &str) -> Vec<String> {
    vorbis_pairs(path)
        .into_iter()
        .filter(|(held, _)| held.eq_ignore_ascii_case(key))
        .map(|(_, value)| value)
        .collect()
}

/// The audio of any writable fixture: what follows the tags.
fn audio_bytes(path: &Path) -> Vec<u8> {
    let bytes = std::fs::read(path).unwrap();
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("flac") => flac_audio_tail(&bytes).to_vec(),
        Some("mp3") => mp3_audio_tail(&bytes).to_vec(),
        Some("m4a") => m4a_mdat(&bytes).to_vec(),
        // Ogg: every packet after the comment packet (the Vorbis setup
        // header and the audio).
        _ => ogg_packets(&bytes)[2..].concat(),
    }
}

/// An MP4 item compared whole: kind, freeform name, data payloads.
type AtomView = (Vec<u8>, Option<String>, Vec<Vec<u8>>);

/// One MP4 `ilst` item: its kind, freeform name, and data payloads with
/// their data types.
struct Mp4Atom {
    kind: [u8; 4],
    name: Option<String>,
    data: Vec<Vec<u8>>,
    types: Vec<u32>,
}

fn mp4_boxes(bytes: &[u8], start: usize, end: usize) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut offset = start;
    while offset + 8 <= end {
        let size = u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) as usize;
        let kind: [u8; 4] = bytes[offset + 4..offset + 8].try_into().unwrap();
        out.push((kind, offset + 8, offset + size));
        offset += size;
    }
    out
}

fn mp4_atoms(path: &Path) -> Vec<Mp4Atom> {
    let bytes = std::fs::read(path).unwrap();
    let find = |start: usize, end: usize, kind: &[u8; 4]| {
        mp4_boxes(&bytes, start, end)
            .into_iter()
            .find(|(held, _, _)| held == kind)
            .map(|(_, content, end)| (content, end))
            .unwrap()
    };
    let (start, end) = find(0, bytes.len(), b"moov");
    let (start, end) = find(start, end, b"udta");
    let (start, end) = find(start, end, b"meta");
    let (start, end) = find(start + 4, end, b"ilst");
    mp4_boxes(&bytes, start, end)
        .into_iter()
        .map(|(kind, content, end)| {
            let mut name = None;
            let mut data = Vec::new();
            let mut types = Vec::new();
            for (child, inner, child_end) in mp4_boxes(&bytes, content, end) {
                match &child {
                    b"name" => {
                        name =
                            Some(String::from_utf8(bytes[inner + 4..child_end].to_vec()).unwrap());
                    }
                    b"data" => {
                        data.push(bytes[inner + 8..child_end].to_vec());
                        types.push(u32::from_be_bytes(
                            bytes[inner..inner + 4].try_into().unwrap(),
                        ));
                    }
                    _ => {}
                }
            }
            Mp4Atom {
                kind,
                name,
                data,
                types,
            }
        })
        .collect()
}

pub(crate) fn mp4_text(path: &Path, kind: &[u8; 4]) -> Vec<String> {
    mp4_atoms(path)
        .into_iter()
        .filter(|atom| &atom.kind == kind)
        .flat_map(|atom| atom.data)
        .map(|data| String::from_utf8(data).unwrap())
        .collect()
}

pub(crate) fn mp4_freeform(path: &Path, name: &str) -> Vec<String> {
    mp4_atoms(path)
        .into_iter()
        .filter(|atom| &atom.kind == b"----" && atom.name.as_deref() == Some(name))
        .flat_map(|atom| atom.data)
        .map(|data| String::from_utf8(data).unwrap())
        .collect()
}

/// A big-endian integer atom's values.
fn mp4_integers(path: &Path, kind: &[u8; 4]) -> Vec<u64> {
    mp4_atoms(path)
        .into_iter()
        .filter(|atom| &atom.kind == kind)
        .flat_map(|atom| atom.data)
        .map(|data| {
            data.iter()
                .fold(0u64, |acc, byte| (acc << 8) | u64::from(*byte))
        })
        .collect()
}

fn texts(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

fn pair(role: &str, name: &str) -> (String, String) {
    (role.to_owned(), name.to_owned())
}

const PERFORMERS: &[&str] = &[
    "A (guitar)",
    "B (lead vocals)",
    "C",
    "A (drums (drum set))",
    "Sunn O))) (guitar)",
];

/// The pairs [`PERFORMERS`] are written as: instrument, then name.
fn performer_pairs() -> Vec<(String, String)> {
    vec![
        pair("guitar", "A"),
        pair("lead vocals", "B"),
        pair("", "C"),
        pair("drums (drum set)", "A"),
        pair("guitar", "Sunn O)))"),
    ]
}

/// One value set for every field beyond the release facts.
fn credit_edits() -> Vec<TagEdit> {
    let many = |field: TagField, values: &[&str]| TagEdit::new(field, texts(values));
    vec![
        many(TagField::AlbumArtists, &["The Lanterns", "Guest"]),
        many(TagField::Work, &["Night Suite"]),
        many(
            TagField::MusicBrainzWorkId,
            &["c0ffee00-0000-4000-8000-00000000f001"],
        ),
        many(TagField::Movement, &["Allegro"]),
        many(TagField::MovementNumber, &["2"]),
        many(TagField::MovementCount, &["4"]),
        many(TagField::Composer, &["Ada Composer", "Ben Composer"]),
        many(TagField::Lyricist, &["Lyra Writer"]),
        many(TagField::Conductor, &["Carl Conductor"]),
        many(TagField::Performer, PERFORMERS),
        many(TagField::Arranger, &["Arlo Arranger"]),
        many(TagField::Remixer, &["Rémi Mixer"]),
        many(TagField::Producer, &["Pia Producer", "Pete Producer"]),
        many(TagField::Comment, &["A comment, with ünïcode"]),
        many(TagField::Copyright, &["2024 Night Bus"]),
        many(TagField::EncodedBy, &["DroppedNeedle"]),
        many(
            TagField::AcoustIdId,
            &["c0ffee00-0000-4000-8000-0000000a0001"],
        ),
        many(TagField::AcoustIdFingerprint, &["AQADtEmUaEkSRUkS"]),
    ]
}

fn value_of(edits: &[TagEdit], field: TagField) -> Vec<String> {
    edits
        .iter()
        .find(|edit| edit.field == field)
        .unwrap()
        .values
        .clone()
}

/// Saves `edits` and checks each lands where `check` reads it from the
/// bytes; a later title edit leaves them all as they are, and the audio
/// stays byte for byte.
fn credits_round_trip(path: &Path, edits: &[TagEdit], check: &dyn Fn(&Path)) {
    let name = path.display().to_string();
    let audio = audio_bytes(path);
    tags::save_tags(path, edits).unwrap_or_else(|error| panic!("{name}: {error}"));
    check(path);
    let fields = tags::read_fields(path).unwrap();
    tags::save_tags(path, &[TagEdit::set_title("Later")])
        .unwrap_or_else(|error| panic!("{name}: {error}"));
    check(path);
    let after = tags::read_fields(path).unwrap();
    for edit in edits {
        assert_eq!(after.get(&edit.field), fields.get(&edit.field), "{name}");
    }
    assert_eq!(audio_bytes(path), audio, "{name}: audio changed");
}

#[test]
fn credit_fields_land_in_vorbis_comments() {
    for name in [
        "management_full.flac",
        "management_full.ogg",
        "management_full.opus",
    ] {
        let (_scratch, path) = temp_copy(name, "credits-vorbis");
        let edits = credit_edits();
        let check = |path: &Path| {
            for (key, field) in [
                ("ALBUMARTISTS", TagField::AlbumArtists),
                ("WORK", TagField::Work),
                ("MUSICBRAINZ_WORKID", TagField::MusicBrainzWorkId),
                ("MOVEMENTNAME", TagField::Movement),
                ("MOVEMENT", TagField::MovementNumber),
                ("MOVEMENTTOTAL", TagField::MovementCount),
                ("COMPOSER", TagField::Composer),
                ("LYRICIST", TagField::Lyricist),
                ("CONDUCTOR", TagField::Conductor),
                ("PERFORMER", TagField::Performer),
                ("ARRANGER", TagField::Arranger),
                ("REMIXER", TagField::Remixer),
                ("PRODUCER", TagField::Producer),
                ("COMMENT", TagField::Comment),
                ("COPYRIGHT", TagField::Copyright),
                ("ENCODEDBY", TagField::EncodedBy),
                ("ACOUSTID_ID", TagField::AcoustIdId),
                ("ACOUSTID_FINGERPRINT", TagField::AcoustIdFingerprint),
            ] {
                assert_eq!(
                    vorbis_key_values(path, key),
                    value_of(&edits, field),
                    "{name}: {key}"
                );
            }
            assert!(vorbis_key_values(path, "ENCODED-BY").is_empty(), "{name}");
        };
        credits_round_trip(&path, &edits, &check);
        let fields = tags::read_fields(&path).unwrap();
        for edit in &edits {
            assert_eq!(fields.get(&edit.field), Some(&edit.values), "{name}");
        }
    }
}

#[test]
fn credit_fields_land_in_id3v24() {
    let (_scratch, path) = temp_copy("management_full.mp3", "credits-v24");
    let edits = credit_edits();
    let check = |path: &Path| {
        assert_eq!(id3_frames(path).0, 4);
        for (description, field) in [
            ("ALBUMARTISTS", TagField::AlbumArtists),
            ("WORK", TagField::Work),
            ("MusicBrainz Work Id", TagField::MusicBrainzWorkId),
            ("Acoustid Id", TagField::AcoustIdId),
            ("Acoustid Fingerprint", TagField::AcoustIdFingerprint),
        ] {
            assert_eq!(
                id3_txxx_values(path, description),
                value_of(&edits, field),
                "{description}"
            );
        }
        for (id, field) in [
            ("MVNM", TagField::Movement),
            ("TCOM", TagField::Composer),
            ("TEXT", TagField::Lyricist),
            ("TPE3", TagField::Conductor),
            ("TPE4", TagField::Remixer),
            ("TCOP", TagField::Copyright),
            ("TENC", TagField::EncodedBy),
        ] {
            assert_eq!(id3_text_values(path, id), value_of(&edits, field), "{id}");
        }
        assert!(id3_frames(path).1.iter().all(|(id, _)| id != "TIT1"));
        assert_eq!(id3_text_values(path, "MVIN"), texts(&["2/4"]));
        assert_eq!(id3_people(path, "TMCL"), performer_pairs());
        // The fixture's producer pair is replaced in place; the arranger
        // pair is added after it.
        assert_eq!(
            id3_people(path, "TIPL"),
            vec![
                pair("producer", "Pia Producer"),
                pair("producer", "Pete Producer"),
                pair("arranger", "Arlo Arranger"),
            ]
        );
        assert_eq!(
            id3_comments(path),
            vec![(
                "eng".to_owned(),
                String::new(),
                "A comment, with ünïcode".to_owned()
            )]
        );
    };
    credits_round_trip(&path, &edits, &check);
    let fields = tags::read_fields(&path).unwrap();
    for edit in &edits {
        assert_eq!(
            fields.get(&edit.field),
            Some(&edit.values),
            "{:?}",
            edit.field
        );
    }
}

/// A pure ID3v2.3 tag over the fixture's audio.
fn pure_v23_copy(test: &str) -> (ScratchDir, PathBuf) {
    let audio = mp3_audio_tail(&std::fs::read(fixture("management_full.mp3")).unwrap()).to_vec();
    let mut tag = id3v2_bytes(
        3,
        &[
            ("TIT2", b"Management Track"),
            ("TPE1", b"Alpha feat. Beta"),
            ("TYER", b"2024"),
            ("TRCK", b"2"),
        ],
    );
    tag.extend_from_slice(&audio);
    let dir = ScratchDir::new(test);
    let path = dir.join("pure_v23.mp3");
    std::fs::write(&path, &tag).unwrap();
    (dir, path)
}

/// lofty 0.25's v2.3 writer keeps `MVNM`, `MVIN`, `TCOP`, `TENC`, `TEXT`,
/// `TPE3`, `TPE4` and `TCOM`, and the wrapper builds `IPLS` and `COMM`
/// itself: every field lands in its Picard home and the tag stays a
/// clean v2.3 tag.
#[test]
fn credit_fields_land_in_id3v23() {
    let (_scratch, path) = pure_v23_copy("tags-credits-v23");
    let edits = credit_edits();
    let check = |path: &Path| {
        assert_eq!(id3_frames(path).0, 3);
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(mixed_v23_detail(&inspect_id3_bytes(&bytes).unwrap()), None);
        // v2.3 holds one text value per frame: several are joined with '/'.
        let joined = |field: TagField| vec![value_of(&edits, field).join("/")];
        for (description, field) in [
            ("ALBUMARTISTS", TagField::AlbumArtists),
            ("WORK", TagField::Work),
            ("MusicBrainz Work Id", TagField::MusicBrainzWorkId),
            ("Acoustid Id", TagField::AcoustIdId),
            ("Acoustid Fingerprint", TagField::AcoustIdFingerprint),
        ] {
            assert_eq!(
                id3_txxx_values(path, description),
                joined(field),
                "{description}"
            );
        }
        for (id, field) in [
            ("MVNM", TagField::Movement),
            ("TCOM", TagField::Composer),
            ("TEXT", TagField::Lyricist),
            ("TPE3", TagField::Conductor),
            ("TPE4", TagField::Remixer),
            ("TCOP", TagField::Copyright),
            ("TENC", TagField::EncodedBy),
        ] {
            assert_eq!(id3_text_values(path, id), joined(field), "{id}");
        }
        assert_eq!(id3_text_values(path, "MVIN"), texts(&["2/4"]));
        let mut ipls = performer_pairs();
        ipls.extend([
            pair("arranger", "Arlo Arranger"),
            pair("producer", "Pia Producer"),
            pair("producer", "Pete Producer"),
        ]);
        assert_eq!(id3_people(path, "IPLS"), ipls);
        assert_eq!(
            id3_comments(path),
            vec![(
                "eng".to_owned(),
                String::new(),
                "A comment, with ünïcode".to_owned()
            )]
        );
    };
    credits_round_trip(&path, &edits, &check);
    let fields = tags::read_fields(&path).unwrap();
    for field in [
        TagField::Performer,
        TagField::Arranger,
        TagField::Producer,
        TagField::Comment,
        TagField::MovementNumber,
        TagField::MovementCount,
    ] {
        assert_eq!(fields[&field], value_of(&edits, field), "{field:?}");
    }
}

#[test]
fn credit_fields_land_in_mp4_atoms() {
    let (_scratch, path) = temp_copy("management_full.m4a", "credits-m4a");
    let edits: Vec<TagEdit> = credit_edits()
        .into_iter()
        .filter(|edit| !matches!(edit.field, TagField::Performer | TagField::Arranger))
        .collect();
    let check = |path: &Path| {
        for (kind, field) in [
            (b"\xa9wrk", TagField::Work),
            (b"\xa9mvn", TagField::Movement),
            (b"\xa9wrt", TagField::Composer),
            (b"\xa9cmt", TagField::Comment),
            (b"cprt", TagField::Copyright),
            (b"\xa9too", TagField::EncodedBy),
        ] {
            assert_eq!(mp4_text(path, kind), value_of(&edits, field), "{field:?}");
        }
        for (name, field) in [
            ("ALBUMARTISTS", TagField::AlbumArtists),
            ("MusicBrainz Work Id", TagField::MusicBrainzWorkId),
            ("LYRICIST", TagField::Lyricist),
            ("CONDUCTOR", TagField::Conductor),
            ("REMIXER", TagField::Remixer),
            ("PRODUCER", TagField::Producer),
            ("Acoustid Id", TagField::AcoustIdId),
            ("Acoustid Fingerprint", TagField::AcoustIdFingerprint),
        ] {
            assert_eq!(mp4_freeform(path, name), value_of(&edits, field), "{name}");
        }
        assert_eq!(mp4_integers(path, b"\xa9mvi"), vec![2]);
        assert_eq!(mp4_integers(path, b"\xa9mvc"), vec![4]);
    };
    credits_round_trip(&path, &edits, &check);
    let fields = tags::read_fields(&path).unwrap();
    for edit in &edits {
        assert_eq!(
            fields.get(&edit.field),
            Some(&edit.values),
            "{:?}",
            edit.field
        );
    }
}

/// Musician credits round-trip with their instruments; MP4 has no home
/// for performers or arrangers and refuses them, with nothing to remove.
#[test]
fn performers_round_trip_and_mp4_refuses() {
    let performers = TagEdit::new(TagField::Performer, texts(PERFORMERS));
    let (_v24, mp3) = temp_copy("management_full.mp3", "performers");
    let (_v23, v23) = pure_v23_copy("tags-performers-v23");
    let (_flac, flac) = temp_copy("management_full.flac", "performers");
    for path in [&mp3, &v23, &flac] {
        tags::save_tags(path, std::slice::from_ref(&performers)).unwrap();
        assert_eq!(
            tags::read_fields(path).unwrap()[&TagField::Performer],
            texts(PERFORMERS),
            "{}",
            path.display()
        );
    }
    assert_eq!(vorbis_key_values(&flac, "PERFORMER"), texts(PERFORMERS));
    assert_eq!(id3_people(&mp3, "TMCL"), performer_pairs());
    assert_eq!(id3_people(&v23, "IPLS"), performer_pairs());

    for edit in [
        performers,
        TagEdit::new(TagField::Arranger, texts(&["Arlo Arranger"])),
    ] {
        let (refusal, before, after) = save_refused("management_full.m4a", "credit-m4a", &[edit]);
        assert!(
            matches!(refusal, Refusal::UnencodableItem { .. }),
            "{refusal:?}"
        );
        assert_eq!(before, after);
    }
    let (_m4a, m4a) = temp_copy("management_full.m4a", "performer-clear");
    tags::save_tags(&m4a, &[TagEdit::new(TagField::Performer, Vec::new())]).unwrap();
}

/// The fixtures v2's generator tagged read their credits, work and
/// AcoustID values under the fields, no longer as native items.
#[test]
fn management_fixtures_read_their_credits() {
    let id = |n: u8| format!("10000000-0000-4000-8000-00000000000{n}");
    for name in [
        "management_full.mp3",
        "management_full.flac",
        "management_full.ogg",
        "management_full.opus",
        "management_full.m4a",
    ] {
        let document = tags::save::read_document(&fixture(name)).unwrap();
        let get = |field: TagField| document.values.get(&field).cloned().unwrap_or_default();
        // The MP4 fixture carries no composer.
        if name != "management_full.m4a" {
            assert_eq!(
                get(TagField::Composer),
                texts(&["Example Composer"]),
                "{name}"
            );
        }
        assert_eq!(
            get(TagField::Lyricist),
            texts(&["Example Lyricist"]),
            "{name}"
        );
        assert_eq!(
            get(TagField::Producer),
            texts(&["Example Producer"]),
            "{name}"
        );
        assert_eq!(get(TagField::Work), texts(&["Example Work"]), "{name}");
        assert_eq!(get(TagField::MusicBrainzWorkId), vec![id(7)], "{name}");
        assert_eq!(get(TagField::Isrc), texts(&["GBABC2400001"]), "{name}");
        assert_eq!(get(TagField::AcoustIdId), vec![id(8)], "{name}");
        assert_eq!(
            get(TagField::AcoustIdFingerprint),
            texts(&["AQAD-management-fingerprint"]),
            "{name}"
        );
        for key in document.native.keys() {
            let folded = key.to_ascii_lowercase();
            assert!(
                ["composer", "lyricist", "tcom", "text", "work", "acoustid"]
                    .iter()
                    .all(|owned| !folded.ends_with(owned)),
                "{name}: {key} is still native"
            );
        }
    }
}

/// Edits of producer, comment and composer leave a foreign `TIPL` pair,
/// an iTunNORM `COMM`, an unknown `TXXX`, an unknown Vorbis key and an
/// unknown MP4 freeform as they were, in order.
#[test]
fn credit_edits_leave_neighbours_alone() {
    let edits = [
        TagEdit::new(TagField::Producer, texts(&["New Producer"])),
        TagEdit::new(TagField::Comment, texts(&["New comment"])),
        TagEdit::new(TagField::Composer, texts(&["New Composer"])),
    ];

    // MP3: the fixture's TIPL swapped for one with neighbours, plus an
    // iTunNORM comment and an unknown TXXX.
    let (_scratch, mp3) = temp_copy("management_full.mp3", "neighbours");
    let audio = mp3_audio_tail(&std::fs::read(&mp3).unwrap()).to_vec();
    let mut frames: Vec<(String, Vec<u8>)> = id3_frames(&mp3)
        .1
        .into_iter()
        .filter(|(id, _)| id != "TIPL")
        .collect();
    frames.push((
        "TIPL".to_owned(),
        b"\x03engineer\x00Eve Engineer\x00producer\x00Example Producer\x00mix\x00Max Mixer\x00"
            .to_vec(),
    ));
    let itunnorm = b"\x00engiTunNORM\x00 00000A1B 00000C2D".to_vec();
    frames.push(("COMM".to_owned(), itunnorm.clone()));
    frames.push(("TXXX".to_owned(), b"\x03ZZZ_UNHEARD_OF\x00mystery".to_vec()));
    let refs: Vec<(&str, &[u8])> = frames
        .iter()
        .map(|(id, payload)| (id.as_str(), payload.as_slice()))
        .collect();
    let mut bytes = id3v24_raw(&refs);
    bytes.extend_from_slice(&audio);
    std::fs::write(&mp3, &bytes).unwrap();
    let untouched = |path: &Path| {
        let mut frames: Vec<(String, Vec<String>)> = id3_frames(path)
            .1
            .into_iter()
            .filter(|(id, _)| !matches!(id.as_str(), "TIPL" | "COMM" | "TCOM"))
            .map(|(id, payload)| {
                let decoded = if id.starts_with('T') {
                    id3_strings(&payload)
                } else {
                    vec![format!("{payload:?}")]
                };
                (id, decoded)
            })
            .collect();
        frames.sort();
        frames
    };
    let before = untouched(&mp3);
    tags::save_tags(&mp3, &edits).unwrap();
    assert_eq!(
        id3_people(&mp3, "TIPL"),
        vec![
            pair("engineer", "Eve Engineer"),
            pair("producer", "New Producer"),
            pair("mix", "Max Mixer"),
        ]
    );
    let comments: Vec<Vec<u8>> = id3_frames(&mp3)
        .1
        .into_iter()
        .filter(|(id, _)| id == "COMM")
        .map(|(_, payload)| payload)
        .collect();
    assert_eq!(comments.len(), 2);
    assert!(comments.contains(&itunnorm), "iTunNORM changed");
    assert!(id3_comments(&mp3).contains(&(
        "eng".to_owned(),
        String::new(),
        "New comment".to_owned()
    )));
    assert_eq!(id3_text_values(&mp3, "TCOM"), texts(&["New Composer"]));
    assert_eq!(id3_txxx_values(&mp3, "ZZZ_UNHEARD_OF"), texts(&["mystery"]));
    assert_eq!(untouched(&mp3), before);
    assert_eq!(audio_bytes(&mp3), audio);

    // FLAC: an unknown key beside CUSTOM_KEEP; every untouched pair keeps
    // its place.
    let (_scratch, flac) = temp_copy("management_full.flac", "neighbours");
    flac_inject_pairs(&flac, &[("ZZZ_UNHEARD_OF_FIELD", "mystery")]);
    let edited = ["PRODUCER", "COMMENT", "COMPOSER"];
    let rest = |path: &Path| -> Vec<(String, String)> {
        vorbis_pairs(path)
            .into_iter()
            .filter(|(key, _)| !edited.contains(&key.as_str()))
            .collect()
    };
    let before = rest(&flac);
    tags::save_tags(&flac, &edits).unwrap();
    assert_eq!(rest(&flac), before);
    assert_eq!(
        vorbis_key_values(&flac, "PRODUCER"),
        texts(&["New Producer"])
    );
    assert_eq!(vorbis_key_values(&flac, "COMMENT"), texts(&["New comment"]));

    // M4A: the fixture's unknown freeform and every other atom stay, in
    // their order.
    let (_scratch, m4a) = temp_copy("management_full.m4a", "neighbours");
    let rest = |path: &Path| -> Vec<AtomView> {
        mp4_atoms(path)
            .into_iter()
            .filter(|atom| {
                !(atom.kind == *b"\xa9cmt"
                    || atom.kind == *b"\xa9wrt"
                    || atom.name.as_deref() == Some("PRODUCER"))
            })
            .map(|atom| {
                // Number pairs lose their padding and integers their
                // width on any save; compare those by value.
                let data = atom
                    .data
                    .into_iter()
                    .map(|data| match &atom.kind {
                        b"trkn" | b"disk" => data[..6].to_vec(),
                        b"stik" | b"cpil" => data
                            .iter()
                            .fold(0u64, |acc, byte| (acc << 8) | u64::from(*byte))
                            .to_be_bytes()
                            .to_vec(),
                        _ => data,
                    })
                    .collect();
                (atom.kind.to_vec(), atom.name, data)
            })
            .collect()
    };
    let before = rest(&m4a);
    tags::save_tags(&m4a, &edits).unwrap();
    assert_eq!(rest(&m4a), before);
    assert_eq!(
        mp4_freeform(&m4a, "CUSTOM_KEEP"),
        texts(&["opaque local value"])
    );
    assert_eq!(mp4_freeform(&m4a, "PRODUCER"), texts(&["New Producer"]));
}

/// Engineer, mixer, DJ-mixer, writer and director land in Picard's home
/// for each format and read back as written; a later title edit leaves
/// them, and the audio, as they were.
#[test]
fn other_role_fields_land_in_their_homes() {
    let many = |field: TagField, values: &[&str]| TagEdit::new(field, texts(values));
    let edits = vec![
        many(TagField::Engineer, &["Eve Engineer", "Ed Engineer"]),
        many(TagField::Mixer, &["Max Mixer"]),
        many(TagField::DjMixer, &["Dee Jay"]),
        many(TagField::Writer, &["Wanda Writer", "Will Writer"]),
        many(TagField::Director, &["Dora Director"]),
    ];
    let read_back = |path: &Path| {
        let fields = tags::read_fields(path).unwrap();
        for edit in &edits {
            assert_eq!(
                fields.get(&edit.field),
                Some(&edit.values),
                "{}: {:?}",
                path.display(),
                edit.field
            );
        }
    };
    let people = |role_of: &dyn Fn(TagField) -> &'static str| {
        let mut pairs = Vec::new();
        for field in [TagField::Engineer, TagField::Mixer, TagField::DjMixer] {
            for name in value_of(&edits, field) {
                pairs.push((role_of(field).to_owned(), name));
            }
        }
        pairs
    };
    let role_of = |field: TagField| match field {
        TagField::Engineer => "engineer",
        TagField::Mixer => "mix",
        _ => "DJ-mix",
    };

    for name in ["management_full.flac", "management_full.ogg"] {
        let (_scratch, path) = temp_copy(name, "other-roles-vorbis");
        let check = |path: &Path| {
            for (key, field) in [
                ("ENGINEER", TagField::Engineer),
                ("MIXER", TagField::Mixer),
                ("DJMIXER", TagField::DjMixer),
                ("WRITER", TagField::Writer),
                ("DIRECTOR", TagField::Director),
            ] {
                assert_eq!(
                    vorbis_key_values(path, key),
                    value_of(&edits, field),
                    "{name}: {key}"
                );
            }
        };
        credits_round_trip(&path, &edits, &check);
        read_back(&path);
    }

    let (_scratch, mp3) = temp_copy("management_full.mp3", "other-roles-v24");
    let check = |path: &Path| {
        assert_eq!(id3_frames(path).0, 4);
        let mut tipl = vec![pair("producer", "Example Producer")];
        tipl.extend(people(&role_of));
        assert_eq!(id3_people(path, "TIPL"), tipl);
        assert_eq!(
            id3_txxx_values(path, "Writer"),
            value_of(&edits, TagField::Writer)
        );
        assert_eq!(
            id3_txxx_values(path, "DIRECTOR"),
            value_of(&edits, TagField::Director)
        );
    };
    credits_round_trip(&mp3, &edits, &check);
    read_back(&mp3);

    // ID3v2.3: the pairs in IPLS, and writer as one "/"-joined value.
    let (_scratch, v23) = pure_v23_copy("other-roles-v23");
    let check = |path: &Path| {
        assert_eq!(id3_frames(path).0, 3);
        assert_eq!(id3_people(path, "IPLS"), people(&role_of));
        assert_eq!(
            id3_txxx_values(path, "Writer"),
            texts(&["Wanda Writer/Will Writer"])
        );
        assert_eq!(
            id3_txxx_values(path, "DIRECTOR"),
            value_of(&edits, TagField::Director)
        );
    };
    credits_round_trip(&v23, &edits, &check);

    let (_scratch, m4a) = temp_copy("management_full.m4a", "other-roles-m4a");
    let check = |path: &Path| {
        for (name, field) in [
            ("ENGINEER", TagField::Engineer),
            ("MIXER", TagField::Mixer),
            ("DJMIXER", TagField::DjMixer),
            ("writer", TagField::Writer),
        ] {
            assert_eq!(mp4_freeform(path, name), value_of(&edits, field), "{name}");
        }
        assert_eq!(
            mp4_text(path, b"\xa9dir"),
            value_of(&edits, TagField::Director)
        );
    };
    credits_round_trip(&m4a, &edits, &check);
    read_back(&m4a);
}

/// An engineer edit replaces only the engineer pairs: a `mastering`
/// pair, a producer, v2.3 musician credits and an unknown `TXXX` stay as
/// they were, and so does the audio. A writer edit updates another
/// tagger's `TXXX:WRITER` in place, as one frame.
#[test]
fn other_role_edits_leave_neighbours_alone() {
    let engineer = TagEdit::new(TagField::Engineer, texts(&["New Engineer"]));
    let writer = TagEdit::new(TagField::Writer, texts(&["New Writer"]));

    let (_scratch, mp3) = temp_copy("management_full.mp3", "other-roles-neighbours");
    let audio = mp3_audio_tail(&std::fs::read(&mp3).unwrap()).to_vec();
    let mut frames: Vec<(String, Vec<u8>)> = id3_frames(&mp3)
        .1
        .into_iter()
        .filter(|(id, _)| id != "TIPL")
        .collect();
    frames.push((
        "TIPL".to_owned(),
        b"\x03mastering\x00Mo Master\x00engineer\x00Old Engineer\x00producer\x00Example Producer\x00"
            .to_vec(),
    ));
    frames.push(("TXXX".to_owned(), b"\x03ZZZ_UNHEARD_OF\x00mystery".to_vec()));
    frames.push(("TXXX".to_owned(), b"\x03WRITER\x00Old Writer".to_vec()));
    let refs: Vec<(&str, &[u8])> = frames
        .iter()
        .map(|(id, payload)| (id.as_str(), payload.as_slice()))
        .collect();
    let mut bytes = id3v24_raw(&refs);
    bytes.extend_from_slice(&audio);
    std::fs::write(&mp3, &bytes).unwrap();
    let writer_frames = |path: &Path| -> Vec<Vec<String>> {
        id3_frames(path)
            .1
            .iter()
            .filter(|(id, _)| id == "TXXX")
            .map(|(_, payload)| id3_strings(payload))
            .filter(|strings| strings[0].eq_ignore_ascii_case("writer"))
            .collect()
    };
    let untouched = |path: &Path| {
        let mut frames: Vec<(String, Vec<String>)> = id3_frames(path)
            .1
            .into_iter()
            .filter(|(id, payload)| {
                id != "TIPL"
                    && !(id == "TXXX" && id3_strings(payload)[0].eq_ignore_ascii_case("writer"))
            })
            .map(|(id, payload)| {
                let decoded = if id.starts_with('T') {
                    id3_strings(&payload)
                } else {
                    vec![format!("{payload:?}")]
                };
                (id, decoded)
            })
            .collect();
        frames.sort();
        frames
    };
    let before = untouched(&mp3);
    tags::save_tags(&mp3, &[engineer.clone(), writer]).unwrap();
    assert_eq!(
        id3_people(&mp3, "TIPL"),
        vec![
            pair("mastering", "Mo Master"),
            pair("engineer", "New Engineer"),
            pair("producer", "Example Producer"),
        ]
    );
    assert_eq!(writer_frames(&mp3), vec![texts(&["WRITER", "New Writer"])]);
    assert_eq!(id3_txxx_values(&mp3, "ZZZ_UNHEARD_OF"), texts(&["mystery"]));
    assert_eq!(untouched(&mp3), before);
    assert_eq!(audio_bytes(&mp3), audio);

    let ipls = latin1(&[
        "guitar",
        "A",
        "engineer",
        "Old Engineer",
        "lead vocals",
        "B",
    ]);
    let (_scratch, v23) = v23_file(
        "other-roles-neighbours-v23",
        &[("TIT2", b"\x00Title"), ("IPLS", &ipls)],
    );
    let audio = audio_bytes(&v23);
    tags::save_tags(&v23, std::slice::from_ref(&engineer)).unwrap();
    assert_eq!(
        id3_people(&v23, "IPLS"),
        vec![
            pair("guitar", "A"),
            pair("engineer", "New Engineer"),
            pair("lead vocals", "B"),
        ]
    );
    assert_eq!(id3_frames(&v23).0, 3);
    assert_eq!(audio_bytes(&v23), audio);
}

/// An undo recorded before these fields existed names their tags as
/// native items. Replaying it, with the removal a restore adds for the
/// field, puts the original bytes back.
#[test]
fn native_replays_restore_owned_tags() {
    let encode =
        |bytes: &[u8]| base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    let replay = |path: &Path, field: TagField, key: &str, values: Vec<String>| {
        tags::save_tags(
            path,
            &[
                TagEdit::verbatim(field, Vec::new()),
                TagEdit::native(key, values),
            ],
        )
        .unwrap_or_else(|error| panic!("{key}: {error}"));
    };

    let (_scratch, flac) = temp_copy("management_full.flac", "replay");
    let original = vorbis_key_values(&flac, "COMPOSER");
    tags::save_tags(
        &flac,
        &[TagEdit::new(TagField::Composer, texts(&["Changed"]))],
    )
    .unwrap();
    replay(
        &flac,
        TagField::Composer,
        "vorbis:COMPOSER",
        original.clone(),
    );
    assert_eq!(vorbis_key_values(&flac, "COMPOSER"), original);

    let (_scratch, m4a) = temp_copy("management_full.m4a", "replay");
    let original = mp4_freeform(&m4a, "LYRICIST");
    tags::save_tags(
        &m4a,
        &[TagEdit::new(TagField::Lyricist, texts(&["Changed"]))],
    )
    .unwrap();
    replay(
        &m4a,
        TagField::Lyricist,
        "mp4:----:LYRICIST",
        original.clone(),
    );
    assert_eq!(mp4_freeform(&m4a, "LYRICIST"), original);

    // MP3: TCOM, and a comment beside an iTunNORM, listed together as
    // `id3:COMM` the way the old reader listed them.
    let (_scratch, mp3) = temp_copy("management_full.mp3", "replay");
    let comment = b"\x03eng\x00old note".to_vec();
    let itunnorm = b"\x00engiTunNORM\x00 00000A1B".to_vec();
    id3_inject_frames(&mp3, &[("COMM", &comment), ("COMM", &itunnorm)]);
    let tcom = id3_text_values(&mp3, "TCOM");
    tags::save_tags(
        &mp3,
        &[
            TagEdit::new(TagField::Composer, texts(&["Changed"])),
            TagEdit::new(TagField::Comment, texts(&["new note"])),
        ],
    )
    .unwrap();
    replay(&mp3, TagField::Composer, "id3:TCOM", tcom.clone());
    replay(
        &mp3,
        TagField::Comment,
        "id3:COMM",
        vec![encode(&comment), encode(&itunnorm)],
    );
    let mut comments: Vec<Vec<u8>> = id3_frames(&mp3)
        .1
        .into_iter()
        .filter(|(id, _)| id == "COMM")
        .map(|(_, payload)| payload)
        .collect();
    comments.sort();
    let mut original = vec![comment, itunnorm];
    original.sort();
    assert_eq!(comments, original);
    assert_eq!(id3_text_values(&mp3, "TCOM"), tcom);
}

/// v2 recorded a v2.3 file's tag as mutagen re-rendered it, which moves
/// every `IPLS` pair into a v2.4 `TIPL`. Restoring that baseline over the
/// v2.3 file keeps its musician credit beside the producer.
#[test]
fn v2_baseline_restore_keeps_v23_musician_credits() {
    use droppedneedle_library::publish::{staging, v2_baseline};
    let credits = [("guitar", "A"), ("producer", "P")];
    let ipls = utf16_people(&credits, false);
    let (_scratch, path) = v23_file(
        "v2-ipls",
        &[("TIT2", b"\x00Management Track"), ("IPLS", &ipls)],
    );
    let snapshot = id3v24_raw(&[
        ("TIT2", b"\x03Management Track"),
        ("TIPL", b"\x03guitar\x00A\x00producer\x00P\x00"),
    ]);
    let json = format!(
        r#"{{"snapshot_version":1,"probe":{{"detected_format":"mp3"}},
            "native_tags":{{"encoded_id3":"{}"}}}}"#,
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &snapshot)
    );
    let baseline = v2_baseline::translate(json.as_bytes(), "mp3", "music", "a.mp3")
        .unwrap()
        .doc;
    let current = staging::document_from_file(&path).unwrap();
    let updates = staging::restore_updates(&baseline, &current, "a.mp3");
    let edits = staging::tag_edits(&updates);
    tags::save_tags(&path, &edits).unwrap();
    assert_eq!(
        id3_people(&path, "IPLS"),
        vec![pair("guitar", "A"), pair("producer", "P")]
    );
}

/// On v2.4 a `TIPL` pair outside the involved roles is a musician
/// credit, as Picard reads it; a performer write moves it to `TMCL`.
#[test]
fn v24_tipl_musicians_read_as_performers() {
    let (_scratch, mp3) = temp_copy("management_full.mp3", "tipl-musicians");
    id3_inject_frames(&mp3, &[("TMCL", b"\x03bass\x00B\x00")]);
    let frames: Vec<(String, Vec<u8>)> = id3_frames(&mp3)
        .1
        .into_iter()
        .map(|(id, payload)| match id.as_str() {
            "TIPL" => (
                id,
                b"\x03guitar\x00A\x00producer\x00Example Producer\x00".to_vec(),
            ),
            _ => (id, payload),
        })
        .collect();
    let audio = mp3_audio_tail(&std::fs::read(&mp3).unwrap()).to_vec();
    let refs: Vec<(&str, &[u8])> = frames
        .iter()
        .map(|(id, payload)| (id.as_str(), payload.as_slice()))
        .collect();
    let mut bytes = id3v24_raw(&refs);
    bytes.extend_from_slice(&audio);
    std::fs::write(&mp3, &bytes).unwrap();
    let fields = tags::read_fields(&mp3).unwrap();
    assert_eq!(
        fields[&TagField::Performer],
        texts(&["B (bass)", "A (guitar)"])
    );
    assert_eq!(fields[&TagField::Producer], texts(&["Example Producer"]));
    // An undo writes back every field as it read: nothing moves.
    // Payloads by frame id: ID3 frame order is not kept across a save.
    let people = |path: &Path| -> Vec<(String, Vec<u8>)> {
        let mut frames: Vec<(String, Vec<u8>)> = id3_frames(path)
            .1
            .into_iter()
            .filter(|(id, _)| matches!(id.as_str(), "TIPL" | "TMCL"))
            .collect();
        frames.sort();
        frames
    };
    let held = people(&mp3);
    tags::save_tags(&mp3, &[TagEdit::set_title("Later")]).unwrap();
    let undo: Vec<TagEdit> = fields
        .iter()
        .map(|(field, values)| TagEdit::verbatim(*field, values.clone()))
        .collect();
    tags::save_tags(&mp3, &undo).unwrap();
    assert_eq!(people(&mp3), held);
    assert_eq!(id3_text_values(&mp3, "TIT2"), texts(&["Management Track"]));
    tags::save_tags(
        &mp3,
        &[TagEdit::new(
            TagField::Performer,
            texts(&["B (bass)", "A (guitar)", "C"]),
        )],
    )
    .unwrap();
    assert_eq!(
        id3_people(&mp3, "TMCL"),
        vec![pair("bass", "B"), pair("guitar", "A"), pair("", "C")]
    );
    assert_eq!(
        id3_people(&mp3, "TIPL"),
        vec![pair("producer", "Example Producer")]
    );
}

/// A NUL inside a credit or comment would split it into other strings;
/// it is refused before anything is written.
#[test]
fn nul_in_credits_and_comments_is_refused() {
    for edit in [
        TagEdit::new(TagField::Producer, texts(&["A\u{0}B"])),
        TagEdit::new(TagField::Performer, texts(&["A\u{0}B (guitar)"])),
        TagEdit::new(TagField::Comment, texts(&["a\u{0}b"])),
    ] {
        let (refusal, before, after) = save_refused("management_full.mp3", "nul-credit", &[edit]);
        assert!(
            matches!(refusal, Refusal::UnencodableItem { .. }),
            "{refusal:?}"
        );
        assert_eq!(before, after);
    }
}

/// lofty writes a v2.3 `IPLS` with one little-endian byte-order mark for
/// the whole frame. Its names read as written, survive an unrelated
/// edit byte for byte, and survive a producer edit.
#[test]
fn lofty_v23_ipls_keeps_its_names() {
    let ipls = utf16_people(&[("guitar", "Ána"), ("producer", "Pål")], true);
    let (_scratch, path) = v23_file(
        "lofty-ipls",
        &[("TIT2", b"\x00Management Track"), ("IPLS", &ipls)],
    );
    let fields = tags::read_fields(&path).unwrap();
    assert_eq!(fields[&TagField::Performer], texts(&["Ána (guitar)"]));
    assert_eq!(fields[&TagField::Producer], texts(&["Pål"]));
    tags::save_tags(&path, &[TagEdit::set_title("Later")]).unwrap();
    let held: Vec<Vec<u8>> = id3_frames(&path)
        .1
        .into_iter()
        .filter(|(id, _)| id == "IPLS")
        .map(|(_, payload)| payload)
        .collect();
    assert_eq!(held, vec![ipls]);
    tags::save_tags(
        &path,
        &[TagEdit::new(TagField::Producer, texts(&["New Producer"]))],
    )
    .unwrap();
    assert_eq!(
        id3_people(&path, "IPLS"),
        vec![pair("guitar", "Ána"), pair("producer", "New Producer")]
    );
}

/// v2.3 keeps every credit in `IPLS`, where a producer is not a
/// musician: a performer playing "producer" is refused, not filed as a
/// producer credit.
#[test]
fn v23_performer_with_an_involved_role_is_refused() {
    let (_scratch, path) = pure_v23_copy("v23-involved");
    let before = std::fs::read(&path).unwrap();
    let result = tags::save_tags(
        &path,
        &[TagEdit::new(
            TagField::Performer,
            texts(&["Bob (producer)"]),
        )],
    );
    assert!(
        matches!(
            result,
            Err(TagsError::SaveRefused {
                refusal: Refusal::UnencodableItem { .. },
                ..
            })
        ),
        "{result:?}"
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

/// A write of as many values as the role held keeps its pairs in their
/// places, so an undo puts interleaved pairs back in order. A comment
/// keeps its language.
#[test]
fn credit_undo_keeps_order_and_comment_language() {
    let (_scratch, mp3) = temp_copy("management_full.mp3", "interleaved");
    let audio = mp3_audio_tail(&std::fs::read(&mp3).unwrap()).to_vec();
    let mut frames: Vec<(String, Vec<u8>)> = id3_frames(&mp3)
        .1
        .into_iter()
        .filter(|(id, _)| id != "TIPL")
        .collect();
    frames.push((
        "TIPL".to_owned(),
        b"\x03producer\x00P1\x00engineer\x00E\x00producer\x00P2\x00".to_vec(),
    ));
    frames.push(("COMM".to_owned(), b"\x03deu\x00alt".to_vec()));
    let refs: Vec<(&str, &[u8])> = frames
        .iter()
        .map(|(id, payload)| (id.as_str(), payload.as_slice()))
        .collect();
    let mut bytes = id3v24_raw(&refs);
    bytes.extend_from_slice(&audio);
    std::fs::write(&mp3, &bytes).unwrap();
    let original = id3_people(&mp3, "TIPL");
    tags::save_tags(
        &mp3,
        &[
            TagEdit::new(TagField::Producer, texts(&["X", "Y"])),
            TagEdit::new(TagField::Comment, texts(&["neu"])),
        ],
    )
    .unwrap();
    assert_eq!(
        id3_comments(&mp3),
        vec![("deu".to_owned(), String::new(), "neu".to_owned())]
    );
    tags::save_tags(
        &mp3,
        &[TagEdit::verbatim(TagField::Producer, texts(&["P1", "P2"]))],
    )
    .unwrap();
    assert_eq!(id3_people(&mp3, "TIPL"), original);
}

/// An integer atom whose bytes happen to be printable (an iTunes `cnID`)
/// keeps its data type and value across a save, and the `ilst` items
/// keep their order.
#[test]
fn mp4_integer_atoms_keep_their_type_and_order() {
    use lofty::config::{ParseOptions, WriteOptions};
    use lofty::file::AudioFile as _;
    use lofty::mp4::{Atom, AtomData, AtomIdent, DataType, Mp4File};
    use lofty::tag::TagExt as _;
    let (_scratch, m4a) = temp_copy("management_full.m4a", "typed-atom");
    // Planted with lofty itself, as an iTunes purchase carries it.
    let mut file =
        Mp4File::read_from(&mut std::fs::File::open(&m4a).unwrap(), ParseOptions::new()).unwrap();
    let mut ilst = file.ilst_mut().unwrap().clone();
    ilst.insert(Atom::new(
        AtomIdent::Fourcc(*b"cnID"),
        AtomData::Unknown {
            code: DataType::BeSignedInteger,
            data: b"Jk<-".to_vec(),
        },
    ));
    ilst.save_to_path(&m4a, WriteOptions::new()).unwrap();
    let view = |path: &Path| -> Vec<(Vec<u8>, Option<String>, Vec<u32>)> {
        mp4_atoms(path)
            .into_iter()
            .map(|atom| (atom.kind.to_vec(), atom.name, atom.types))
            .collect()
    };
    let cnid = |path: &Path| -> Vec<(u32, Vec<u8>)> {
        mp4_atoms(path)
            .into_iter()
            .filter(|atom| &atom.kind == b"cnID")
            .flat_map(|atom| atom.types.into_iter().zip(atom.data))
            .collect()
    };
    assert_eq!(cnid(&m4a), vec![(21, b"Jk<-".to_vec())]);
    let before = view(&m4a);
    tags::save_tags(&m4a, &[TagEdit::set_title("Later")]).unwrap();
    assert_eq!(cnid(&m4a), vec![(21, b"Jk<-".to_vec())]);
    assert_eq!(view(&m4a), before);
}

// ---------------------------------------------------------------------------
// Save wrapper: frames lofty would re-encode go back byte for byte
// ---------------------------------------------------------------------------

/// An MP3 whose ID3 tag (`major` 3 or 4) holds exactly these frames,
/// built by bytes over the management fixture's audio.
fn raw_id3_mp3(test: &str, major: u8, frames: &[(&str, Vec<u8>)]) -> (ScratchDir, PathBuf) {
    let syncsafe = |size: usize| {
        [
            ((size >> 21) & 0x7F) as u8,
            ((size >> 14) & 0x7F) as u8,
            ((size >> 7) & 0x7F) as u8,
            (size & 0x7F) as u8,
        ]
    };
    let mut body = Vec::new();
    for (id, payload) in frames {
        body.extend_from_slice(id.as_bytes());
        if major == 4 {
            body.extend_from_slice(&syncsafe(payload.len()));
        } else {
            body.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        }
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(payload);
    }
    body.resize(body.len() + 256, 0);
    let mut out = vec![b'I', b'D', b'3', major, 0, 0];
    out.extend_from_slice(&syncsafe(body.len()));
    out.extend_from_slice(&body);
    out.extend_from_slice(mp3_audio_tail(
        &std::fs::read(fixture("management_full.mp3")).unwrap(),
    ));
    let dir = ScratchDir::new(&format!("tags-{test}"));
    let path = dir.join("raw.mp3");
    std::fs::write(&path, out).unwrap();
    (dir, path)
}

/// Little-endian UTF-16 with its byte order mark.
fn utf16(text: &str) -> Vec<u8> {
    let mut out = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// A title save keeps every other frame byte for byte in a v2.3 and a
/// v2.4 tag, though lofty would rewrite each of these: Latin-1 pictures
/// and lyrics, a UTF-16 URL, the ratings Windows Media Player, MusicBee
/// and Picard write (a counter added, a rating rounded to stars) and
/// an `ETCO` it cannot write at all.
#[test]
fn title_save_keeps_frames_lofty_rewrites() {
    let frames: Vec<(&str, Vec<u8>)> = vec![
        ("TIT2", latin1(&["Probe"])),
        (
            "APIC",
            b"\0image/png\0\x03cover\0\x89PNG\r\n\x1a\ndata".to_vec(),
        ),
        ("USLT", b"\0eng\0Some lyrics\nline two".to_vec()),
        (
            "USLT",
            [
                &[1u8][..],
                b"eng",
                &utf16("d"),
                &[0, 0],
                &utf16("UTF-16 lyrics"),
            ]
            .concat(),
        ),
        (
            "WXXX",
            [&[1u8][..], &utf16("s"), &[0, 0], b"http://example.org/y"].concat(),
        ),
        ("POPM", b"Windows Media Player 9 Series\0\xC4".to_vec()),
        ("POPM", b"MusicBee\0\xBA".to_vec()),
        ("POPM", b"users@musicbrainz.org\0\x99\0\0\0\0".to_vec()),
        ("POPM", b"\0\x05".to_vec()),
        ("ETCO", vec![2, 3, 0, 0, 0, 1]),
    ];
    for major in [3, 4] {
        let (_dir, path) = raw_id3_mp3(&format!("kept-{major}"), major, &frames);
        let tail = after_id3v2(&path);
        let (_, before) = id3_view(&path);
        assert_eq!(tags::save::tags_kept(&path).unwrap().refusal, None);

        tags::save_tags(&path, &[TagEdit::set_title("New")]).unwrap();
        let (after_major, after) = id3_view(&path);
        assert_eq!(after_major, major);
        assert_eq!(
            view_without(&after, &["TIT2"]),
            view_without(&before, &["TIT2"]),
            "v2.{major}"
        );
        assert_eq!(id3_text_values(&path, "TIT2"), texts(&["New"]));
        assert_eq!(after_id3v2(&path), tail);
    }
}

/// A tag holding two frames the writer keeps only once (identical
/// pictures, two play counters, two `UFID`s of one owner) is refused at
/// preview in words, and a save leaves the file as it was.
#[test]
fn frames_kept_only_once_are_refused_before_saving() {
    let cases: Vec<(&str, Vec<u8>, Vec<u8>)> = vec![
        (
            "APIC",
            b"\0image/png\0\x03\0abc".to_vec(),
            b"\0image/png\0\x03\0abc".to_vec(),
        ),
        ("PCNT", vec![0, 0, 0, 1], vec![0, 0, 0, 2]),
        ("UFID", b"owner\0one".to_vec(), b"owner\0two".to_vec()),
    ];
    for (id, first, second) in cases {
        let frames = vec![("TIT2", latin1(&["Probe"])), (id, first), (id, second)];
        let (_dir, path) = raw_id3_mp3(&format!("unkept-{id}"), 3, &frames);
        let message = tags::save::tags_kept(&path).unwrap().refusal.unwrap();
        assert!(message.contains(&format!("two {id} frames")), "{message}");
        let before = std::fs::read(&path).unwrap();
        assert!(matches!(
            tags::save_tags(&path, &[TagEdit::set_title("New")]),
            Err(TagsError::SaveRefused { .. })
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before, "{id}");
    }
}

/// A v2.3 tag saved as v2.4 (it holds a `TDRC`) keeps its chapters
/// readable: a chapter picture of 128 bytes or more has its embedded
/// frame size rewritten as syncsafe, its bytes unchanged.
#[test]
fn chapters_move_to_v24_readable() {
    let picture: Vec<u8> = (0..300u32).map(|byte| byte as u8).collect();
    let sub_frame = |id: &str, body: &[u8]| {
        let mut out = id.as_bytes().to_vec();
        out.extend_from_slice(&(body.len() as u32).to_be_bytes());
        out.extend_from_slice(&[0, 0]);
        out.extend_from_slice(body);
        out
    };
    let mut chap = b"ch1\0".to_vec();
    chap.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0x27, 0x10]);
    chap.extend_from_slice(&[0xFF; 8]);
    chap.extend(sub_frame("TIT2", b"\0Intro"));
    chap.extend(sub_frame(
        "APIC",
        &[&b"\0image/png\0\x03\0"[..], &picture].concat(),
    ));
    let frames = vec![
        ("TIT2", latin1(&["Probe"])),
        ("TDRC", latin1(&["2024"])),
        ("CHAP", chap),
    ];
    let (_dir, path) = raw_id3_mp3("chapters", 3, &frames);
    assert_eq!(tags::save::tags_kept(&path).unwrap().refusal, None);

    let report = tags::save_tags(&path, &[TagEdit::set_title("New")]).unwrap();
    assert_eq!(report.id3_version.as_deref(), Some("2.4"));
    let mut file = std::fs::File::open(&path).unwrap();
    let mpeg = <lofty::mpeg::MpegFile as lofty::file::AudioFile>::read_from(
        &mut file,
        lofty::config::ParseOptions::new(),
    )
    .unwrap();
    let chapter = mpeg
        .id3v2()
        .unwrap()
        .iter()
        .find_map(|frame| match frame {
            lofty::id3::v2::Frame::Chapter(chapter) => Some(chapter.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(chapter.id, "ch1");
    assert_eq!(chapter.times, 0..10_000);
    assert_eq!(chapter.title(), Some("Intro"));
    let pictures: Vec<Vec<u8>> = chapter
        .children
        .iter()
        .filter_map(|frame| match frame {
            lofty::id3::v2::Frame::Picture(apic) => Some(apic.picture.data().to_vec()),
            _ => None,
        })
        .collect();
    assert_eq!(pictures, vec![picture]);
}

// ---------------------------------------------------------------------------
// Scan and the tag editor read one file the same way
// ---------------------------------------------------------------------------

/// `YYYY`, `YYYY-MM` or `YYYY-MM-DD` from the start of a date, as scan
/// keeps a release date.
fn date_prefix(value: &str) -> Option<String> {
    let value = value.trim();
    let digits = |part: &str| part.bytes().all(|byte| byte.is_ascii_digit());
    let mut out = value.get(..4).filter(|year| digits(year))?.to_owned();
    let mut rest = &value[4..];
    for _ in 0..2 {
        match rest.strip_prefix('-').and_then(|after| after.get(..2)) {
            Some(part) if digits(part) => {
                out.push('-');
                out.push_str(part);
                rest = &rest[3..];
            }
            _ => break,
        }
    }
    Some(out)
}

/// Scan reads tags through lofty's generic items, the tag editor through
/// its own byte parsers. For every committed fixture the editor can
/// write, both give the same value for each writable field scan reads,
/// so the catalog and the editor never show one file two ways. The
/// release date is the editor's date fields put through scan's rule.
#[test]
fn scan_and_editor_agree_on_writable_fields() {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(fixture(""))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    paths.sort();
    let mut checked = 0;
    let mut differences: Vec<String> = Vec::new();
    for path in paths {
        let Ok(format) = format_for_path(&path) else {
            continue;
        };
        if !tags::save::writable(format) {
            continue;
        }
        checked += 1;
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let (scan, _) = tags::read::read_scan_metadata(&path, format).unwrap();
        let editor = tags::save::read_document(&path).unwrap();
        let mut agree = |label: &str, scanned: String, edited: String| {
            if scanned != edited {
                differences.push(format!("{name} {label}: scan {scanned}, editor {edited}"));
            }
        };
        // Scan splits values on NUL, trims them and drops repeats.
        let all = |field: TagField| -> Vec<String> {
            let mut out: Vec<String> = Vec::new();
            for value in editor.values.get(&field).into_iter().flatten() {
                for part in value.split('\0').map(str::trim) {
                    if !part.is_empty() && !out.iter().any(|seen| seen == part) {
                        out.push(part.to_owned());
                    }
                }
            }
            out
        };
        let first = |field: TagField| all(field).into_iter().next();
        let number = |field: TagField| first(field).and_then(|value| value.parse::<u32>().ok());
        let gain = |field: TagField| {
            first(field).and_then(|value| {
                let lower = value.to_lowercase();
                lower
                    .strip_suffix("db")
                    .unwrap_or(&lower)
                    .trim()
                    .parse::<f64>()
                    .ok()
            })
        };
        let names = |listed: bool, credits: &[AudioArtistCredit]| -> Vec<String> {
            if listed {
                credits.iter().map(|credit| credit.name.clone()).collect()
            } else {
                Vec::new()
            }
        };
        for (label, scanned, field) in [
            ("title", Some(&scan.title), TagField::Title),
            ("artist", Some(&scan.artist), TagField::Artist),
            ("album", Some(&scan.album), TagField::Album),
        ] {
            let scanned = scanned.filter(|value| !value.is_empty()).cloned();
            agree(label, format!("{scanned:?}"), format!("{:?}", first(field)));
        }
        for (label, scanned, field) in [
            ("album_artist", &scan.album_artist, TagField::AlbumArtist),
            ("title_sort", &scan.title_sort, TagField::TitleSort),
            ("artist_sort", &scan.artist_sort, TagField::ArtistSort),
            ("album_sort", &scan.album_sort, TagField::AlbumSort),
            (
                "album_artist_sort",
                &scan.album_artist_sort,
                TagField::AlbumArtistSort,
            ),
            ("disc_subtitle", &scan.disc_subtitle, TagField::DiscSubtitle),
            (
                "original_date",
                &scan.original_release_date,
                TagField::OriginalDate,
            ),
            ("release_type", &scan.release_type, TagField::ReleaseType),
            ("media", &scan.edition.media, TagField::Media),
            ("barcode", &scan.edition.barcode, TagField::Barcode),
            (
                "catalog_number",
                &scan.edition.catalog_number,
                TagField::CatalogNumber,
            ),
            (
                "release_country",
                &scan.edition.release_country,
                TagField::ReleaseCountry,
            ),
            (
                "release_group_id",
                &scan.musicbrainz_release_group_id,
                TagField::MusicBrainzReleaseGroupId,
            ),
            (
                "release_id",
                &scan.musicbrainz_release_id,
                TagField::MusicBrainzReleaseId,
            ),
            (
                "recording_id",
                &scan.musicbrainz_recording_id,
                TagField::MusicBrainzRecordingId,
            ),
            (
                "release_track_id",
                &scan.musicbrainz_release_track_id,
                TagField::MusicBrainzReleaseTrackId,
            ),
            ("acoustid_id", &scan.acoustid_id, TagField::AcoustIdId),
        ] {
            agree(label, format!("{scanned:?}"), format!("{:?}", first(field)));
        }
        for (label, scanned, field) in [
            ("genres", scan.genres.clone(), TagField::Genre),
            (
                "artist_ids",
                scan.musicbrainz_artist_ids.clone(),
                TagField::MusicBrainzArtistId,
            ),
            (
                "album_artist_ids",
                scan.musicbrainz_album_artist_ids.clone(),
                TagField::MusicBrainzAlbumArtistId,
            ),
            (
                "artists",
                names(scan.artists_listed, &scan.artists),
                TagField::Artists,
            ),
            (
                "album_artists",
                names(scan.album_artists_listed, &scan.album_artists),
                TagField::AlbumArtists,
            ),
        ] {
            agree(label, format!("{scanned:?}"), format!("{:?}", all(field)));
        }
        agree(
            "compilation",
            scan.compilation.to_string(),
            (first(TagField::Compilation).as_deref() == Some("1")).to_string(),
        );
        agree(
            "track_number",
            scan.track_number.to_string(),
            number(TagField::TrackNumber).unwrap_or(0).to_string(),
        );
        agree(
            "disc_number",
            scan.disc_number.to_string(),
            number(TagField::DiscNumber).unwrap_or(1).to_string(),
        );
        agree(
            "disc_total",
            format!("{:?}", scan.edition.total_discs),
            format!(
                "{:?}",
                number(TagField::DiscTotal).filter(|total| *total > 0)
            ),
        );
        let date = first(TagField::Date);
        agree(
            "year",
            format!("{:?}", scan.year),
            format!(
                "{:?}",
                date.as_deref()
                    .and_then(|date| date.get(..4)?.parse::<i32>().ok())
            ),
        );
        for (label, scanned, field) in [
            (
                "track_gain",
                scan.replaygain_track_gain,
                TagField::ReplayGainTrackGain,
            ),
            (
                "album_gain",
                scan.replaygain_album_gain,
                TagField::ReplayGainAlbumGain,
            ),
            (
                "track_peak",
                scan.replaygain_track_peak,
                TagField::ReplayGainTrackPeak,
            ),
            (
                "album_peak",
                scan.replaygain_album_peak,
                TagField::ReplayGainAlbumPeak,
            ),
        ] {
            agree(label, format!("{scanned:?}"), format!("{:?}", gain(field)));
        }
        // Scan's release date rule over what the editor reads: the
        // release date item, an MP4 date, a Vorbis `YEAR`, then the date
        // when it is not earlier than the original date.
        let native = |key: &str| {
            editor
                .native
                .iter()
                .find(|(held, _)| held.eq_ignore_ascii_case(key))
                .and_then(|(_, values)| values.first())
                .and_then(|value| date_prefix(value))
        };
        let vorbis = matches!(
            format,
            AudioFormat::Flac | AudioFormat::Ogg | AudioFormat::Opus
        );
        let release_item = match format {
            AudioFormat::Mp3 => "id3:TDRL",
            AudioFormat::M4a => "mp4:----:RELEASEDATE",
            _ => "vorbis:RELEASEDATE",
        };
        let dated = date.as_deref().and_then(date_prefix);
        let release_date = native(release_item)
            .or_else(|| dated.clone().filter(|_| format == AudioFormat::M4a))
            .or_else(|| native("vorbis:YEAR").filter(|_| vorbis))
            .or_else(|| {
                let original =
                    first(TagField::OriginalDate).and_then(|value| date_prefix(&value))?;
                dated.clone().filter(|date| *date >= original)
            });
        agree(
            "release_date",
            format!("{:?}", scan.edition.release_date),
            format!("{release_date:?}"),
        );
    }
    assert!(checked >= 10, "only {checked} writable fixtures found");
    assert!(
        differences.is_empty(),
        "scan and the editor disagree:\n{}",
        differences.join("\n")
    );
}
