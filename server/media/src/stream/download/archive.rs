//! Streamed ZIP framing for album downloads.
//!
//! Album archives store files as-is (audio is already compressed), so every
//! byte of the archive is known before the first one is sent: headers,
//! names and sizes fix the length, and only the CRC-32 of each file has to
//! be read from disk. The download route computes each file's CRC just
//! before its local header and streams the file body straight after, so an
//! archive is never assembled in memory or on disk.
//!
//! The CRC goes in the local header instead of a trailing data descriptor
//! because some unzip tools (Java's `ZipInputStream`, older macOS
//! utilities) refuse stored entries that use descriptors. ZIP64 fields
//! appear only when a file, an offset or the entry count outgrows the
//! classic format, which keeps ordinary albums readable everywhere.

/// Largest value a classic 32-bit field holds; reaching it needs ZIP64.
const MAX_U32: u64 = 0xFFFF_FFFF;
/// Largest entry count the classic end record holds.
const MAX_ENTRIES: usize = 0xFFFF;

const LOCAL_HEADER: u32 = 0x0403_4b50;
const CENTRAL_HEADER: u32 = 0x0201_4b50;
const END_OF_CENTRAL: u32 = 0x0605_4b50;
const ZIP64_END: u32 = 0x0606_4b50;
const ZIP64_LOCATOR: u32 = 0x0706_4b50;
const ZIP64_EXTRA_ID: u16 = 0x0001;

/// Version 2.0 (stored entries) or 4.5 (ZIP64).
const VERSION_CLASSIC: u16 = 20;
const VERSION_ZIP64: u16 = 45;
/// Made by Unix, so the permission bits below apply on extraction.
const MADE_BY_UNIX: u16 = 3 << 8;
/// General-purpose flag bit 11: names are UTF-8.
const FLAG_UTF8: u16 = 0x0800;
/// `-rw-r--r--` regular file, in the high half of the external attributes.
const UNIX_FILE_MODE: u32 = 0o100_644 << 16;

/// One file of the archive: its name inside the zip, its size, and its
/// modification time in DOS form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Path inside the archive, UTF-8.
    pub name: String,
    /// File length, bytes.
    pub size: u64,
    /// DOS time word (2-second resolution).
    pub dos_time: u16,
    /// DOS date word.
    pub dos_date: u16,
}

/// Where each entry's local header starts, plus the central directory's
/// offset and size. Computed once; the stream and the length both use it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// Local header offset per entry, in entry order.
    pub offsets: Vec<u64>,
    /// Offset of the central directory.
    pub central_offset: u64,
    /// Length of the central directory.
    pub central_len: u64,
    /// Total archive length, bytes.
    pub total_len: u64,
}

fn size_needs_zip64(entry: &Entry) -> bool {
    entry.size >= MAX_U32
}

/// Local header length for one entry, name and extra included.
fn local_header_len(entry: &Entry) -> u64 {
    let extra = if size_needs_zip64(entry) { 20 } else { 0 };
    30 + entry.name.len() as u64 + extra
}

/// How many 8-byte ZIP64 values the central record carries.
fn central_zip64_fields(entry: &Entry, offset: u64) -> u64 {
    let sizes = if size_needs_zip64(entry) { 2 } else { 0 };
    let offset = if offset >= MAX_U32 { 1 } else { 0 };
    sizes + offset
}

fn central_header_len(entry: &Entry, offset: u64) -> u64 {
    let fields = central_zip64_fields(entry, offset);
    let extra = if fields > 0 { 4 + 8 * fields } else { 0 };
    46 + entry.name.len() as u64 + extra
}

fn end_needs_zip64(count: usize, central_offset: u64, central_len: u64) -> bool {
    count >= MAX_ENTRIES || central_offset >= MAX_U32 || central_len >= MAX_U32
}

/// Lay the archive out: local header offsets, central directory span, and
/// the exact total length (the `Content-Length` of the download).
pub fn layout(entries: &[Entry]) -> Layout {
    let mut offsets = Vec::with_capacity(entries.len());
    let mut position = 0u64;
    for entry in entries {
        offsets.push(position);
        position = position
            .saturating_add(local_header_len(entry))
            .saturating_add(entry.size);
    }
    let central_offset = position;
    let central_len: u64 = entries
        .iter()
        .zip(&offsets)
        .map(|(entry, offset)| central_header_len(entry, *offset))
        .sum();
    let end_len = if end_needs_zip64(entries.len(), central_offset, central_len) {
        56 + 20 + 22
    } else {
        22
    };
    Layout {
        total_len: central_offset
            .saturating_add(central_len)
            .saturating_add(end_len),
        offsets,
        central_offset,
        central_len,
    }
}

