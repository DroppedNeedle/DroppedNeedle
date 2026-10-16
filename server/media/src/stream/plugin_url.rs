//! Byte plumbing for `streaming_source` plugins that answer with a URL.
//!
//! A direct play is proxied by range through [`super::ranged`]. A
//! transcode spools the URL into a temp file first, because ffmpeg only
//! ever reads local files; the file lives as long as the transcode body
//! and is removed when it ends.

use std::path::{Path, PathBuf};

use futures_util::StreamExt as _;

use super::ranged;
use super::routes::SpanOpener;
use droppedneedle_plugins::capabilities::stream::{PROXY_MAX_BYTES, open_url, proxy_chunks};

/// The audio format a plugin URL carries: the URL path's extension, else
/// the content type hint (v2's rule), else empty.
pub fn url_format(url: &reqwest::Url, content_type: &str) -> String {
    let suffix = Path::new(url.path())
        .extension()
        .and_then(|ext| ext.to_str())
        .map(str::to_lowercase)
        .filter(|ext| !ext.is_empty());
    if let Some(suffix) = suffix {
        return suffix;
    }
    let media = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_lowercase();
    let known = match media.as_str() {
        "audio/mpeg" | "audio/mp3" => "mp3",
        "audio/ogg" | "audio/opus" => "opus",
        "audio/flac" | "audio/x-flac" => "flac",
        "audio/wav" | "audio/x-wav" => "wav",
        "audio/mp4" => "mp4",
        "audio/aac" => "aac",
        _ => "",
    };
    if !known.is_empty() {
        return known.to_owned();
    }
    media
        .strip_prefix("audio/")
        .map(|rest| rest.split('+').next().unwrap_or_default().trim().to_owned())
        .unwrap_or_default()
}

/// Span opener over a sized plugin URL; every range request re-checks
/// each redirect hop, like the first open.
pub fn spans(client: reqwest::Client, url: reqwest::Url, first: reqwest::Response) -> SpanOpener {
    ranged::spans(
        first,
        |response| proxy_chunks(response).boxed(),
        Box::new(move |range| Box::pin(async move { open_url(&client, url, Some(&range)).await })),
    )
}

/// Name prefix of spool files, so a startup sweep finds ones a crash left.
const SPOOL_PREFIX: &str = "droppedneedle-plugin-stream-";

/// Temp disk all plugin transcode spools may hold at once.
pub const SPOOL_BUDGET_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Bytes currently held by live spools.
static SPOOLED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Remove spool files a previous run left behind (a crash or kill skips
/// the drop that would have removed them). Called once at startup.
pub fn sweep_stale_spools() {
    let dir = std::env::temp_dir();
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(dir = %dir.display(), %error, "could not list the temp folder for old plugin stream files");
            return;
        }
    };
    for entry in entries.flatten() {
        let stale = entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(SPOOL_PREFIX))
            && entry.file_type().is_ok_and(|kind| kind.is_file());
        if stale && let Err(error) = std::fs::remove_file(entry.path()) {
            tracing::warn!(path = %entry.path().display(), %error, "could not remove an old plugin stream file");
        }
    }
}

/// A plugin URL copied to a local temp file for ffmpeg, readable by the
/// server's user only. Dropping it (the transcode ended, or the client
/// went away mid-copy or mid-play) removes the file and frees its share
/// of [`SPOOL_BUDGET_BYTES`].
pub struct Spool {
    path: PathBuf,
    held: u64,
}

impl Spool {
    /// Copy the whole response body (capped like any plugin proxy) into a
    /// new temp file named with `format` as its extension. Fails once all
    /// spools together would pass [`SPOOL_BUDGET_BYTES`].
    pub async fn fill(response: reqwest::Response, format: &str) -> std::io::Result<Self> {
        let extension = if !format.is_empty()
            && format.len() <= 8
            && format.chars().all(|c| c.is_ascii_alphanumeric())
        {
            format
        } else {
            "bin"
        };
        let mut spool = Self {
            path: std::env::temp_dir().join(format!(
                "{SPOOL_PREFIX}{}.{extension}",
                uuid::Uuid::new_v4().simple()
            )),
            held: 0,
        };
        let target = spool.path.clone();
        let mut file = tokio::task::spawn_blocking(move || create_private(&target))
            .await
            .map_err(std::io::Error::other)??;
        let mut chunks = proxy_chunks(response).boxed();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            spool.reserve(chunk.len() as u64)?;
            file = tokio::task::spawn_blocking(move || {
                use std::io::Write as _;
                file.write_all(&chunk).map(|()| file)
            })
            .await
            .map_err(std::io::Error::other)??;
        }
        Ok(spool)
    }

    /// Count `bytes` more against the shared budget.
    fn reserve(&mut self, bytes: u64) -> std::io::Result<()> {
        use std::sync::atomic::Ordering;
        let total = SPOOLED.fetch_add(bytes, Ordering::SeqCst) + bytes;
        self.held += bytes;
        if total > SPOOL_BUDGET_BYTES {
            return Err(std::io::Error::other(
                "plugin transcodes are using all of their temp disk space",
            ));
        }
        Ok(())
    }

    /// The spooled file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Create a new file only the server's user can read and write.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    options.open(path)
}

impl Drop for Spool {
    fn drop(&mut self) {
        SPOOLED.fetch_sub(self.held, std::sync::atomic::Ordering::SeqCst);
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(path = %self.path.display(), %error, "could not remove a plugin stream spool file");
        }
    }
}

/// True when an upstream's declared size is one the proxy will carry.
pub fn within_cap(len: u64) -> bool {
    len > 0 && len <= PROXY_MAX_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_prefers_the_url_extension_then_the_hint() {
        let url = reqwest::Url::parse("https://cdn.example/a/track.FLAC?sig=1").unwrap();
        assert_eq!(url_format(&url, "audio/mpeg"), "flac");
        let bare = reqwest::Url::parse("https://cdn.example/play/123").unwrap();
        assert_eq!(url_format(&bare, "audio/mpeg; charset=x"), "mp3");
        assert_eq!(url_format(&bare, "audio/x-matroska"), "x-matroska");
        assert_eq!(url_format(&bare, ""), "");
    }
}
