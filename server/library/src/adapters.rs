//! Seam implementations between the library modules.
//!
//! Each adapter is narrow by design: it translates at a module boundary
//! and owns no domain logic.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::publish::PublishError;
use super::publish::planner::SpaceProbe;
use super::scan::seams::{ScannedTags, TagReadError, TagReader};

// ---------------------------------------------------------------------------
// Scan -> tags: read-only tag access over the tags module.
// ---------------------------------------------------------------------------

/// Read-only tag reader over [`crate::tags`]. Never writes:
/// tags and stream properties come from the container headers (no audio
/// decode), and every failure maps onto the scan seam's two errors. A
/// parser panic on a malformed file is caught and reads as a fatal tag
/// failure for that file instead of taking the scan worker down.
pub struct LoftyTagReader;

impl TagReader for LoftyTagReader {
    fn read_tags(&self, path: &Path) -> Result<ScannedTags, TagReadError> {
        let format = super::tags::format_for_path(path).map_err(|_| TagReadError::Fatal)?;
        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::tags::read::read_scan_metadata(path, format)
        }));
        match read {
            Ok(Ok((tag, header))) => Ok(ScannedTags { tag, header }),
            Ok(Err(error)) => Err(map_tag_read_error(error)),
            Err(_) => {
                tracing::error!(path = %path.display(), "tag parser panicked; file skipped");
                Err(TagReadError::Fatal)
            }
        }
    }
}

/// Map a tag failure onto the scan seam. I/O races (a file vanishing
/// mid-read, contended disk) defer with re-offer; unrecognized and
/// unparseable files are fatal for this file.
fn map_tag_read_error(error: super::tags::TagsError) -> TagReadError {
    match error {
        super::tags::TagsError::Io { .. } => TagReadError::Deferred,
        _ => TagReadError::Fatal,
    }
}

// ---------------------------------------------------------------------------
// Publish -> filesystem: free-space probe over statvfs.
// ---------------------------------------------------------------------------

/// Free-space probe over `statvfs` on each root directory. The root
/// lookup re-reads the shared registry every call, never a captured
/// snapshot.
pub struct FsSpaceProbe {
    root_dirs: Arc<dyn Fn() -> HashMap<String, PathBuf> + Send + Sync>,
}

impl FsSpaceProbe {
    pub fn new(root_dirs: Arc<dyn Fn() -> HashMap<String, PathBuf> + Send + Sync>) -> Self {
        Self { root_dirs }
    }
}

impl SpaceProbe for FsSpaceProbe {
    fn free_bytes(&self, root_id: &str) -> Result<u64, PublishError> {
        let dirs = (self.root_dirs)();
        let dir = dirs
            .get(root_id)
            .ok_or_else(|| PublishError::UnsafePath(format!("unknown root {root_id}").into()))?;
        free_bytes_for(dir)
    }
}

pub fn free_bytes_for(dir: &Path) -> Result<u64, PublishError> {
    let text = dir.to_string_lossy().into_owned();
    let cstr = std::ffi::CString::new(text)
        .map_err(|_| PublishError::UnsafePath("root path holds a NUL byte".into()))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // statvfs only reads filesystem metadata; the path is already sandbox-checked.
    let status = unsafe { libc::statvfs(cstr.as_ptr(), &mut stat) };
    if status != 0 {
        return Err(PublishError::Io(
            format!("cannot stat free space under {}", dir.display()).into(),
        ));
    }
    // The field widths differ by platform (u32 blocks on macOS).
    #[allow(clippy::unnecessary_cast)]
    let free = stat.f_bavail as u64 * stat.f_frsize as u64;
    Ok(free)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../tests/fixtures/library")
            .join(name)
    }

    /// The scan reads the same tags the full read does, and its header
    /// duration agrees with the decode-counted probe, without decoding.
    #[test]
    fn scan_read_matches_full_read() {
        let reader = LoftyTagReader;
        for name in [
            "flac_full_01.flac",
            "flac_no_tags.flac",
            "mp3_full_01.mp3",
            "m4a_full_01.m4a",
            "management_full.ogg",
            "management_full.opus",
            "management_full.wav",
            "management_full.aac",
        ] {
            let path = fixture(name);
            let format = super::super::tags::format_for_path(&path).expect("fixture format");
            let scanned = reader.read_tags(&path).expect("scan read");
            let full = super::super::tags::read::read_tag_only(&path, format).expect("full read");
            assert_eq!(scanned.tag, full, "fixture {name}");
            let probed = super::super::tags::probe(&path)
                .expect("probe")
                .duration_seconds;
            let header = scanned.header.duration_seconds.expect("header duration");
            assert!(
                (header - probed).abs() < 0.5,
                "fixture {name}: {header} vs {probed}"
            );
        }
        assert_eq!(
            reader.read_tags(&fixture("management_full.wma")),
            Err(TagReadError::Fatal)
        );
        // A vanished file is an I/O failure: deferred, offered again.
        assert_eq!(
            reader.read_tags(Path::new("/nonexistent-missing-file.mp3")),
            Err(TagReadError::Deferred)
        );
    }
}