fn put_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// A classic 32-bit field, or the ZIP64 marker when the value outgrows it.
fn clamp_u32(value: u64) -> u32 {
    u32::try_from(value)
        .ok()
        .filter(|small| u64::from(*small) < MAX_U32)
        .unwrap_or(u32::MAX)
}

/// The local file header that precedes an entry's bytes.
pub fn local_header(entry: &Entry, crc: u32) -> Vec<u8> {
    let zip64 = size_needs_zip64(entry);
    let mut out = Vec::with_capacity(local_header_len(entry) as usize);
    put_u32(&mut out, LOCAL_HEADER);
    put_u16(
        &mut out,
        if zip64 {
            VERSION_ZIP64
        } else {
            VERSION_CLASSIC
        },
    );
    put_u16(&mut out, FLAG_UTF8);
    put_u16(&mut out, 0); // stored
    put_u16(&mut out, entry.dos_time);
    put_u16(&mut out, entry.dos_date);
    put_u32(&mut out, crc);
    put_u32(&mut out, clamp_u32(entry.size));
    put_u32(&mut out, clamp_u32(entry.size));
    put_u16(&mut out, entry.name.len() as u16);
    put_u16(&mut out, if zip64 { 20 } else { 0 });
    out.extend_from_slice(entry.name.as_bytes());
    if zip64 {
        put_u16(&mut out, ZIP64_EXTRA_ID);
        put_u16(&mut out, 16);
        put_u64(&mut out, entry.size);
        put_u64(&mut out, entry.size);
    }
    out
}

/// The central directory and end records, once every CRC is known.
pub fn central_directory(entries: &[Entry], crcs: &[u32], layout: &Layout) -> Vec<u8> {
    let mut out = Vec::with_capacity(layout.central_len as usize + 98);
    for ((entry, crc), offset) in entries.iter().zip(crcs).zip(&layout.offsets) {
        let fields = central_zip64_fields(entry, *offset);
        let zip64 = fields > 0;
        let version = if zip64 {
            VERSION_ZIP64
        } else {
            VERSION_CLASSIC
        };
        put_u32(&mut out, CENTRAL_HEADER);
        put_u16(&mut out, MADE_BY_UNIX | VERSION_ZIP64);
        put_u16(&mut out, version);
        put_u16(&mut out, FLAG_UTF8);
        put_u16(&mut out, 0); // stored
        put_u16(&mut out, entry.dos_time);
        put_u16(&mut out, entry.dos_date);
        put_u32(&mut out, *crc);
        put_u32(&mut out, clamp_u32(entry.size));
        put_u32(&mut out, clamp_u32(entry.size));
        put_u16(&mut out, entry.name.len() as u16);
        put_u16(&mut out, if zip64 { (4 + 8 * fields) as u16 } else { 0 });
        put_u16(&mut out, 0); // comment
        put_u16(&mut out, 0); // disk
        put_u16(&mut out, 0); // internal attributes
        put_u32(&mut out, UNIX_FILE_MODE);
        put_u32(&mut out, clamp_u32(*offset));
        out.extend_from_slice(entry.name.as_bytes());
        if zip64 {
            put_u16(&mut out, ZIP64_EXTRA_ID);
            put_u16(&mut out, (8 * fields) as u16);
            if size_needs_zip64(entry) {
                put_u64(&mut out, entry.size);
                put_u64(&mut out, entry.size);
            }
            if *offset >= MAX_U32 {
                put_u64(&mut out, *offset);
            }
        }
    }
    let count = entries.len();
    if end_needs_zip64(count, layout.central_offset, layout.central_len) {
        let record_offset = layout.central_offset + layout.central_len;
        put_u32(&mut out, ZIP64_END);
        put_u64(&mut out, 44);
        put_u16(&mut out, MADE_BY_UNIX | VERSION_ZIP64);
        put_u16(&mut out, VERSION_ZIP64);
        put_u32(&mut out, 0);
        put_u32(&mut out, 0);
        put_u64(&mut out, count as u64);
        put_u64(&mut out, count as u64);
        put_u64(&mut out, layout.central_len);
        put_u64(&mut out, layout.central_offset);
        put_u32(&mut out, ZIP64_LOCATOR);
        put_u32(&mut out, 0);
        put_u64(&mut out, record_offset);
        put_u32(&mut out, 1);
    }
    let classic_count = u16::try_from(count)
        .ok()
        .filter(|small| usize::from(*small) < MAX_ENTRIES)
        .unwrap_or(u16::MAX);
    put_u32(&mut out, END_OF_CENTRAL);
    put_u16(&mut out, 0);
    put_u16(&mut out, 0);
    put_u16(&mut out, classic_count);
    put_u16(&mut out, classic_count);
    put_u32(&mut out, clamp_u32(layout.central_len));
    put_u32(&mut out, clamp_u32(layout.central_offset));
    put_u16(&mut out, 0); // comment
    out
}

