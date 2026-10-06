//! The network source behind cover art: Cover Art Archive front covers.

use futures_util::future::BoxFuture;

use crate::providers::coverart::{
    ArtworkBytes, CaaClient, CaaError, CaaTransport, DownloadSize, EntityKind, RateGate,
};

use super::MAX_REMOTE_BYTES;

/// Cover fetch pacing toward the archive. The archive documents no limit
/// and serves bytes from the Internet Archive CDN; v2 used 10 per second
/// (burst 20) so a cold grid of covers does not load one per second.
pub const COVER_FETCH_RATE_PER_SEC: f64 = 10.0;

/// Fetch one front cover. Object-safe so the service holds any client.
pub trait RemoteCovers: Send + Sync {
    /// Front cover bytes, `Ok(None)` when the archive has none.
    fn front<'a>(
        &'a self,
        entity: EntityKind,
        mbid: &'a str,
        size: DownloadSize,
    ) -> BoxFuture<'a, Result<Option<ArtworkBytes>, CaaError>>;
}

impl<T: CaaTransport + Send + Sync + 'static> RemoteCovers for CaaClient<T> {
    fn front<'a>(
        &'a self,
        entity: EntityKind,
        mbid: &'a str,
        size: DownloadSize,
    ) -> BoxFuture<'a, Result<Option<ArtworkBytes>, CaaError>> {
        Box::pin(self.fetch_front(entity, mbid, size, MAX_REMOTE_BYTES))
    }
}

/// The archive client covers use: the shared no-redirect HTTP client (each
/// redirect hop is checked by the client) at the cover pacing.
pub fn cover_client<T: CaaTransport>(transport: T) -> CaaClient<T> {
    CaaClient::new(transport).with_gate(RateGate::new(COVER_FETCH_RATE_PER_SEC))
}
