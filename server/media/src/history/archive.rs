//! Reading a ListenBrainz export ZIP, which is hostile input.
//!
//! Only entries named `listens/<4 digits>/<1 or 2 digits>.jsonl` are read:
//! every other entry, a traversal name or a link among them, is skipped by
//! its name alone. An entry is read into memory, one at a time, up to
//! [`MAX_MONTH_BYTES`] inflated; one that says it is larger, or turns out
//! larger, is skipped. Nothing is ever written to disk from an archive.

use std::fs::File;
use std::io::Read as _;
use std::path::Path;

use droppedneedle_providers::listenbrainz::history::{HistoryListen, parse_line};

/// The most entries an archive may list. A ListenBrainz export holds one
/// file a month and a few others.
pub const MAX_ENTRIES: usize = 4096;
/// The most one month file may inflate to, in bytes.
pub const MAX_MONTH_BYTES: u64 = 64 * (1 << 20);
/// The most listens one month file may add.
pub const MAX_MONTH_LISTENS: usize = 100_000;

/// Why an archive cannot be read at all.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// The file is not a ZIP this reader can open.
    #[error("the file is not a readable ZIP archive")]
    Unreadable,
    /// The archive lists more entries than an export holds.
    #[error("the archive lists more than {MAX_ENTRIES} entries")]
    TooManyEntries,
    /// The file could not be opened.
    #[error("the archive could not be opened: {0}")]
    Io(#[from] std::io::Error),
}

/// One month file of an archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Month {
    /// The year in its name.
    pub year: u16,
    /// The month in its name, 1 to 12.
    pub month: u8,
    /// Its place in the archive.
    index: usize,
}

/// An open export archive and its month files, oldest first.
pub struct Export {
    zip: zip::ZipArchive<File>,
    months: Vec<Month>,
}

/// What one month file gave.
#[derive(Debug, Default)]
pub struct MonthListens {
    /// The listens read from it.
    pub listens: Vec<HistoryListen>,
    /// Lines that were not a listen, or past [`MAX_MONTH_LISTENS`].
    pub skipped: usize,
    /// Why the whole file was passed over, when it was.
    pub passed_over: Option<&'static str>,
}

impl Export {
    /// Open the archive at `path` and find its month files.
    pub fn open(path: &Path) -> Result<Self, ArchiveError> {
        let zip = zip::ZipArchive::new(File::open(path)?).map_err(|_| ArchiveError::Unreadable)?;
        if zip.len() > MAX_ENTRIES {
            return Err(ArchiveError::TooManyEntries);
        }
        let mut months: Vec<Month> = (0..zip.len())
            .filter_map(|index| {
                let name = zip.name_for_index(index)?.ok()?;
                let (year, month) = month_name(&name)?;
                Some(Month { year, month, index })
            })
            .collect();
        months.sort();
        Ok(Self { zip, months })
    }

    /// The month files, oldest first.
    pub fn months(&self) -> &[Month] {
        &self.months
    }

    /// Read the month file at `position` in [`Export::months`]. `None`
    /// past the last one.
    pub fn read(&mut self, position: usize) -> Option<MonthListens> {
        let month = *self.months.get(position)?;
        Some(self.read_month(month))
    }

    fn read_month(&mut self, month: Month) -> MonthListens {
        let passed_over = |why| MonthListens {
            passed_over: Some(why),
            ..MonthListens::default()
        };
        let Ok(mut entry) = self.zip.by_index(month.index) else {
            return passed_over("the entry could not be opened");
        };
        if !entry.is_file() {
            return passed_over("the entry is not a file");
        }
        if entry.size() > MAX_MONTH_BYTES {
            return passed_over("the entry is too large");
        }
        let mut bytes = Vec::new();
        if (&mut entry)
            .take(MAX_MONTH_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_err()
        {
            return passed_over("the entry did not inflate");
        }
        if bytes.len() as u64 > MAX_MONTH_BYTES {
            return passed_over("the entry is too large");
        }
        let mut read = MonthListens::default();
        for line in bytes.split(|byte| *byte == b'\n') {
            let Ok(line) = std::str::from_utf8(line) else {
                read.skipped += 1;
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            match parse_line(line) {
                Ok(listen) if read.listens.len() < MAX_MONTH_LISTENS => read.listens.push(listen),
                Ok(_) | Err(_) => read.skipped += 1,
            }
        }
        read
    }
}

/// The year and month a month file's name gives:
/// `listens/<4 digits>/<1 or 2 digits>.jsonl`, the month 1 to 12.
pub fn month_name(name: &str) -> Option<(u16, u8)> {
    let rest = name.strip_prefix("listens/")?;
    let (year, file) = rest.split_once('/')?;
    let month = file.strip_suffix(".jsonl")?;
    let digits = |text: &str, lengths: std::ops::RangeInclusive<usize>| {
        lengths.contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_digit())
    };
    if !digits(year, 4..=4) || !digits(month, 1..=2) {
        return None;
    }
    let month: u8 = month.parse().ok()?;
    (1..=12)
        .contains(&month)
        .then_some((year.parse().ok()?, month))
}
