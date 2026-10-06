//! Stage-8 tags slice briefs: read, probe, fingerprint, save wrapper.
//!
//! Reads, probes, and fingerprints run against the committed real-audio
//! fixtures in `backend/tests/fixtures/library/`. Every save test copies
//! its fixture into a scratch dir first; the committed files are never
//! written. Nothing here touches the network.

use droppedneedle::library::tags;

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

fn vorbis_values(path: &Path, key: &str) -> Vec<String> {
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
fn mp3_audio_tail(bytes: &[u8]) -> &[u8] {
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
fn id3_inject_frames(path: &Path, frames: &[(&str, &[u8])]) {
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
fn flac_inject_pairs(path: &Path, extra: &[(&str, &str)]) {
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
    assert_eq!(
        tag.musicbrainz_recording_id.as_deref(),
        Some("10000000-0000-4000-8000-000000000003")
    );
    assert_eq!(
        tag.musicbrainz_release_track_id.as_deref(),
        Some("10000000-0000-4000-8000-000000000004")
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
    // AAC tag reads are the accepted stage-1 gap: core fields only.
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

    // Plant unknown frames the way the stage-1 spike did, by splicing
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

    // Plant an unknown field the spike way, by splicing the raw pair
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

#[test]
fn save_refuses_mixed_v23_fixture() {
    // The committed v2.3 fixture actually carries v2.4-only frames
    // (TDRC, TSOP, ...), so the wrapper must refuse it untouched.
    let (refusal, before, after) = save_refused(
        "management_full_v23.mp3",
        "v23",
        &[TagEdit::set_title("WRAP TITLE")],
    );
    assert!(
        matches!(refusal, Refusal::MixedId3v23 { .. }),
        "{refusal:?}"
    );
    assert_eq!(before, after);
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

    // The full tag set keeps it a clean v2.3 tag: years in TYER/TORY,
    // sort names in TXXX, multiple values joined with '/'.
    let report = tags::save_tags(&path, &picard_edits()).unwrap();
    assert_eq!(report.id3_version.as_deref(), Some("2.3"));
    let bytes = std::fs::read(&path).unwrap();
    let byte_report = inspect_id3_bytes(&bytes).unwrap();
    assert_eq!(mixed_v23_detail(&byte_report), None);
    let fields = tags::read_fields(&path).unwrap();
    assert_eq!(fields[&TagField::Date], vec!["2024".to_owned()]);
    assert_eq!(fields[&TagField::OriginalDate], vec!["2019".to_owned()]);
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
    ]
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
