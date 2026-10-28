//! Release polling for the flows loops: [`MusicBrainzReleasePoll`] reads a
//! followed artist's release groups for the follow poll.

use futures_util::future::BoxFuture;

use super::flows::seams::{ObservedRelease, ReleasePoll};
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::musicbrainz::{Criticality, LiveSource, MusicBrainzView};

/// Release groups read per page (the MusicBrainz browse maximum).
const RELEASE_PAGE: u32 = 100;
/// Pages read per artist at most: 1,000 release groups covers all but a
/// handful of artists, and bounds one poll's provider work.
const MAX_RELEASE_PAGES: u32 = 10;

/// The follow poll of a test bundle with no MusicBrainz: every poll
/// fails, so no artist is baselined against an empty page.
#[cfg(any(test, feature = "test-support"))]
pub struct NoReleasePoll;

#[cfg(any(test, feature = "test-support"))]
impl ReleasePoll for NoReleasePoll {
    fn poll_releases<'a>(
        &'a self,
        _artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ObservedRelease>, String>> {
        Box::pin(async { Err("MusicBrainz is not available".to_owned()) })
    }
}

/// The artist's release groups from the MusicBrainz browse endpoint,
/// every page, at background priority on the shared limiter. The
/// browse list is complete, so a release cannot hide behind search
/// ranking. A provider outage is an error, never an empty page.
pub struct MusicBrainzReleasePoll {
    client: MusicBrainzView,
}

impl MusicBrainzReleasePoll {
    /// Poll over one client (callers set background priority on it).
    pub fn new(client: MusicBrainzView) -> Self {
        Self { client }
    }
}

impl ReleasePoll for MusicBrainzReleasePoll {
    fn poll_releases<'a>(
        &'a self,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ObservedRelease>, String>> {
        Box::pin(async move {
            let mut out = Vec::new();
            for page in 0..MAX_RELEASE_PAGES {
                let offset = page * RELEASE_PAGE;
                let found = self
                    .client
                    .browse_artist_release_groups(
                        artist_mbid,
                        RELEASE_PAGE,
                        offset,
                        Criticality::IdentityCritical,
                    )
                    .await
                    .map_err(|error| error.to_string())?;
                let read = found.items.len();
                out.extend(found.items.into_iter().map(|group| ObservedRelease {
                    rg_mbid: group.id,
                    title: group.title.unwrap_or_default(),
                    first_release_date: group.first_release_date,
                    primary_type: group.primary_type,
                    secondary_types: group.secondary_types,
                }));
                if read == 0 || u64::from(offset) + read as u64 >= found.count {
                    return Ok(out);
                }
            }
            tracing::debug!(artist_mbid, "follow poll read the page cap for this artist");
            Ok(out)
        })
    }
}

/// The production poll: background priority on the shared limiter, the
/// configured MusicBrainz source read per request, failures counted
/// toward system health.
pub fn live_release_poll(
    providers: &droppedneedle_providers::Providers,
    source: LiveSource,
) -> MusicBrainzReleasePoll {
    MusicBrainzReleasePoll::new(
        providers
            .musicbrainz
            .live(source, RequestPriority::BackgroundSync),
    )
}
