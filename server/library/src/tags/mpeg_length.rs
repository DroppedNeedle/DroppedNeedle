//! Whether an MP3's length was measured or estimated.
//!
//! lofty takes an MP3's length from the Xing, Info or VBRI header in the
//! first frame when that header counts the frames and bytes, and
//! otherwise divides the stream's size by the first frame's bitrate, which
//! can be seconds off for a VBR file. It does not say which it did, so
//! this finds the first frame the way lofty does and reads that header,
//! without reading the tags. Checked against lofty 0.25.4: `mpeg/read.rs`
//! (`read_from`, `find_next_frame`), `mpeg/header.rs` (`Header::read`,
//! `cmp_header`, `VbrHeader::read`, `VbrHeader::is_valid`) and
//! `mpeg/properties.rs` (`read_properties`). Check it again when lofty
//! moves.
//!
//! An APE tag before the audio, which lofty also skips, reads here as an
//! estimate: that only keeps the length out of the rules that settle on
//! a length alone.

use std::fs::File;
use std::io::{BufReader, ErrorKind, Read, Seek, SeekFrom};
use std::path::Path;

/// Bitrates in kbit/s by MPEG version (1, then 2 and 2.5), layer (I, II,
/// III) and bitrate index. Zero is free format or invalid.
const BITRATES: [[[u32; 16]; 3]; 2] = [
    [
        [
            0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448, 0,
        ],
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0,
        ],
        [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 0,
        ],
    ],
    [
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256, 0,
        ],
        [
            0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
        ],
        [
            0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0,
        ],
    ],
];
/// Sample rates by MPEG version (1, 2, 2.5) and sample rate index.
const SAMPLE_RATES: [[u32; 3]; 3] = [
    [44100, 48000, 32000],
    [22050, 24000, 16000],
    [11025, 12000, 8000],
];
/// Samples per frame by layer and version (1, then 2 and 2.5).
const SAMPLES: [[u32; 2]; 3] = [[384, 384], [1152, 1152], [1152, 576]];
/// Side information bytes by version (1, then 2 and 2.5) and channel mode.
const SIDE_INFORMATION: [[u64; 4]; 2] = [[32, 32, 32, 17], [17, 17, 17, 9]];
const PADDING: [u32; 3] = [4, 1, 1];
/// The header bits two frames of one stream share: sync, version, layer
/// and sample rate.
const HEADER_MASK: u32 = 0xFFFE_0C00;

/// True when lofty takes this MP3's length from a frame count. A file
/// with no frame lofty would read, or one that ends early, is an
/// estimate; only an I/O failure is an error.
pub fn length_measured(path: &Path) -> std::io::Result<bool> {
    let mut reader = BufReader::new(File::open(path)?);
    match first_frame_counts_frames(&mut reader) {
        Err(error) if error.kind() == ErrorKind::UnexpectedEof => Ok(false),
        read => read,
    }
}

/// One MPEG audio frame header.
struct FrameHeader {
    /// 0 for MPEG 1, 1 for MPEG 2 and 2.5.
    version_index: usize,
    channel_mode: usize,
    len: u32,
}

impl FrameHeader {
    /// lofty's `Header::read`: `None` for a reserved version, layer or
    /// sample rate, or a free-format bitrate.
    fn read(data: u32) -> Option<Self> {
        let version = match (data >> 19) & 0b11 {
            0b11 => 0,
            0b10 => 1,
            0b00 => 2,
            _ => return None,
        };
        let version_index = usize::from(version != 0);
        let layer_index = match (data >> 17) & 0b11 {
            0b11 => 0,
            0b10 => 1,
            0b01 => 2,
            _ => return None,
        };
        let bitrate = BITRATES[version_index][layer_index][((data >> 12) & 0xF) as usize];
        if bitrate == 0 {
            return None;
        }
        let sample_rate = match (data >> 10) & 0b11 {
            0b11 => return None,
            index => SAMPLE_RATES[version][index as usize],
        };
        let padding = if (data >> 9) & 1 == 1 {
            PADDING[layer_index]
        } else {
            0
        };
        Some(Self {
            version_index,
            channel_mode: ((data >> 6) & 0b11) as usize,
            len: SAMPLES[layer_index][version_index] * bitrate * 125 / sample_rate + padding,
        })
    }

