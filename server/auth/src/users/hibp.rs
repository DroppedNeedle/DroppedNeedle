//! Breach-corpus screening (Have-I-Been-Pwned), kept from v2.
//!
//! Priority, unchanged: a local hash-ordered file when configured, else the
//! free k-anonymity range API (only the first 5 SHA-1 hex chars leave the
//! server). Screening is fail-open, so a set-up never fails because an outside
//! site is down: missing files and transport faults allow, logged once per
//! change between reachable and unreachable; only a corpus hit rejects. The
//! HTTP call sits behind [`HibpHttp`] so tests never touch the network.
//!
//! SHA-1 serves only HIBP lookups, never security decisions.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sha1::{Digest, Sha1};

use super::stores::BoxFuture;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::SecuritySettings;
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::hibp::HibpClient;

/// Number of SHA-1 hex chars sent to the range API (k-anonymity prefix).
pub const RANGE_PREFIX_LEN: usize = 5;

/// SHA-1 hex digest (uppercase, the HIBP file ordering).
pub fn sha1_hex_upper(data: &[u8]) -> String {
    hex::encode_upper(Sha1::digest(data))
}

/// One HTTP range lookup. Unit error: transport details never escape.
/// A trait because it reaches the breach service: tests answer with a
/// scripted range instead of the network.
pub trait HibpHttp: Send + Sync {
    /// Suffix set for a 5-char prefix (each `SUFFIX:COUNT` line's suffix,
    /// uppercased). Transport failure is `Err` and fails open upstream.
    fn range<'a>(
        &'a self,
        prefix: &'a str,
    ) -> BoxFuture<'a, Result<HashSet<String>, HibpHttpError>>;
}

/// A failed range call. Carries no detail on purpose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HibpHttpError;

/// Production range client: the providers crate's Pwned Passwords
/// client. Every screen runs while a person sets a password, so lookups
/// go at user priority; a lookup the row cannot fit fails open like any
/// other failure.
#[derive(Debug, Clone)]
pub struct PwnedPasswordsHttp(pub HibpClient);

impl HibpHttp for PwnedPasswordsHttp {
    fn range<'a>(
        &'a self,
        prefix: &'a str,
    ) -> BoxFuture<'a, Result<HashSet<String>, HibpHttpError>> {
        Box::pin(async move {
            self.0
                .range(prefix, RequestPriority::UserInitiated)
                .await
                .map_err(|error| {
                    tracing::debug!(%error, "pwnedpasswords range lookup failed");
                    HibpHttpError
                })
        })
    }
}

/// The v2 breach screen: local file first, range API second, fail open.
/// Its switch and local file are read live from the `security` section.
#[derive(Clone)]
pub struct HibpScreen {
    http: Arc<dyn HibpHttp>,
    settings: Live<SecuritySettings>,
    /// Whether the last check could read its source, so the warning is
    /// logged when that changes and not on every set-up.
    reachable: Arc<AtomicBool>,
}

impl HibpScreen {
    /// Build over one HTTP seam and the live `security` section.
    pub fn new(http: Arc<dyn HibpHttp>, settings: Live<SecuritySettings>) -> Self {
        Self {
            http,
            settings,
            reachable: Arc::new(AtomicBool::new(true)),
        }
    }

    /// True when the password appears in the corpus. Screening is off when
    /// the section is off or cannot be read (advisory only; auth itself
    /// never depends on it). Transport and file errors fail open (false),
    /// exactly like v2; only a hit returns true.
    pub async fn screen(&self, password: &str) -> bool {
        let Some(local_path) = self
            .settings
            .get()
            .ok()
            .filter(|section| section.hibp_check)
            .map(|section| section.hibp_local_path.clone())
        else {
            return false;
        };
        let digest = sha1_hex_upper(password.as_bytes());
        let checked = if local_path.is_empty() {
            let prefix = &digest[..RANGE_PREFIX_LEN];
            let suffix = &digest[RANGE_PREFIX_LEN..];
            self.http
                .range(prefix)
                .await
                .map(|suffixes| suffixes.contains(suffix))
                .map_err(|_| "pwnedpasswords range check failed".to_owned())
        } else {
            screen_local_file(&local_path, &digest).await
        };
        let was_reachable = self.reachable.swap(checked.is_ok(), Ordering::Relaxed);
        match checked {
            Ok(hit) => {
                if !was_reachable {
                    tracing::info!("breach check is reachable again");
                }
                hit
            }
            Err(cause) => {
                if was_reachable {
                    tracing::warn!(%cause, "proceeding without breach check until it is reachable");
                }
                false
            }
        }
    }
}

