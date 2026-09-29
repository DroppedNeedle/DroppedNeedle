//! Release quarantine: blocklist plus sandboxed file holding.
//!
//! Ports the `download_quarantine` semantics from `download_store.py`.
//! A release that fails verification is blocklisted by source identity so
//! failover and future searches skip it; entries expire after 7 days and
//! every write prunes the expired rows so the TTL self-heal lands on disk.
//! Local faults (disk full, mount gone) are never quarantined: the
//! backoff'd auto-retry re-grabs once the environment recovers.
//!
//! Suspect files themselves move into a quarantine directory only under
//! tests, which point [`QuarantineDir`] at a throwaway sandbox.
//!
//! Coverage note: soulseek consults the live blocklist at enqueue; the
//! usenet consult is deferred until release identity rides the handle
//! (failover records job-name rows for audit until then). Wire the usenet
//! consult the moment the handle carries release identity.

use std::path::{Path, PathBuf};

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

/// SABnzbd failure substrings that mean a local/environment fault, not a
/// bad release. Never blocklist these; auto-retry re-grabs once the
/// environment recovers. Ported from `_LOCAL_FAULT_MARKERS`.
pub const LOCAL_FAULT_MARKERS: &[&str] = &[
    "disk is full",
    "disk full",
    "no space",
    "not enough disk",
    "write error",
    "failed moving",
    "moving failed",
    "permission denied",
    "cannot write",
    "could not create",
    "read-only file system",
];

/// True when a client failure message describes the local environment
/// rather than the release.
pub fn is_local_fault(message: Option<&str>) -> bool {
    let low = message.unwrap_or_default().to_lowercase();
    LOCAL_FAULT_MARKERS.iter().any(|mark| low.contains(mark))
}

/// Canonicalize a soulseek identity for blocklist matching.
///
/// Ports `canonical_soulseek_identity`: backslashes become forward
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
/// cannot evade the match), one row per enqueued file; usenet identifies
/// by the deterministic job name, which scopes the row to this task's
/// failover walk until release identity rides the handle (later stage).
/// Empty handles yield no identities and are never recorded.
pub fn failover_identities(
    source: &str,
    username: &str,
    filenames: &[String],
    job_name: &str,
) -> Vec<String> {
    match source {
        "soulseek" => {
            if username.is_empty() {
                return Vec::new();
            }
            filenames
                .iter()
                .filter(|file| !file.is_empty())
                .map(|file| canonical_soulseek_identity(&format!("{username}/{file}")))
                .collect()
        }
        "usenet" => {
            if job_name.is_empty() {
                Vec::new()
            } else {
                vec![job_name.to_owned()]
            }
        }
        _ => Vec::new(),
    }
}

/// Whether a soulseek hit is blocklisted: its canonical peer/file identity
/// appears in the live set.
pub fn soulseek_hit_quarantined(username: &str, filename: &str, live: &[(String, String)]) -> bool {
    let identity = canonical_soulseek_identity(&format!("{username}/{filename}"));
    live.iter()
        .any(|(source, blocked)| source == "soulseek" && *blocked == identity)
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
    fn local_faults_never_quarantine() {
        assert!(is_local_fault(Some("SABnzbd: Disk is Full, pausing")));
        assert!(is_local_fault(Some(
            "failed moving to library: permission denied"
        )));
        assert!(!is_local_fault(Some("CRC mismatch in segment 4")));
        assert!(!is_local_fault(None));
    }

    #[test]
    fn soulseek_identity_canonicalizes() {
        assert_eq!(
            canonical_soulseek_identity("Peer\\Music\\Track.Flac"),
            "peer/music/track.flac"
        );
        assert_eq!(canonical_soulseek_identity("lone"), "lone");
    }
}
