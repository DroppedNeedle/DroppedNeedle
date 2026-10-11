//! Range proxying for upstream HTTP audio (remote servers and plugin
//! URLs). The open that learned an object's size serves a span from byte
//! 0; any other span asks the upstream for just that range, so a seek
//! never pulls the bytes before it. Bodies move one upstream chunk at a
//! time, so memory stays bounded whatever the file size.

use std::time::Duration;

use futures_util::future::BoxFuture;
use futures_util::{StreamExt as _, TryStreamExt as _};

use super::routes::{ChunkStream, SpanOpener};

/// How long an upstream may go quiet mid-body before the stream fails.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Turns an upstream response into body chunks.
pub type ChunkFn = fn(reqwest::Response) -> ChunkStream;

/// Opens the upstream again with a `Range` header value.
pub type RangeOpen =
    Box<dyn FnOnce(String) -> BoxFuture<'static, Result<reqwest::Response, String>> + Send>;

/// `take` bytes of `chunks` after skipping the first `skip`. Ending short
/// fails the stream, so a response never sends less than it advertised.
pub fn byte_window(chunks: ChunkStream, skip: u64, take: u64) -> ChunkStream {
    futures_util::stream::unfold(Some((chunks, skip, take)), |state| async move {
        let (mut chunks, mut skip, remaining) = state?;
        if remaining == 0 {
            return None;
        }
        loop {
            match chunks.next().await {
                Some(Ok(mut chunk)) => {
                    let len = chunk.len() as u64;
                    if skip >= len {
                        skip -= len;
                        continue;
                    }
                    chunk.drain(..usize::try_from(skip).unwrap_or(chunk.len()));
                    let keep = remaining.min(chunk.len() as u64);
                    chunk.truncate(usize::try_from(keep).unwrap_or(chunk.len()));
                    return Some((Ok(chunk), Some((chunks, 0, remaining - keep))));
                }
                Some(Err(error)) => return Some((Err(error), None)),
                None => {
                    return Some((
                        Err(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "upstream audio ended before the advertised length",
                        )),
                        None,
                    ));
                }
            }
        }
    })
    .boxed()
}

/// Span opener over a sized upstream object. `first` is the open that
/// learned the size; `reopen` asks for a range. An upstream that ignores
/// `Range` (200) is skipped forward to the span instead.
pub fn spans(first: reqwest::Response, chunks: ChunkFn, reopen: RangeOpen) -> SpanOpener {
    Box::new(move |start, len| {
        if start == 0 {
            return byte_window(chunks(first), 0, len);
        }
        drop(first);
        if len == 0 {
            return futures_util::stream::empty().boxed();
        }
        let range = format!("bytes={}-{}", start, start.saturating_add(len - 1));
        futures_util::stream::once(async move {
            let response = reopen(range).await.map_err(std::io::Error::other)?;
            match response.status().as_u16() {
                206 => {
                    let served = response
                        .headers()
                        .get(reqwest::header::CONTENT_RANGE)
                        .and_then(|value| value.to_str().ok())
                        .and_then(range_start);
                    if served != Some(start) {
                        return Err(std::io::Error::other(
                            "upstream answered a different range than asked",
                        ));
                    }
                    Ok(byte_window(chunks(response), 0, len))
                }
                200 => Ok(byte_window(chunks(response), start, len)),
                status => Err(std::io::Error::other(format!(
                    "upstream range request answered {status}"
                ))),
            }
        })
        .try_flatten()
        .boxed()
    })
}

/// First byte of a `Content-Range: bytes <start>-<end>/<total>` value.
fn range_start(value: &str) -> Option<u64> {
    let span = value.trim().strip_prefix("bytes")?.trim_start();
    span.split('-').next()?.trim().parse().ok()
}

/// A response body as chunks, each due within [`IDLE_TIMEOUT`].
pub fn response_chunks(response: reqwest::Response) -> ChunkStream {
    futures_util::stream::unfold(Some(response), |state| async move {
        let mut response = state?;
        match tokio::time::timeout(IDLE_TIMEOUT, response.chunk()).await {
            Ok(Ok(Some(bytes))) => Some((Ok(bytes.to_vec()), Some(response))),
            Ok(Ok(None)) => None,
            // The URL can carry credentials (Subsonic auth is in the query).
            Ok(Err(error)) => Some((Err(std::io::Error::other(error.without_url())), None)),
            Err(_) => Some((Err(std::io::Error::other("upstream audio stalled")), None)),
        }
    })
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chunks(parts: &[&[u8]]) -> ChunkStream {
        let parts: Vec<_> = parts.iter().map(|part| Ok(part.to_vec())).collect();
        futures_util::stream::iter(parts).boxed()
    }

    #[tokio::test]
    async fn window_skips_and_cuts_across_chunks() {
        let out: Vec<u8> = byte_window(chunks(&[b"abc", b"defg", b"hij"]), 2, 6)
            .try_concat()
            .await
            .unwrap();
        assert_eq!(out, b"cdefgh");
        let short = byte_window(chunks(&[b"abc"]), 1, 5).try_concat().await;
        assert!(short.is_err());
    }

    #[test]
    fn content_range_start_parses() {
        assert_eq!(range_start("bytes 100-199/1000"), Some(100));
        assert_eq!(range_start("bytes */1000"), None);
    }
}
