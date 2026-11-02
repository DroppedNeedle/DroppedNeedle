//! Byte-exact framing for every stream route: range parsing, 200/206/HEAD
//! status and headers, and the body they send.
//!
//! [`frame`] turns one opened object, the `Range` header and HEAD into
//! status, headers and body. The native, Subsonic and Jellyfin stream routes
//! and the track download all frame through it or through [`DirectSpan`], so
//! ranges, lengths and HEAD answer the same everywhere; each protocol keeps
//! only its own error body.
//!
//! # Ranges
//!
//! One `bytes=start-end` range only (suffix and open forms included), end
//! clamped to size-1, suffix longer than the file clamped to the whole
//! file. Anything else, a multi-range request included, is 416 with
//! `Content-Range: bytes */N`.

use std::path::PathBuf;

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use futures_util::StreamExt as _;

use super::engine::{ChunkStream, MediaBody, StreamMedia};
use super::error::StreamError;

/// Inclusive resolved byte range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ByteRange {
    /// First byte offset.
    start: u64,
    /// Last byte offset, inclusive.
    end: u64,
}

/// Parse a single `Range` header against `total_len`:
/// `bytes=start-end` (end clamped), `bytes=start-` (open), `bytes=-suffix`
/// (clamped to the whole file when longer). `None` means 416: no match,
/// both bounds empty, suffix of 0, start past the end, or an empty object.
/// Multi-range and unit forms other than `bytes=` never match.
fn parse_range(header: &str, total_len: u64) -> Option<ByteRange> {
    if total_len == 0 {
        return None;
    }
    // Surrounding whitespace is insignificant: `"bytes=0-1 "` seeks.
    let spec = header.trim().strip_prefix("bytes=")?;
    let (start_str, end_str) = spec.split_once('-')?;
    if start_str.is_empty() && end_str.is_empty() {
        return None;
    }
    let digits = |text: &str| text.bytes().all(|byte| byte.is_ascii_digit());
    if !digits(start_str) || !digits(end_str) {
        return None;
    }
    if start_str.is_empty() {
        let suffix_len: u64 = end_str.parse().ok()?;
        if suffix_len == 0 {
            return None;
        }
        let start = total_len.saturating_sub(suffix_len);
        return Some(ByteRange {
            start,
            end: total_len - 1,
        });
    }
    let start: u64 = start_str.parse().ok()?;
    let end = if end_str.is_empty() {
        total_len - 1
    } else {
        let asked: u64 = end_str.parse().ok()?;
        asked.min(total_len - 1)
    };
    if start > end || start >= total_len {
        return None;
    }
    Some(ByteRange { start, end })
}

/// The `Range` header as text. One that is not visible ASCII reads as an
/// empty value, which no range matches (416).
pub fn range_header(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::RANGE)
        .map(|value| value.to_str().unwrap_or(""))
}

/// A framed stream answer: status, framing headers and body.
#[derive(Debug)]
pub struct Framed {
    /// 200 or 206.
    pub status: StatusCode,
    /// `Content-Type`, `Content-Encoding`, `Accept-Ranges`, and
    /// `Content-Length` and `Content-Range` where they apply; a transcode
    /// adds `Cache-Control`.
    pub headers: HeaderMap,
    /// What to send.
    pub body: FramedBody,
}

/// The body of a [`Framed`] answer. A stream carries the open's direct
/// lease inside, so the slot stays held until the last byte is sent or
/// the client goes away.
pub enum FramedBody {
    /// Nothing (HEAD, or a transcode HEAD answered without ffmpeg).
    Empty,
    /// Bytes already in memory, sent whole.
    Bytes(Vec<u8>),
    /// Bytes read as the response goes out.
    Stream(ChunkStream),
}

impl std::fmt::Debug for FramedBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::Bytes(bytes) => write!(f, "Bytes({} bytes)", bytes.len()),
            Self::Stream(_) => f.write_str("Stream"),
        }
    }
}

