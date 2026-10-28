//! Release quarantine: blocklist plus sandboxed file holding.
//!
//! A release that fails verification is blocklisted by source identity so
//! failover and future searches skip it; entries expire after 7 days and
//! every write prunes the expired rows so the TTL self-heal lands on disk.
//! A reason that says nothing against the source is never quarantined: the
//! backoff'd auto-retry re-grabs once the environment recovers.
//!
//! Suspect files themselves move into a quarantine directory only under
//! tests, which point [`QuarantineDir`] at a throwaway sandbox.
//!
//! Every source journals what it fetched on the attempt's handle (soulseek
//! the peer and files, Usenet and plugin sources the release identity), so
//! one write path blocklists any source's failed release and one filter,
//! [`Blocklist`], keeps it out of every source's search and pick.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::source_tag::SourceTag;
use super::sources::SourceHandle;

/// Blocklist entry lifetime: 7 days.
pub const QUARANTINE_TTL_SECONDS: f64 = 7.0 * 24.0 * 3600.0;

/// Quarantine reasons. Mirrors the `download_quarantine.reason` CHECK.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineReason {
    /// Post-download verification failed.
    VerifyFailed,
    /// File is corrupt or unreadable.
    Corrupt,
    /// Audio fingerprint does not match the candidate.
    FingerprintMismatch,
    /// Duration gate failed on a whole-release download.
    DurationMismatch,
    /// The download itself failed terminally.
    DownloadFailed,
    /// Operator blocklisted the release by hand.
    Manual,
}

impl QuarantineReason {
    /// Wire string stored in `download_quarantine.reason`.
    pub fn as_str(self) -> &'static str {
        match self {
            QuarantineReason::VerifyFailed => "verify_failed",
            QuarantineReason::Corrupt => "corrupt",
            QuarantineReason::FingerprintMismatch => "fingerprint_mismatch",
            QuarantineReason::DurationMismatch => "duration_mismatch",
            QuarantineReason::DownloadFailed => "download_failed",
            QuarantineReason::Manual => "manual",
        }
    }
}

/// Canonicalize a soulseek identity for blocklist matching.
///
/// Backslashes become forward
/// slashes and the `username/file` halves split on the first slash, so
/// rows written by older versions cannot evade a match.
pub fn canonical_soulseek_identity(identity: &str) -> String {
    let slashed = identity.replace('\\', "/");
    match slashed.split_once('/') {
        Some((user, file)) => format!(
            "{}/{}",
            user.trim().to_lowercase(),
            file.trim().to_lowercase()
        ),
        None => slashed.trim().to_lowercase(),
    }
}

/// Blocklist identities for a failed fetch, from the journaled handle.
/// Soulseek identifies by peer + filename (canonicalized, so older rows
/// cannot evade the match), one row per enqueued file; Usenet and plugin
/// sources by the release identity the handle carries. A handle with
/// neither yields no identities and nothing is recorded.
pub fn failover_identities(handle: &SourceHandle) -> Vec<String> {
    match handle.source {
        SourceTag::Soulseek => {
            if handle.username.is_empty() {
                return Vec::new();
            }
            handle
                .filenames
                .iter()
                .filter(|file| !file.is_empty())
                .map(|file| soulseek_file_identity(&handle.username, file))
                .collect()
        }
        SourceTag::Usenet | SourceTag::Plugin(_) => {
            if handle.release_identity.is_empty() {
                Vec::new()
            } else {
                vec![handle.release_identity.clone()]
            }
        }
    }
}

/// The blocklist identity of one soulseek file.
fn soulseek_file_identity(username: &str, filename: &str) -> String {
    canonical_soulseek_identity(&format!("{username}/{filename}"))
}

/// One source's live blocklist: the filter every source runs its search
/// results and its picks through.
#[derive(Debug, Clone, Default)]
pub struct Blocklist {
    identities: HashSet<String>,
}