    /// Where a Xing, Info or VBRI header starts in the frame.
    fn data_start(&self) -> u64 {
        SIDE_INFORMATION[self.version_index][self.channel_mode] + 4
    }
}

fn read_u32<R: Read>(reader: &mut R) -> std::io::Result<u32> {
    let mut bytes = [0; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_be_bytes(bytes))
}

fn first_frame_counts_frames<R: Read + Seek>(reader: &mut R) -> std::io::Result<bool> {
    let mut byte = [0; 1];
    loop {
        reader.read_exact(&mut byte)?;
        if byte[0] != 0 {
            break;
        }
    }
    reader.seek(SeekFrom::Current(-1))?;
    let mut head = [0; 4];
    let frame = loop {
        reader.read_exact(&mut head)?;
        match head {
            [b'I', b'D', b'3', version] => {
                let mut rest = [0; 6];
                reader.read_exact(&mut rest)?;
                if !(2..=4).contains(&version) {
                    return Ok(false);
                }
                // The size is a syncsafe integer: seven bits a byte.
                let size = rest[2..]
                    .iter()
                    .fold(0_i64, |size, byte| (size << 7) | i64::from(byte & 0x7F));
                let footer = version >= 3 && rest[1] & 0x10 != 0;
                reader.seek(SeekFrom::Current(size + if footer { 10 } else { 0 }))?;
            }
            [b'A', b'P', b'E', b'T'] => return Ok(false),
            _ => {
                reader.seek(SeekFrom::Current(-4))?;
                match next_frame(reader)? {
                    Some(frame) => break frame,
                    None => return Ok(false),
                }
            }
        }
    };
    let (header, offset) = frame;
    reader.seek(SeekFrom::Start(offset + header.data_start()))?;
    let mut vbr = [0; 32];
    reader.read_exact(&mut vbr)?;
    let word = |at: usize| u32::from_be_bytes([vbr[at], vbr[at + 1], vbr[at + 2], vbr[at + 3]]);
    let (frames, size) = match &vbr[..4] {
        // Both the frame and byte count flags must be set.
        b"Xing" | b"Info" if vbr[7] & 0x03 == 0x03 => (word(8), word(12)),
        b"VBRI" => (word(14), word(10)),
        _ => return Ok(false),
    };
    Ok(frames > 0 && size > 0)
}

/// lofty's `find_next_frame`: the first frame sync whose header reads and
/// whose next frame shares its version, layer and sample rate, with its
/// offset. A sync whose header does not read is passed; one whose next
/// frame differs skips that frame's length.
fn next_frame<R: Read + Seek>(reader: &mut R) -> std::io::Result<Option<(FrameHeader, u64)>> {
    loop {
        let Some(start) = next_sync(reader)? else {
            return Ok(None);
        };
        reader.seek(SeekFrom::Start(start))?;
        let data = read_u32(reader)?;
        let Some(header) = FrameHeader::read(data) else {
            continue;
        };
        reader.seek(SeekFrom::Current(i64::from(header.len.saturating_sub(4))))?;
        let Ok(next) = read_u32(reader) else {
            return Ok(None);
        };
        reader.seek(SeekFrom::Current(-4))?;
        if data & HEADER_MASK == next & HEADER_MASK {
            return Ok(Some((header, start)));
        }
    }
}

/// The offset of the next byte pair holding a frame sync (eleven set
/// bits), from the reader's position.
fn next_sync<R: Read + Seek>(reader: &mut R) -> std::io::Result<Option<u64>> {
    let mut at = reader.stream_position()?;
    let mut previous: Option<u8> = None;
    let mut byte = [0; 1];
    loop {
        match reader.read(&mut byte)? {
            0 => return Ok(None),
            _ => {
                if previous == Some(0xFF) && byte[0] >> 5 == 0b111 {
                    return Ok(Some(at - 1));
                }
                previous = Some(byte[0]);
                at += 1;
            }
        }
    }
}