/// Screen against a local hash-ordered HIBP file. The error is why it could
/// not be read: a missing file, or an IO fault mid-search.
async fn screen_local_file(path: &str, digest: &str) -> Result<bool, String> {
    if !std::path::Path::new(path).is_file() {
        return Err("HIBP local path configured but file not found".to_owned());
    }
    let owned_path = path.to_owned();
    let owned_digest = digest.to_owned();
    tokio::task::spawn_blocking(move || search_sorted_file(&owned_path, &owned_digest))
        .await
        .map_err(|cause| format!("HIBP local file search stopped: {cause}"))?
        .map_err(|cause| format!("HIBP local file search failed: {cause}"))
}

/// Binary-search a `HASH:COUNT` file ordered ascending by hash. Seeks to
/// the line containing each midpoint (bounded backward scan for its start),
/// so every probe compares a full line and both bounds always progress.
fn search_sorted_file(path: &str, target: &str) -> Result<bool, std::io::Error> {
    use std::io::{Read, Seek, SeekFrom};
    /// Lines are `40 hex + : + count`; 256 bytes of lookback always finds
    /// the start of the line containing the midpoint.
    const LOOKBACK: u64 = 256;
    /// A line longer than this means a corrupt file, not a hash list.
    const MAX_LINE: usize = 4096;

    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let mut low = 0u64;
    let mut high = len;
    let mut byte = [0u8; 1];
    let mut line = Vec::with_capacity(64);

    while low < high {
        let mid = low + (high - low) / 2;
        // Find the start of the line containing `mid`.
        let scan_from = mid.saturating_sub(LOOKBACK);
        file.seek(SeekFrom::Start(scan_from))?;
        let mut line_start = scan_from;
        let mut pos = scan_from;
        while pos < mid {
            if file.read_exact(&mut byte).is_err() {
                break;
            }
            pos += 1;
            if byte[0] == b'\n' {
                line_start = pos;
            }
        }
        // Read the full line at `line_start`.
        file.seek(SeekFrom::Start(line_start))?;
        line.clear();
        loop {
            if line.len() > MAX_LINE {
                return Err(std::io::Error::other("HIBP file has an overlong line"));
            }
            if file.read_exact(&mut byte).is_err() {
                break;
            }
            if byte[0] == b'\n' {
                break;
            }
            if byte[0] != b'\r' {
                line.push(byte[0]);
            }
        }
        let end = file.stream_position()?;
        let text = String::from_utf8_lossy(&line);
        let hash = text.split(':').next().unwrap_or("");
        if hash == target {
            return Ok(true);
        }
        if hash < target {
            low = end.max(low + 1);
        } else {
            high = line_start.max(low);
            if high == low && line_start <= low {
                // The line starts at or below `low` yet sorts after the
                // target: the target sits before every remaining line.
                return Ok(false);
            }
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_matches_nist_vectors() {
        assert_eq!(
            sha1_hex_upper(b""),
            "DA39A3EE5E6B4B0D3255BFEF95601890AFD80709"
        );
        assert_eq!(
            sha1_hex_upper(b"abc"),
            "A9993E364706816ABA3E25717850C26C9CD0D89D"
        );
        assert_eq!(
            sha1_hex_upper(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983E441C3BD26EBAAE4AA1F95129E5E54670F1"
        );
    }

    /// A test's own temp folder, removed when dropped.
    struct TempFolder(std::path::PathBuf);
    impl Drop for TempFolder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A hash file of `lines` in a folder no other test shares.
    fn hash_file(test: &str, lines: &str) -> (TempFolder, String) {
        let dir = std::env::temp_dir().join(format!("dn-hibp-{test}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hashes.txt");
        std::fs::write(&path, lines).unwrap();
        (TempFolder(dir), path.to_string_lossy().into_owned())
    }

    #[test]
    fn file_search_finds_edges_and_misses() {
        let hashes = [
            "00000AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA:3",
            "11111BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB:1",
            "5BAA61E4C9B93F3F0682250B6CF8331B7EE68FD8:3730471",
            "7C4A8D09CA3762AF61E59520943DC26494F8941B:4218065",
            "FFFFFZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ:2",
        ];
        let (_dir, path) = hash_file("edges", &hashes.join("\n"));
        for line in &hashes {
            let hash = line.split(':').next().unwrap();
            assert!(search_sorted_file(&path, hash).unwrap(), "missed {hash}");
        }
        assert!(!search_sorted_file(&path, "5BAA61E4C9B93F3F0682250B6CF8331B7EE68FD9").unwrap());
        assert!(!search_sorted_file(&path, "0000000000000000000000000000000000000000").unwrap());
        assert!(!search_sorted_file(&path, "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF").unwrap());

        let (_empty_dir, empty) = hash_file("empty", "");
        assert!(
            !search_sorted_file(&empty, "5BAA61E4C9B93F3F0682250B6CF8331B7EE68FD8").unwrap(),
            "an empty file misses"
        );
    }
}