impl Blocklist {
    /// The rows of `live` that belong to `source`.
    pub fn for_source(source: &SourceTag, live: Vec<(String, String)>) -> Self {
        let identities = live
            .into_iter()
            .filter(|(tag, _)| *source == *tag)
            .map(|(_, identity)| match source {
                SourceTag::Soulseek => canonical_soulseek_identity(&identity),
                _ => identity,
            })
            .collect();
        Self { identities }
    }

    /// Whether the Usenet or plugin release with this identity is blocked.
    pub fn blocks(&self, identity: &str) -> bool {
        self.identities.contains(identity)
    }

    /// Whether a soulseek peer's file is blocked.
    pub fn blocks_file(&self, username: &str, filename: &str) -> bool {
        self.blocks(&soulseek_file_identity(username, filename))
    }
}

/// A sandboxed directory for quarantined files.
///
/// Production moves suspect files aside through the import pipeline; this
/// helper exists so tests can prove the move without touching the real
/// library. The directory is created on first use.
#[derive(Debug, Clone)]
pub struct QuarantineDir {
    root: PathBuf,
}

impl QuarantineDir {
    /// Point quarantine file moves at a sandbox directory.
    pub fn sandbox(root: PathBuf) -> Self {
        Self { root }
    }

    /// Move `path` into the sandbox, returning its new location. The file
    /// keeps its name under a unique entry directory so two files with the
    /// same basename can never collide. The stamp is a single path
    /// component: separators or parent markers are refused so a crafted
    /// stamp cannot escape the sandbox. A cross-filesystem move falls back
    /// to copy-plus-remove.
    pub fn hold_file(&self, path: &Path, stamp: &str) -> Result<PathBuf, QuarantineFileError> {
        if stamp.is_empty()
            || stamp.contains(['/', '\\'])
            || stamp
                .split(std::path::MAIN_SEPARATOR)
                .any(|part| part == "..")
            || stamp == ".."
        {
            return Err(QuarantineFileError::InvalidStamp {
                stamp: stamp.to_owned(),
            });
        }
        let name = path
            .file_name()
            .ok_or_else(|| QuarantineFileError::NoName {
                path: path.to_path_buf(),
            })?;
        let entry = self.root.join(format!("{stamp}-quarantined"));
        std::fs::create_dir_all(&entry).map_err(|source| QuarantineFileError::Io {
            path: entry.clone(),
            detail: source.to_string(),
        })?;
        let destination = entry.join(name);
        match std::fs::rename(path, &destination) {
            Ok(()) => Ok(destination),
            Err(rename_error) => {
                // Cross-filesystem (EXDEV) or otherwise unmovable: copy the
                // bytes, then remove the source.
                std::fs::copy(path, &destination).map_err(|source| QuarantineFileError::Io {
                    path: destination.clone(),
                    detail: format!(
                        "rename failed ({rename_error}) and copy fallback failed: {source}"
                    ),
                })?;
                std::fs::remove_file(path).map_err(|source| QuarantineFileError::Io {
                    path: path.to_path_buf(),
                    detail: source.to_string(),
                })?;
                Ok(destination)
            }
        }
    }
}

/// Sandboxed file-hold failures.
#[derive(Debug, thiserror::Error)]
pub enum QuarantineFileError {
    /// The path has no file name to preserve.
    #[error("quarantine path has no file name: {}", path.display())]
    NoName {
        /// Offending path.
        path: PathBuf,
    },
    /// The entry stamp would escape the sandbox.
    #[error("quarantine stamp is not a single path component: {stamp}")]
    InvalidStamp {
        /// Offending stamp.
        stamp: String,
    },
    /// The move failed.
    #[error("quarantine move failed for {}: {detail}", path.display())]
    Io {
        /// Path being created or written.
        path: PathBuf,
        /// Underlying OS error text.
        detail: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soulseek_identity_canonicalizes() {
        assert_eq!(
            canonical_soulseek_identity("Peer\\Music\\Track.Flac"),
            "peer/music/track.flac"
        );
        assert_eq!(canonical_soulseek_identity("lone"), "lone");
    }
}