/// Frame one opened object for the wire, the same way on every stream
/// route. A direct object honours one `Range` (206) or answers whole (200);
/// any other range, multi-range included, is
/// [`StreamError::Unsatisfiable`], which each protocol renders as its own
/// 416. A transcode is 200 with no ranges and no caching, sized only when
/// `want_estimate` and the engine estimated it. HEAD gets GET's status and
/// headers with no body.
pub fn frame(
    media: StreamMedia,
    range: Option<&str>,
    head_only: bool,
    want_estimate: bool,
) -> Result<Framed, StreamError> {
    let StreamMedia {
        content_type,
        total_len,
        transcoded,
        estimated_len,
        body,
        lease,
    } = media;
    let hold = move |chunks: ChunkStream| match lease {
        Some(lease) => lease.hold(chunks),
        None => chunks,
    };
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, upstream_content_type(&content_type));
    headers.insert(
        header::CONTENT_ENCODING,
        HeaderValue::from_static("identity"),
    );
    if transcoded {
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        let estimated = estimated_len.filter(|_| want_estimate);
        if let Some(len) = estimated {
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
        } else if let (true, MediaBody::Bytes(bytes)) = (head_only, &body) {
            // An in-memory body knows its length: pin it on HEAD so HEAD matches
            // GET, whose buffered body gets it stamped.
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(bytes.len()));
        }
        // A live transcode has no known size, so GET without an estimate
        // carries no Content-Length and streams chunked.
        let body = if head_only {
            FramedBody::Empty
        } else {
            match body {
                MediaBody::Bytes(bytes) => FramedBody::Bytes(bytes),
                MediaBody::Chunks(chunks) => FramedBody::Stream(hold(chunks)),
                MediaBody::File(path) => FramedBody::Stream(hold(file_range(path, 0, total_len))),
                MediaBody::Spans(open) => FramedBody::Stream(hold(open(0, total_len))),
                MediaBody::Empty => FramedBody::Empty,
            }
        };
        return Ok(Framed {
            status: StatusCode::OK,
            headers,
            body,
        });
    }
    let span = DirectSpan::for_request(range, total_len)
        .ok_or(StreamError::Unsatisfiable { total_len })?;
    span.write_headers(&mut headers);
    let body = if head_only {
        FramedBody::Empty
    } else {
        direct_body(body, span.start, span.len, hold)?
    };
    Ok(Framed {
        status: span.status(),
        headers,
        body,
    })
}

/// The bytes a direct (untranscoded) response sends: the whole object, or
/// the one span a `Range` header asked for. [`frame`], the Subsonic stream
/// and download routes and local file downloads all frame their bodies
/// with this.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectSpan {
    /// First byte sent.
    pub start: u64,
    /// Bytes sent.
    pub len: u64,
    /// Length of the whole object.
    pub total_len: u64,
    /// True when answering a satisfiable range (206).
    pub partial: bool,
}

impl DirectSpan {
    /// Apply the [`parse_range`] rules to an optional `Range` header. No
    /// header sends everything. `None` means 416, a multi-range ask
    /// included.
    pub fn for_request(range: Option<&str>, total_len: u64) -> Option<Self> {
        let Some(text) = range else {
            return Some(Self {
                start: 0,
                len: total_len,
                total_len,
                partial: false,
            });
        };
        parse_range(text, total_len).map(|resolved| Self {
            start: resolved.start,
            len: resolved.end - resolved.start + 1,
            total_len,
            partial: true,
        })
    }

    /// 206 for a range, else 200.
    pub fn status(&self) -> StatusCode {
        if self.partial {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        }
    }

    /// `Accept-Ranges`, `Content-Length`, and `Content-Range` on a 206.
    pub fn write_headers(&self, headers: &mut HeaderMap) {
        headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(self.len));
        if self.partial {
            let last = self.start + self.len.saturating_sub(1);
            if let Ok(value) =
                HeaderValue::from_str(&format!("bytes {}-{last}/{}", self.start, self.total_len))
            {
                headers.insert(header::CONTENT_RANGE, value);
            }
        }
    }
}

/// `len` bytes from `start` of a direct object's body, holding `lease`
/// until a streamed body ends. For a caller that framed the span itself
/// with [`DirectSpan`].
pub fn read_span(
    body: MediaBody,
    lease: Option<super::leases::OwnedDirectLease>,
    start: u64,
    len: u64,
) -> Result<FramedBody, StreamError> {
    direct_body(body, start, len, move |chunks| match lease {
        Some(lease) => lease.hold(chunks),
        None => chunks,
    })
}

/// Body for `len` bytes of a direct object starting at `start`.
fn direct_body(
    body: MediaBody,
    start: u64,
    len: u64,
    hold: impl FnOnce(ChunkStream) -> ChunkStream,
) -> Result<FramedBody, StreamError> {
    match body {
        MediaBody::Bytes(bytes) => {
            let from = usize::try_from(start).unwrap_or(usize::MAX);
            let to = usize::try_from(len)
                .ok()
                .and_then(|len| from.checked_add(len))
                .unwrap_or(usize::MAX);
            if from == 0 && to == bytes.len() {
                return Ok(FramedBody::Bytes(bytes));
            }
            let slice = bytes
                .get(from..to)
                .ok_or_else(|| StreamError::internal("engine short-read the ranged object"))?;
            Ok(FramedBody::Bytes(slice.to_vec()))
        }
        MediaBody::File(path) => Ok(FramedBody::Stream(hold(file_range(path, start, len)))),
        MediaBody::Spans(open) => Ok(FramedBody::Stream(hold(open(start, len)))),
        MediaBody::Chunks(_) | MediaBody::Empty => {
            Err(StreamError::internal("engine returned no direct body"))
        }
    }
}

/// Read size of one file chunk.
const FILE_CHUNK: u64 = 64 * 1024;

