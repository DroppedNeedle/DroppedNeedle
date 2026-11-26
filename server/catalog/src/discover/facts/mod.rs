//! Kept facts: the provider answers Discover keeps in the database, so a
//! page build reads local rows and never waits on a provider for them.
//!
//! Four kinds, shared by everyone and nobody's listening: an artist's
//! similar artists (ListenBrainz labs' list, kept per source with the raw
//! score and rank; the order is worked out when read), an artist's
//! listener counts, an artist's album list with each group's counts, and
//! an artist's band links (its MusicBrainz relationships to its members,
//! its bands, founders, subgroups, collaborations and other names).
//! Each fact has one fetch row: when it last answered, how its last read
//! went, when it is next due and when a reader last read or wanted it.
//!
//! - A reader at background priority (a page build, a job) reads rows
//!   only: a fact not kept is recorded as wanted and answers "not yet". A
//!   reader a person is waiting on makes one read for a fact nobody has
//!   asked about and keeps the answer.
//! - A found answer replaces that fact's rows in one write. A clean "none"
//!   is asked again after a day (an album list with nothing to offer after
//!   14, an artist with no band links after 30). A failed read keeps the
//!   rows, backs the next try off and records when the fact first failed.
//! - One job kind ([`job::KIND`]) does the background reads: wanted facts
//!   first, then the oldest kept ones past their age that someone read in
//!   the last seven days, under the daily ceiling of [`plan::ceiling`].
//! - Every background read Discover makes to ListenBrainz or MusicBrainz
//!   is counted by day and source ([`KeptFacts::count_read`]), so a restart
//!   does not reset the ceiling. A read a person waits on is not counted.
//!
//! These tables hold provider facts written by this module only. They are
//! not a read model: nothing derives them from other tables.

pub mod job;
pub mod plan;
mod store;

pub use store::KeptFacts;
pub(crate) use store::{Counts, FactRef, Kept, Looked, Neighbour, Notes};

use std::time::Duration;

use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_providers::ProviderError;
use droppedneedle_providers::listenbrainz::LABS_SOURCE;

/// A day, ms.
pub(crate) const DAY_MS: i64 = 86_400_000;

/// What a labs read answers when it is not made because labs rests after
/// a failure, `wait` still to go: nothing is sent, counted or marked
/// failed.
pub(crate) fn labs_resting(wait: Duration) -> ProviderError {
    ProviderError::Unavailable {
        provider: LABS_SOURCE,
        retry_after: Some(wait),
    }
}

/// A read's failure, as the provider failure behind it when there is one.
pub(crate) trait ReadError {
    /// The provider's own failure, if this is one.
    fn provider(&self) -> Option<&ProviderError>;

    /// Whether the read never left the server ([`unsent`]).
    fn unsent(&self) -> bool {
        self.provider().is_some_and(unsent)
    }
}

impl ReadError for ProviderError {
    fn provider(&self) -> Option<&ProviderError> {
        Some(self)
    }
}

impl ReadError for crate::discover::listening::ListeningError {
    fn provider(&self) -> Option<&ProviderError> {
        match self {
            Self::Provider(error) => Some(error),
            Self::MediaServer(_) => None,
        }
    }
}

/// Whether a read that failed with `error` never left the server: an id
/// that is not an MBID, or a service not set up. Such a read is not
/// counted; its fact is still marked failed, so it backs off. A labs read
/// is not made at all while labs rests, which the reader checks first.
pub(crate) fn unsent(error: &ProviderError) -> bool {
    matches!(
        error,
        ProviderError::Invalid { .. } | ProviderError::NotConfigured { .. }
    )
}

/// What kind of fact a fetch row is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FactKind {
    /// An artist's similar artists.
    Neighbours,
    /// An artist's listener and listen counts.
    Listeners,
    /// An artist's albums that may be offered.
    Albums,
    /// The listener counts of an artist's albums.
    AlbumCounts,
    /// An artist's links to its members, bands and other names.
    BandLinks,
}

droppedneedle_kernel::spelled!(FactKind {
    Neighbours => "neighbours",
    Listeners => "listeners",
    Albums => "albums",
    AlbumCounts => "album_counts",
    BandLinks => "band_links",
});

/// Where a fact is read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FactSource {
    /// ListenBrainz labs' similar-artists list.
    Labs,
    /// Last.fm's similar artists. First written by later work.
    LastFm,
    /// ListenBrainz's popularity counts.
    ListenBrainz,
    /// The install's MusicBrainz source (discographies and band links).
    MusicBrainz,
}

droppedneedle_kernel::spelled!(FactSource {
    Labs => "listenbrainz_labs",
    LastFm => "lastfm",
    ListenBrainz => "listenbrainz",
    MusicBrainz => "musicbrainz",
});

/// Where a kept similar-artists list came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListSource {
    /// ListenBrainz labs' co-listening list.
    Labs,
    /// Last.fm's similar artists. First written by later work.
    LastFm,
}

droppedneedle_kernel::spelled!(ListSource {
    Labs => "listenbrainz_labs",
    LastFm => "lastfm",
});

/// How a fact's last read went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// It answered with rows.
    Found,
    /// It answered cleanly with nothing.
    None,
    /// It failed; any rows it had are kept.
    Failed,
}

droppedneedle_kernel::spelled!(Outcome {
    Found => "found",
    None => "none",
    Failed => "failed",
});

/// The MetaBrainz services a background read is counted against.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MeteredSource {
    /// ListenBrainz's API.
    ListenBrainz,
    /// ListenBrainz labs.
    Labs,
    /// The official MusicBrainz service. Mirrors and BrainzMash are not
    /// counted.
    MusicBrainz,
}

droppedneedle_kernel::spelled!(MeteredSource {
    ListenBrainz => "listenbrainz",
    Labs => "listenbrainz_labs",
    MusicBrainz => "musicbrainz",
});

/// Which reader wanted a fact, first served first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum WantOrder {
    /// A build's seed artists and what ranks their lists.
    Seed = 0,
    /// The other artists a build reads (their album lists).
    Pool = 1,
    /// The library precache's artists.
    Library = 2,
}

/// Empty the kept facts and their fetch rows (the admin's "Clear provider
/// data"). The read counter is kept: it meters what was sent, not what is
/// held. Answers how many fact rows went.
///
/// # Errors
///
/// The write's failure.
pub async fn clear(db: &Db) -> Result<u64, DbError> {
    store::clear(db).await
}

/// How many fact rows are kept, for the cache stats.
///
/// # Errors
///
/// The read's failure.
pub async fn kept_rows(db: &Db) -> Result<u64, DbError> {
    store::kept_rows(db).await
}

/// A person's ListenBrainz history reads draw on the same day as
/// Discover's other background reads: the room left under
/// [`plan::ceiling`], and each read counted against ListenBrainz.
impl droppedneedle_media::history::ReadBudget for KeptFacts {
    fn room(
        &self,
    ) -> droppedneedle_media::playback::listening::BoxFuture<'_, Result<u64, DbError>> {
        Box::pin(plan::room(self))
    }

    fn count(&self, reads: u64) -> droppedneedle_media::playback::listening::BoxFuture<'_, ()> {
        Box::pin(self.count_read(MeteredSource::ListenBrainz, reads))
    }
}
