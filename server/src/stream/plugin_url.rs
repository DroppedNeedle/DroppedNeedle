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
use crate::plugins::capabilities::stream::{PROXY_MAX_BYTES, open_url, proxy_chunks};

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

/// A plugin URL copied to a local temp file for ffmpeg. Dropping it
/// removes the file.
pub struct Spool {
    path: PathBuf,
}

impl Spool {
    /// Copy the whole response body (capped like any plugin proxy) into a
    /// new temp file named with `format` as its extension.
    pub async fn fill(response: reqwest::Response, format: &str) -> std::io::Result<Self> {
        let extension = if !format.is_empty()
            && format.len() <= 8
            && format.chars().all(|c| c.is_ascii_alphanumeric())
        {
            format
        } else {
            "bin"
        };
        let spool = Self {
            path: std::env::temp_dir().join(format!(
                "droppedneedle-plugin-stream-{}.{extension}",
                uuid::Uuid::new_v4().simple()
            )),
        };
        let target = spool.path.clone();
        let mut file = tokio::task::spawn_blocking(move || std::fs::File::create(target))
            .await
            .map_err(std::io::Error::other)??;
        let mut chunks = proxy_chunks(response).boxed();
        while let Some(chunk) = chunks.next().await {
            let chunk = chunk?;
            file = tokio::task::spawn_blocking(move || {
                use std::io::Write as _;
                file.write_all(&chunk).map(|()| file)
            })
            .await
            .map_err(std::io::Error::other)??;
        }
        Ok(spool)
    }

    /// The spooled file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
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
