//! The Internet Archive as Free Music sees it: search, an item's files,
//! and a file's bytes.
//!
//! [`ArchiveSource`] is the seam; [`LiveArchive`] serves it from the
//! provider client in `providers::archive` (searches and metadata, paced
//! at 2 requests a second with a burst of 4 as v2 did) plus a streamed GET
//! for the file bytes on the shared HTTP client.

use futures_util::future::BoxFuture;

use crate::providers::adapters::ReqwestGet;
use crate::providers::archive::{ArchiveClient, DOWNLOAD_PATTERN, FetchError};
pub use crate::providers::archive::{ArchiveFile, ArchiveItem};
use crate::providers::limiter::{RateLimiter, RatePolicy};

/// v2's Archive pace: no published limit, well under its own tooling.
const ARCHIVE_POLICY: RatePolicy = RatePolicy::new(2.0, 4);
/// Search hits considered per request (v2 default).
const SEARCH_LIMIT: u32 = 12;
/// A file transfer that sends nothing for this long is dead.
const STALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// Why the Archive could not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArchiveError {
    /// Network trouble or a 429; worth one more try.
    Unreachable(String),
    /// Any other answer we cannot use.
    Unusable(String),
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(detail) | Self::Unusable(detail) => f.write_str(detail),
        }
    }
}

impl From<FetchError> for ArchiveError {
    fn from(error: FetchError) -> Self {
        match error {
            FetchError::Transport => Self::Unreachable("Internet Archive unreachable".into()),
            FetchError::RateLimited { .. } => {
                Self::Unreachable("Internet Archive rate limited the request".into())
            }
            FetchError::Unusable => Self::Unusable("Internet Archive answer unusable".into()),
        }
    }
}

/// A file's bytes, chunk by chunk.
pub trait Chunks: Send {
    /// The next chunk, or `None` at the end.
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, ArchiveError>>;
}

/// What Free Music needs from the Internet Archive.
pub trait ArchiveSource: Send + Sync {
    /// Openly licensed audio items for an artist and title.
    fn search<'a>(
        &'a self,
        artist: &'a str,
        title: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ArchiveItem>, ArchiveError>>;

    /// An item's licence and audio files (`("", [])` for a dark item or
    /// one without an open licence).
    fn files<'a>(
        &'a self,
        identifier: &'a str,
    ) -> BoxFuture<'a, Result<(String, Vec<ArchiveFile>), ArchiveError>>;

    /// Open one file for reading.
    fn open<'a>(
        &'a self,
        identifier: &'a str,
        filename: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Chunks>, ArchiveError>>;
}

/// The live Internet Archive.
pub struct LiveArchive {
    transfers: reqwest::Client,
    port: ReqwestGet,
    limiter: RateLimiter,
}

impl LiveArchive {
    /// Searches and metadata over the factory's shared client, file
    /// transfers over its archive client (no total timeout, redirects
    /// kept on archive.org).
    pub fn new(shared: reqwest::Client, transfers: reqwest::Client) -> Self {
        Self {
            port: ReqwestGet::new(shared),
            transfers,
            limiter: RateLimiter::new(ARCHIVE_POLICY),
        }
    }

    async fn paced(&self) {
        if self.limiter.acquire().await.is_err() {
            tracing::warn!("archive limiter refused a single token");
        }
    }
}

struct ResponseChunks(reqwest::Response);

impl Chunks for ResponseChunks {
    fn next(&mut self) -> BoxFuture<'_, Result<Option<Vec<u8>>, ArchiveError>> {
        Box::pin(async move {
            match tokio::time::timeout(STALL_TIMEOUT, self.0.chunk()).await {
                Ok(chunk) => chunk
                    .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
                    .map_err(|error| {
                        ArchiveError::Unreachable(format!("file transfer broke: {error}"))
                    }),
                Err(_) => Err(ArchiveError::Unreachable(
                    "file transfer stalled".to_owned(),
                )),
            }
        })
    }
}

/// Percent-encode a file name for a URL path, keeping `/` (Archive file
/// names may sit in subfolders).
fn encode_path(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(char::from(byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

impl ArchiveSource for LiveArchive {
    fn search<'a>(
        &'a self,
        artist: &'a str,
        title: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ArchiveItem>, ArchiveError>> {
        Box::pin(async move {
            self.paced().await;
            Ok(ArchiveClient::new(&self.port)
                .search_audio(artist, title, SEARCH_LIMIT)
                .await?)
        })
    }

    fn files<'a>(
        &'a self,
        identifier: &'a str,
    ) -> BoxFuture<'a, Result<(String, Vec<ArchiveFile>), ArchiveError>> {
        Box::pin(async move {
            self.paced().await;
            Ok(ArchiveClient::new(&self.port)
                .get_item_files(&encode_path(identifier))
                .await?)
        })
    }

    fn open<'a>(
        &'a self,
        identifier: &'a str,
        filename: &'a str,
    ) -> BoxFuture<'a, Result<Box<dyn Chunks>, ArchiveError>> {
        Box::pin(async move {
            let url = DOWNLOAD_PATTERN
                .replace("{identifier}", &encode_path(identifier))
                .replace("{filename}", &encode_path(filename));
            self.paced().await;
            let response = self.transfers.get(url).send().await.map_err(|error| {
                ArchiveError::Unreachable(format!("file request failed: {error}"))
            })?;
            match response.status().as_u16() {
                200 => Ok(Box::new(ResponseChunks(response)) as Box<dyn Chunks>),
                429 => Err(ArchiveError::Unreachable(
                    "Internet Archive rate limited the file".into(),
                )),
                status => Err(ArchiveError::Unusable(format!(
                    "Internet Archive answered HTTP {status} for a file"
                ))),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_path_encoded() {
        assert_eq!(
            encode_path("CD 1/01 - Song.flac"),
            "CD%201/01%20-%20Song.flac"
        );
        assert_eq!(encode_path("a&b?.mp3"), "a%26b%3F.mp3");
    }
}
