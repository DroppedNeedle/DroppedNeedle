//! The audio a tag save must leave as it was: one SHA-256 over each
//! format's audio region, taken before and after the save.
//!
//! - MP3: everything after the ID3v2 tag (its footer included), or the
//!   whole file when it has none. ID3v1, APE and padding past the tag's
//!   size count as audio here; the save leaves them alone.
//! - FLAC: any leading ID3v2 prefix before `fLaC`, and everything after
//!   the last metadata block. Hashing both regions keeps a save from
//!   silently stripping or altering a leading ID3v2 tag, including on a
//!   FLAC with no Vorbis comment block yet.
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
use super::vorbis::{flac_regions, ogg_packets};
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
        AudioFormat::Flac => {
            let (stream, audio_start) = flac_regions(bytes).map_err(no_inventory)?;
            length_prefixed(&mut hash, &bytes[..stream]);
            length_prefixed(&mut hash, &bytes[audio_start..]);
        }
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

#[cfg(test)]
mod tests {
    use super::audio_digest;
    use crate::tags::AudioFormat;

    /// A FLAC with a leading ID3v2 tag hashes both that prefix and the
    /// audio after the metadata blocks, so stripping or altering the
    /// prefix changes the digest and is refused by post-save verify.
    #[test]
    fn flac_digest_catches_stripped_or_mutated_id3v2_prefix() {
        let mut flac = b"fLaC".to_vec();
        flac.push(0x80);
        flac.extend_from_slice(&[0, 0, 0]);
        flac.extend_from_slice(b"AUDIO_BYTES");

        let mut prefix = b"ID3\x03\x00\x00\x00\x00\x00\x04TEST".to_vec();
        let mut prefixed = prefix.clone();
        prefixed.extend_from_slice(&flac);

        let before = audio_digest(AudioFormat::Flac, &prefixed).unwrap();
        let stripped = audio_digest(AudioFormat::Flac, &flac).unwrap();
        assert_ne!(
            before, stripped,
            "stripping ID3v2 prefix must change digest"
        );

        prefix[10] = b'X';
        let mut mutated = prefix;
        mutated.extend_from_slice(&flac);
        let after_mutate = audio_digest(AudioFormat::Flac, &mutated).unwrap();
        assert_ne!(
            before, after_mutate,
            "mutating ID3v2 prefix must change digest"
        );

        let mut overrunning = b"ID3\x03\x00\x00\x0F\x7F\x7F\x7F".to_vec();
        overrunning.extend_from_slice(&flac);
        assert_eq!(
            audio_digest(AudioFormat::Flac, &overrunning),
            Err(super::Refusal::NoByteInventory {
                detail: "audio unreadable: tag overruns the file".to_owned(),
            })
        );
    }
}
