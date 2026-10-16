//! Archive safety: traversal, zip-slip, symlink, size, and bomb limits.
//!
//! Drop and acquisition inputs may arrive as archives. Before any
//! entry is extracted, the manifest passes this pure validator:
//! traversal and zip-slip entries are blocked, symlink entries are
//! refused (the no-symlinks rule extends to archives), and file count, per-file size,
//! total size, nesting depth, and compression ratio are bounded so a
//! decompression bomb cannot exhaust the staging disk.

use super::PublishError;

/// One archive manifest entry, as reported by the extractor before it
/// writes anything.
#[derive(Debug, Clone)]
pub struct ArchiveEntry {
    /// Raw entry name inside the archive.
    pub name: String,
    /// Uncompressed size in bytes.
    pub size: u64,
    /// Compressed size in bytes (0 when unknown).
    pub compressed_size: u64,
    /// Whether the entry is a symlink or hardlink.
    pub is_link: bool,
    /// Whether the entry is a directory.
    pub is_dir: bool,
}

/// Bounds for one archive. Defaults are conservative; profiles may
/// narrow them but never widen past these ceilings.
#[derive(Debug, Clone)]
pub struct ArchivePolicy {
    /// Maximum entries per archive.
    pub max_files: usize,
    /// Maximum total uncompressed bytes.
    pub max_total_bytes: u64,
    /// Maximum uncompressed bytes per file.
    pub max_file_bytes: u64,
    /// Maximum uncompressed/compressed ratio before a bomb block.
    pub max_ratio: u64,
    /// Maximum directory nesting depth.
    pub max_depth: usize,
}

impl Default for ArchivePolicy {
    fn default() -> Self {
        Self {
            max_files: 10_000,
            max_total_bytes: 4_294_967_296,
            max_file_bytes: 1_073_741_824,
            max_ratio: 100,
            max_depth: 16,
        }
    }
}

/// Why an archive was blocked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveBlock {
    /// Entry escapes the destination (`..`, absolute path, drive prefix).
    Traversal(String),
    /// Entry normalizes outside the destination after cleaning.
    ZipSlip(String),
    /// Symlink or hardlink entry.
    Link(String),
    /// Too many entries.
    TooManyFiles(usize),
    /// One file exceeds the per-file cap.
    FileTooLarge(String),
    /// Total uncompressed size exceeds the cap.
    TotalTooLarge(u64),
    /// Compression ratio looks like a bomb.
    BombRatio(String),
    /// Nesting exceeds the depth cap.
    TooDeep(String),
    /// Empty or missing entry name.
    EmptyName,
}

/// Accepted manifest summary.
#[derive(Debug, Clone)]
pub struct ArchiveReport {
    /// Number of file entries (directories excluded).
    pub files: usize,
    /// Total uncompressed bytes.
    pub total_bytes: u64,
}

/// Validate an archive manifest before extraction. The first blocking
/// entry fails the whole archive; extraction of a blocked archive
/// must not start.
///
/// No production archive-ingest path exists yet: nothing in the
/// tree extracts archives, so nothing calls this outside the
/// tests. The first ingest path (drop or acquisition unpacking)
/// must validate its manifest through this gate before it writes
/// any entry.
pub fn validate_archive(
    entries: &[ArchiveEntry],
    policy: &ArchivePolicy,
) -> Result<ArchiveReport, PublishError> {
    let files = entries.iter().filter(|entry| !entry.is_dir).count();
    if files > policy.max_files {
        return Err(block(ArchiveBlock::TooManyFiles(files)));
    }
    let mut total: u64 = 0;
    for entry in entries {
        check_entry(entry, policy)?;
        if !entry.is_dir {
            total = total.saturating_add(entry.size);
        }
    }
    if total > policy.max_total_bytes {
        return Err(block(ArchiveBlock::TotalTooLarge(total)));
    }
    Ok(ArchiveReport {
        files,
        total_bytes: total,
    })
}

fn check_entry(entry: &ArchiveEntry, policy: &ArchivePolicy) -> Result<(), PublishError> {
    if entry.name.is_empty() {
        return Err(block(ArchiveBlock::EmptyName));
    }
    if entry.is_link {
        return Err(block(ArchiveBlock::Link(entry.name.clone())));
    }
    if is_absolute_or_driven(&entry.name) {
        return Err(block(ArchiveBlock::Traversal(entry.name.clone())));
    }
    let cleaned = clean_entry_name(&entry.name);
    if cleaned.is_empty() {
        return Err(block(ArchiveBlock::ZipSlip(entry.name.clone())));
    }
    let depth = cleaned.split('/').count();
    if depth > policy.max_depth {
        return Err(block(ArchiveBlock::TooDeep(entry.name.clone())));
    }
    if !entry.is_dir {
        if entry.size > policy.max_file_bytes {
            return Err(block(ArchiveBlock::FileTooLarge(entry.name.clone())));
        }
        if entry.compressed_size > 0 {
            let ratio = entry.size / entry.compressed_size.max(1);
            if ratio > policy.max_ratio && entry.size > 1024 * 1024 {
                return Err(block(ArchiveBlock::BombRatio(entry.name.clone())));
            }
        }
    }
    Ok(())
}

/// Reject absolute paths and Windows drive/UNC prefixes outright.
fn is_absolute_or_driven(name: &str) -> bool {
    if name.starts_with('/') || name.starts_with('\\') {
        return true;
    }
    let bytes = name.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        return true;
    }
    if name.starts_with("\\\\") {
        return true;
    }
    false
}

/// Normalize separators and resolve `.`/`..` lexically; `None` means
/// the entry escapes the destination root.
fn clean_entry_name(name: &str) -> String {
    let unified = name.replace('\\', "/");
    let mut parts: Vec<&str> = Vec::new();
    for raw in unified.split('/') {
        match raw {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() {
                    return String::new();
                }
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

fn block(block: ArchiveBlock) -> PublishError {
    let text = match &block {
        ArchiveBlock::Traversal(name) => format!("traversal entry: {name}"),
        ArchiveBlock::ZipSlip(name) => format!("zip-slip entry: {name}"),
        ArchiveBlock::Link(name) => format!("link entry refused: {name}"),
        ArchiveBlock::TooManyFiles(count) => format!("too many entries: {count}"),
        ArchiveBlock::FileTooLarge(name) => format!("entry too large: {name}"),
        ArchiveBlock::TotalTooLarge(total) => format!("archive too large: {total} bytes"),
        ArchiveBlock::BombRatio(name) => format!("bomb-like ratio: {name}"),
        ArchiveBlock::TooDeep(name) => format!("entry too deep: {name}"),
        ArchiveBlock::EmptyName => "empty entry name".to_string(),
    };
    PublishError::Archive(text)
}
