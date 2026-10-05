//! Breach-corpus screening (Have-I-Been-Pwned), kept from v2.
//!
//! Priority, unchanged: a local hash-ordered file when configured, else the
//! free k-anonymity range API (only the first 5 SHA-1 hex chars leave the
//! server). Screening is fail-open: missing files and transport faults log
//! and allow; only a corpus hit rejects. The HTTP call sits behind
//! [`HibpHttp`] so tests never touch the network.
//!
//! SHA-1 is implemented here (~60 lines, NIST-vector tested) because no
//! SHA-1 crate is depended on for this alone. It serves only HIBP lookups, never security decisions.

use std::collections::HashSet;
use std::sync::Arc;

use super::stores::{BoxFuture, HibpPolicy, PasswordScreen};

/// Number of SHA-1 hex chars sent to the range API (k-anonymity prefix).
pub const RANGE_PREFIX_LEN: usize = 5;
/// Padding header the range API honors.
pub const RANGE_URL_PREFIX: &str = "https://api.pwnedpasswords.com/range/";

/// SHA-1 hex digest (uppercase, the HIBP file ordering).
pub fn sha1_hex_upper(data: &[u8]) -> String {
    let mut state = [
        0x6745_2301u32,
        0xEFCD_AB89,
        0x98BA_DCFE,
        0x1032_5476,
        0xC3D2_E1F0,
    ];
    let mut message = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in message.chunks_exact(64) {
        let mut schedule = [0u32; 80];
        for (i, word) in schedule.iter_mut().enumerate().take(16) {
            let o = i * 4;
            *word = u32::from_be_bytes([chunk[o], chunk[o + 1], chunk[o + 2], chunk[o + 3]]);
        }
        for i in 16..80 {
            schedule[i] = (schedule[i - 3] ^ schedule[i - 8] ^ schedule[i - 14] ^ schedule[i - 16])
                .rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) =
            (state[0], state[1], state[2], state[3], state[4]);
        for (i, word) in schedule.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let tmp = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*word);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = tmp;
        }
        state[0] = state[0].wrapping_add(a);
        state[1] = state[1].wrapping_add(b);
        state[2] = state[2].wrapping_add(c);
        state[3] = state[3].wrapping_add(d);
        state[4] = state[4].wrapping_add(e);
    }

    let mut out = String::with_capacity(40);
    for word in state {
        out.push_str(&format!("{word:08X}"));
    }
    out
}

/// One HTTP range lookup. Unit error: transport details never escape.
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

/// Production range client over the shared outbound factory client.
#[derive(Debug, Clone)]
pub struct PwnedPasswordsHttp {
    /// Factory-built client (owns timeouts and TLS behavior).
    pub client: reqwest::Client,
}

impl HibpHttp for PwnedPasswordsHttp {
    fn range<'a>(
        &'a self,
        prefix: &'a str,
    ) -> BoxFuture<'a, Result<HashSet<String>, HibpHttpError>> {
        Box::pin(async move {
            let response = self
                .client
                .get(format!("{RANGE_URL_PREFIX}{prefix}"))
                .header("Add-Padding", "true")
                .send()
                .await
                .map_err(|_| HibpHttpError)?;
            if response.status() != reqwest::StatusCode::OK {
                return Err(HibpHttpError);
            }
            let text = response.text().await.map_err(|_| HibpHttpError)?;
            let mut suffixes = HashSet::new();
            for line in text.lines() {
                let Some((suffix, _)) = line.split_once(':') else {
                    continue;
                };
                let suffix = suffix.trim().to_uppercase();
                if !suffix.is_empty() {
                    suffixes.insert(suffix);
                }
            }
            Ok(suffixes)
        })
    }
}

/// The v2 breach screen: local file first, range API second, fail open.
pub struct HibpScreen {
    http: Arc<dyn HibpHttp>,
}

impl HibpScreen {
    /// Build over one HTTP seam.
    pub fn new(http: Arc<dyn HibpHttp>) -> Self {
        Self { http }
    }
}

impl PasswordScreen for HibpScreen {
    fn screen<'a>(&'a self, password: &'a str, policy: &'a HibpPolicy) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            if !policy.check {
                return false;
            }
            let digest = sha1_hex_upper(password.as_bytes());
            if !policy.local_path.is_empty() {
                return screen_local_file(&policy.local_path, &digest).await;
            }
            let prefix = digest[..RANGE_PREFIX_LEN].to_owned();
            let suffix = digest[RANGE_PREFIX_LEN..].to_owned();
            match self.http.range(&prefix).await {
                Ok(suffixes) => suffixes.contains(&suffix),
                Err(_) => {
                    tracing::warn!(
                        "pwnedpasswords range check failed; proceeding without breach check"
                    );
                    false
                }
            }
        })
    }
}

/// Screen against a local hash-ordered HIBP file. Missing file fails open
/// (the operator opted out of API calls); IO faults mid-search also fail
/// open with a warning, exactly like v2.
async fn screen_local_file(path: &str, digest: &str) -> bool {
    if !std::path::Path::new(path).is_file() {
        tracing::warn!("HIBP local path configured but file not found; skipping breach check");
        return false;
    }
    let owned_path = path.to_owned();
    let owned_digest = digest.to_owned();
    let found =
        tokio::task::spawn_blocking(move || search_sorted_file(&owned_path, &owned_digest)).await;
    match found {
        Ok(Ok(hit)) => hit,
        Ok(Err(cause)) => {
            tracing::warn!(%cause, "HIBP local file search failed; proceeding without breach check");
            false
        }
        Err(_) => false,
    }
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

    /// Minimal temp-file guard without new dependencies.
    struct TempGuard {
        path: std::path::PathBuf,
    }
    impl Drop for TempGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
    fn tempfile_path_guard() -> TempGuard {
        let path = std::env::temp_dir().join(format!(
            "dn-hibp-test-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        TempGuard { path }
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
        let guard = tempfile_path_guard();
        std::fs::write(&guard.path, hashes.join("\n")).unwrap();
        let path = guard.path.to_string_lossy().into_owned();
        for line in &hashes {
            let hash = line.split(':').next().unwrap();
            assert!(search_sorted_file(&path, hash).unwrap(), "missed {hash}");
        }
        assert!(!search_sorted_file(&path, "5BAA61E4C9B93F3F0682250B6CF8331B7EE68FD9").unwrap());
        assert!(!search_sorted_file(&path, "0000000000000000000000000000000000000000").unwrap());
        assert!(!search_sorted_file(&path, "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF").unwrap());
    }

    #[test]
    fn file_search_on_empty_file_misses() {
        let guard = tempfile_path_guard();
        std::fs::write(&guard.path, "").unwrap();
        let path = guard.path.to_string_lossy().into_owned();
        assert!(!search_sorted_file(&path, "5BAA61E4C9B93F3F0682250B6CF8331B7EE68FD8").unwrap());
    }
}
