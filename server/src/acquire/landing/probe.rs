//! Probe: what actually landed on disk.
//!
//! The download client reports files or a job folder; the probe walks
//! them (never following symlinks), sorts audio from everything else, and
//! reads each audio file's tags and stream header the way the library
//! scan does: container headers only, no decode. Blocking work: callers
//! run it on a blocking thread.

use std::path::{Path, PathBuf};

use crate::library::tags::read::{HeaderInfo, read_scan_metadata};
use crate::library::tags::{AudioFormat, AudioTag, format_for_path};

/// How deep the probe walks a job folder (`Album/CD1/track.flac` is 2).
const MAX_DEPTH: usize = 4;
/// More files than this in one landing is not an album download.
const MAX_FILES: usize = 2_000;

/// One readable audio file.
#[derive(Debug, Clone)]
pub struct LandedFile {
    pub path: PathBuf,
    pub format: AudioFormat,
    pub tag: AudioTag,
    pub header: HeaderInfo,
    pub size_bytes: u64,
}

impl LandedFile {
    /// File name for logs and held rows (never the full peer path).
    pub fn file_name(&self) -> String {
        file_name(&self.path)
    }

    /// The file's quality tier.
    pub fn tier(&self) -> &'static str {
        super::quality::tier_for(
            self.format.as_str(),
            self.header.bitrate_kbps,
            self.header.bit_depth,
        )
    }
}

/// Everything the probe saw.
#[derive(Debug, Clone, Default)]
pub struct Landing {
    /// Readable audio, in path order.
    pub audio: Vec<LandedFile>,
    /// Audio files whose tags or header could not be read.
    pub unreadable: Vec<PathBuf>,
    /// Audio files named as samples (teaser clips, not tracks).
    pub samples: Vec<PathBuf>,
    /// Paths the client reported that do not exist.
    pub missing: Vec<PathBuf>,
    /// Any other file (cover art, cue sheets, logs).
    pub other: Vec<PathBuf>,
    /// The walk stopped at [`MAX_FILES`].
    pub truncated: bool,
}

impl Landing {
    /// Nothing at all exists where the client said the files are.
    pub fn nothing_found(&self) -> bool {
        self.audio.is_empty()
            && self.unreadable.is_empty()
            && self.samples.is_empty()
            && self.other.is_empty()
    }
}

/// Walk and read the reported paths. Blocking.
pub fn probe(paths: &[PathBuf]) -> Landing {
    let mut files = Vec::new();
    let mut landing = Landing::default();
    for path in paths {
        collect(path, 0, &mut files, &mut landing);
    }
    files.sort();
    files.dedup();
    for path in files {
        if landing.audio.len() + landing.unreadable.len() >= MAX_FILES {
            landing.truncated = true;
            break;
        }
        let Ok(format) = format_for_path(&path) else {
            landing.other.push(path);
            continue;
        };
        if is_sample(&path) {
            landing.samples.push(path);
            continue;
        }
        match read_one(&path, format) {
            Some(file) => landing.audio.push(file),
            None => landing.unreadable.push(path),
        }
    }
    landing
}

fn collect(path: &Path, depth: usize, files: &mut Vec<PathBuf>, landing: &mut Landing) {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(_) => {
            if depth == 0 {
                landing.missing.push(path.to_path_buf());
            }
            return;
        }
    };
    if meta.file_type().is_symlink() {
        tracing::info!(file = %file_name(path), "landing probe skips a symlink");
        return;
    }
    if meta.is_file() {
        files.push(path.to_path_buf());
        return;
    }
    if !meta.is_dir() || depth >= MAX_DEPTH || files.len() >= MAX_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        tracing::warn!(dir = %file_name(path), "landing probe cannot read a folder");
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        collect(&entry.path(), depth + 1, files, landing);
    }
}

/// Tags and header for one file. A parser panic on a malformed file
/// counts as unreadable instead of taking the worker down.
fn read_one(path: &Path, format: AudioFormat) -> Option<LandedFile> {
    let size_bytes = std::fs::metadata(path).map(|meta| meta.len()).ok()?;
    let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        read_scan_metadata(path, format)
    }));
    match read {
        Ok(Ok((tag, header))) => Some(LandedFile {
            path: path.to_path_buf(),
            format,
            tag,
            header,
            size_bytes,
        }),
        Ok(Err(error)) => {
            tracing::info!(file = %file_name(path), %error, "landed file is unreadable");
            None
        }
        Err(_) => {
            tracing::warn!(file = %file_name(path), "tag parser panicked on a landed file");
            None
        }
    }
}

/// A file named with the standalone word "sample" (not "sampler",
/// not "sample rate"), as Lidarr's `NotSample` and v2's sample spec.
pub fn is_sample(path: &Path) -> bool {
    let stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let words = super::specs::words(&stem);
    words.iter().enumerate().any(|(index, word)| {
        word == "sample" && words.get(index + 1).map(String::as_str) != Some("rate")
    })
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn samples_are_whole_words() {
        assert!(is_sample(Path::new("/d/album-sample.flac")));
        assert!(is_sample(Path::new("/d/Sample.mp3")));
        assert!(!is_sample(Path::new("/d/01 Sampler.flac")));
        assert!(!is_sample(Path::new("/d/sample rate test.flac")));
    }
}