/// Stream `len` bytes of `path` from `start`. The file is opened when the
/// stream is first polled. A file that ends early fails the stream instead
/// of sending short.
pub fn file_range(path: PathBuf, start: u64, len: u64) -> ChunkStream {
    use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};

    struct Cursor {
        path: PathBuf,
        file: Option<tokio::fs::File>,
        start: u64,
        remaining: u64,
    }
    async fn next(mut cursor: Cursor) -> std::io::Result<(Vec<u8>, Cursor)> {
        let mut file = match cursor.file.take() {
            Some(file) => file,
            None => {
                let mut file = tokio::fs::File::open(&cursor.path).await?;
                file.seek(std::io::SeekFrom::Start(cursor.start)).await?;
                file
            }
        };
        let want = usize::try_from(cursor.remaining.min(FILE_CHUNK)).unwrap_or(0);
        let mut buf = vec![0u8; want];
        let read = file.read(&mut buf).await?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "file ended before the advertised length",
            ));
        }
        buf.truncate(read);
        cursor.remaining = cursor.remaining.saturating_sub(read as u64);
        cursor.file = Some(file);
        Ok((buf, cursor))
    }
    let cursor = Cursor {
        path,
        file: None,
        start,
        remaining: len,
    };
    futures_util::stream::unfold(Some(cursor), |state| async move {
        let cursor = state.filter(|cursor| cursor.remaining > 0)?;
        Some(match next(cursor).await {
            Ok((buf, cursor)) => (Ok(buf), Some(cursor)),
            Err(error) => (Err(error), None),
        })
    })
    .boxed()
}

/// Upstream content type, infallible: remotes resolve their own type, and an
/// unparseable one must not 500 the stream, so it falls back to `audio/mpeg`.
fn upstream_content_type(content_type: &str) -> HeaderValue {
    HeaderValue::from_str(content_type).unwrap_or_else(|_| HeaderValue::from_static("audio/mpeg"))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn transcode_media() -> StreamMedia {
        let bytes = b"fake-transcoded-mp3-bytes".to_vec();
        StreamMedia {
            content_type: "audio/mpeg".to_owned(),
            total_len: 0,
            transcoded: true,
            estimated_len: Some(bytes.len() as u64),
            body: MediaBody::Bytes(bytes),
            lease: None,
        }
    }

    #[test]
    fn transcode_sets_length_only_with_estimate() {
        let estimated = frame(transcode_media(), None, false, true).expect("frames");
        assert_eq!(
            estimated.headers.get(header::CONTENT_LENGTH),
            Some(&HeaderValue::from_static("25"))
        );

        let plain = frame(transcode_media(), None, false, false).expect("frames");
        assert_eq!(plain.headers.get(header::CONTENT_LENGTH), None);
        assert_eq!(
            plain.headers.get(header::ACCEPT_RANGES),
            Some(&HeaderValue::from_static("none"))
        );
    }

    #[tokio::test]
    async fn the_lease_lasts_until_the_framed_stream_is_gone() {
        let gate = Arc::new(super::super::leases::DirectGate::new());
        let lease = gate.acquire_owned("u1").await.expect("slot");
        let media = StreamMedia {
            content_type: "audio/mpeg".to_owned(),
            total_len: 3,
            transcoded: false,
            estimated_len: None,
            body: MediaBody::Spans(Box::new(|_, _| {
                futures_util::stream::iter(vec![Ok(b"ab".to_vec()), Ok(b"c".to_vec())]).boxed()
            })),
            lease: Some(lease),
        };
        let framed = frame(media, None, false, false).expect("frames");
        assert_eq!(gate.active(), 1, "held while the body waits");
        let FramedBody::Stream(mut chunks) = framed.body else {
            panic!("a span streams");
        };
        let mut read = Vec::new();
        while let Some(chunk) = chunks.next().await {
            read.extend(chunk.expect("reads"));
        }
        assert_eq!(read, b"abc");
        drop(chunks);
        assert_eq!(gate.active(), 0, "released once the stream is done");
    }

    #[test]
    fn range_matrix_matches_v2() {
        let total = 100;
        assert_eq!(
            parse_range("bytes=0-9", total),
            Some(ByteRange { start: 0, end: 9 })
        );
        assert_eq!(
            parse_range("bytes=90-", total),
            Some(ByteRange { start: 90, end: 99 })
        );
        assert_eq!(
            parse_range("bytes=-10", total),
            Some(ByteRange { start: 90, end: 99 })
        );
        assert_eq!(
            parse_range("bytes=-1000", total),
            Some(ByteRange { start: 0, end: 99 })
        );
        assert_eq!(
            parse_range("bytes=0-9999", total),
            Some(ByteRange { start: 0, end: 99 })
        );
        assert_eq!(
            parse_range("bytes=99-99", total),
            Some(ByteRange { start: 99, end: 99 })
        );
        for bad in [
            "bytes=100-",
            "bytes=50-40",
            "bytes=-0",
            "bytes=-",
            "bytes=",
            "bytes=abc",
            "bytes=0-1,2-3",
            "items=0-9",
            "bytes=0 - 9",
            "",
        ] {
            assert_eq!(parse_range(bad, total), None, "{bad:?}");
        }
        assert_eq!(parse_range("bytes=0-", 0), None);
        assert_eq!(parse_range("bytes=-5", 0), None);
    }
}