/// DOS `(time, date)` words for a unix timestamp, in UTC. Times before
/// 1980 (the DOS epoch) read as 1980-01-01 and times past 2107 as the last
/// representable day.
pub fn dos_datetime(unix_seconds: i64) -> (u16, u16) {
    let Ok(moment) = time::OffsetDateTime::from_unix_timestamp(unix_seconds) else {
        return (0, (1 << 5) | 1);
    };
    let year = moment.year();
    if year < 1980 {
        return (0, (1 << 5) | 1);
    }
    if year > 2107 {
        return ((23 << 11) | (59 << 5) | 29, (127 << 9) | (12 << 5) | 31);
    }
    let date = (((year - 1980) as u16) << 9)
        | ((u8::from(moment.month()) as u16) << 5)
        | moment.day() as u16;
    let time = ((moment.hour() as u16) << 11)
        | ((moment.minute() as u16) << 5)
        | (moment.second() as u16 / 2);
    (time, date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    fn crc(bytes: &[u8]) -> u32 {
        let mut crc = flate2::Crc::new();
        crc.update(bytes);
        crc.sum()
    }

    /// Frame files exactly the way the download stream does, then read the
    /// result back with an independent zip reader (which checks CRCs).
    #[test]
    fn framed_archive_matches_its_length_and_reads_back() {
        let files: Vec<(&str, &[u8])> = vec![
            ("01 Opener.flac", b"first file bytes"),
            ("02 Sp\u{e9}cial \u{2013} Track.mp3", b""),
            ("03 Closer.ogg", &[7u8; 70_000]),
        ];
        let (time, date) = dos_datetime(1_700_000_000);
        let entries: Vec<Entry> = files
            .iter()
            .map(|(name, bytes)| Entry {
                name: (*name).to_owned(),
                size: bytes.len() as u64,
                dos_time: time,
                dos_date: date,
            })
            .collect();
        let layout = layout(&entries);
        let mut archive = Vec::new();
        let mut crcs = Vec::new();
        for (entry, (_, bytes)) in entries.iter().zip(&files) {
            assert_eq!(archive.len() as u64, layout.offsets[crcs.len()]);
            let sum = crc(bytes);
            archive.extend(local_header(entry, sum));
            archive.extend_from_slice(bytes);
            crcs.push(sum);
        }
        archive.extend(central_directory(&entries, &crcs, &layout));
        assert_eq!(archive.len() as u64, layout.total_len);

        let mut reader =
            ::zip::ZipArchive::new(std::io::Cursor::new(archive)).expect("archive parses");
        assert_eq!(reader.len(), files.len());
        for (index, (name, bytes)) in files.iter().enumerate() {
            let mut file = reader.by_index(index).expect("entry opens");
            assert_eq!(file.name(), *name);
            let mut read = Vec::new();
            file.read_to_end(&mut read)
                .expect("entry reads with a good crc");
            assert_eq!(read.as_slice(), *bytes);
        }
    }

    #[test]
    fn dos_dates_clamp_to_the_representable_range() {
        // 2023-11-14 22:13:20 UTC.
        let (time, date) = dos_datetime(1_700_000_000);
        assert_eq!(date, (43 << 9) | (11 << 5) | 14);
        assert_eq!(time, (22 << 11) | (13 << 5) | 10);
        assert_eq!(dos_datetime(0), (0, (1 << 5) | 1));
    }

    #[test]
    fn big_files_switch_to_zip64_fields() {
        let entry = Entry {
            name: "huge.wav".to_owned(),
            size: 5_000_000_000,
            dos_time: 0,
            dos_date: 33,
        };
        let header = local_header(&entry, 1);
        assert_eq!(header.len() as u64, local_header_len(&entry));
        assert_eq!(&header[18..26], &[0xFF; 8]);
        let layout = layout(std::slice::from_ref(&entry));
        let central = central_directory(std::slice::from_ref(&entry), &[1], &layout);
        assert_eq!(
            central.len() as u64,
            layout.total_len - layout.central_offset,
            "central directory plus ZIP64 end records match the layout"
        );
    }
}
