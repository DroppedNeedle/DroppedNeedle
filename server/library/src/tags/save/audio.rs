//! The audio a tag save must leave as it was: one SHA-256 over each
//! format's audio region, taken before and after the save.
//!
//! - MP3: everything after the ID3v2 tag (its footer included), or the
//!   whole file when it has none. ID3v1, APE and padding past the tag's
//!   size count as audio here; the save leaves them alone.
//! - FLAC: everything after the last metadata block.
//! - MP4: each top-level `mdat` payload, in order. The save moves boxes
//!   and patches offsets, so the rest of the file is not compared.
//! - Ogg Vorbis and Opus: each packet after the first stream's comment
//!   packet, chained streams included, in order. The save renumbers
//!   pages and redoes their checksums, so pages are not compared. It
//!   drops bytes after the last whole page, so a file holding some is
//!   refused before the save.
//!
//! A file whose region cannot be worked out is refused like unparseable
//! tag bytes before the save, and as changed audio after it.

use sha2::{Digest as _, Sha256};

use super::Refusal;
use super::id3_bytes::id3v2_end;
use super::mp4::mdat_payloads;
use super::vorbis::{flac_audio_start, ogg_packets};
use crate::tags::AudioFormat;

/// The SHA-256 of a file's audio region.
pub(super) type AudioDigest = [u8; 32];

pub(super) fn audio_digest(format: AudioFormat, bytes: &[u8]) -> Result<AudioDigest, Refusal> {
    let no_inventory = |detail: String| Refusal::NoByteInventory {
        detail: format!("audio unreadable: {detail}"),
    };
    let mut hash = Sha256::new();
    match format {
        AudioFormat::Mp3 => hash.update(&bytes[id3v2_end(bytes)?..]),
        AudioFormat::Flac => hash.update(&bytes[flac_audio_start(bytes).map_err(no_inventory)?..]),
        AudioFormat::M4a => {
            for payload in mdat_payloads(bytes).map_err(no_inventory)? {
                length_prefixed(&mut hash, payload);
            }
        }
        AudioFormat::Ogg | AudioFormat::Opus => {
            ogg_packets(bytes, |stream, index, packet| {
                if stream > 0 || index >= 2 {
                    length_prefixed(&mut hash, &packet);
                }
                true
            })
            .map_err(no_inventory)?;
        }
        AudioFormat::Aac | AudioFormat::Wav => {
            return Err(no_inventory(format!("{} is not written", format.as_str())));
        }
    }
    Ok(hash.finalize().into())
}

/// One piece, with its length first, so pieces cannot run into each other.
fn length_prefixed(hash: &mut Sha256, piece: &[u8]) {
    hash.update((piece.len() as u64).to_be_bytes());
    hash.update(piece);
}
