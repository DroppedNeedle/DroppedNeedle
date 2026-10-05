//! Audio response bodies for both compat protocols.
//!
//! Audio either sits in memory (covers, test doubles, whole remote reads)
//! or streams: file bytes read by range, or transcode output chunk by
//! chunk. A streaming body carries the stream lease inside it, so the slot
//! stays held until the last byte is sent or the client goes away.

use std::sync::{Arc, Mutex};

use futures_util::StreamExt as _;

use crate::stream::leases::OwnedDirectLease;
use crate::stream::routes::ChunkStream;

/// A chunk stream that can sit in cloneable protocol values: it is taken
/// out exactly once, by whoever builds the HTTP response.
#[derive(Clone)]
pub struct LiveBody(Arc<Mutex<Option<ChunkStream>>>);

impl LiveBody {
    /// Wrap a stream, holding `lease` until the stream ends or drops.
    pub fn new(chunks: ChunkStream, lease: Option<OwnedDirectLease>) -> Self {
        let chunks = match lease {
            Some(lease) => chunks
                .map(move |chunk| {
                    let _held = &lease;
                    chunk
                })
                .boxed(),
            None => chunks,
        };
        Self(Arc::new(Mutex::new(Some(chunks))))
    }

    /// The stream, the first time; `None` after.
    pub fn take(&self) -> Option<ChunkStream> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take()
    }

    /// Read the whole stream into memory (tests and small bodies).
    pub async fn collect(&self) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::new();
        if let Some(mut chunks) = self.take() {
            while let Some(chunk) = chunks.next().await {
                out.extend(chunk?);
            }
        }
        Ok(out)
    }
}

impl std::fmt::Debug for LiveBody {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LiveBody")
    }
}

/// Two handles are equal when they share one stream.
impl PartialEq for LiveBody {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for LiveBody {}

/// An audio body: bytes in memory or a live stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AudioBody {
    /// The whole body in memory.
    Bytes(Vec<u8>),
    /// Streamed to the client as it is read.
    Live(LiveBody),
}

impl AudioBody {
    /// No body.
    pub fn empty() -> Self {
        Self::Bytes(Vec::new())
    }

    /// The bytes, when the body is in memory.
    pub fn bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(bytes) => Some(bytes),
            Self::Live(_) => None,
        }
    }

    /// True for an in-memory empty body.
    pub fn is_empty(&self) -> bool {
        self.bytes().is_some_and(<[u8]>::is_empty)
    }

    /// The whole body in memory (tests and small bodies).
    pub async fn collect(self) -> std::io::Result<Vec<u8>> {
        match self {
            Self::Bytes(bytes) => Ok(bytes),
            Self::Live(live) => live.collect().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stream::leases::DirectGate;

    #[tokio::test]
    async fn the_lease_lasts_until_the_stream_is_gone() {
        let gate = Arc::new(DirectGate::new());
        let lease = gate.acquire_owned("u1").await.expect("slot");
        let chunks =
            futures_util::stream::iter(vec![Ok(b"ab".to_vec()), Ok(b"c".to_vec())]).boxed();
        let body = LiveBody::new(chunks, Some(lease));
        assert_eq!(gate.active(), 1, "held while the body waits");
        assert_eq!(body.collect().await.expect("reads"), b"abc");
        assert_eq!(gate.active(), 0, "released once the stream is done");
    }
}
