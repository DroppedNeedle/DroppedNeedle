//! How much background reading the day has room for, and which facts the
//! refresh job may read.
//!
//! The ceiling counts every background read Discover makes to ListenBrainz
//! (either host) or the official MusicBrainz service: 60 a day plus 30 for
//! each person seen in the last seven days, never over 300, and none at
//! all while nobody has been seen. BrainzMash and mirrors are not counted.

use droppedneedle_kernel::db::DbError;

use super::{DAY_MS, FactKind, KeptFacts};

/// How recently a person must have been seen for work to run for them, and
/// how recently a fact must have been wanted or read to be refreshed.
pub const SEEN_WINDOW_MS: i64 = 7 * DAY_MS;
/// The day's reads with one person seen, before the people are added.
pub const CEILING_BASE: u64 = 60;
/// The day's reads each person seen adds.
pub const CEILING_PER_PERSON: u64 = 30;
/// The most reads in a day, however many people were seen.
pub const CEILING_MAX: u64 = 300;
/// The most reads one run of the refresh job makes.
pub const RUN_READS: u64 = 100;

/// The day's ceiling with `seen` people seen in the last seven days.
pub fn ceiling(seen: usize) -> u64 {
    if seen == 0 {
        return 0;
    }
    let seen = u64::try_from(seen).unwrap_or(u64::MAX);
    CEILING_BASE
        .saturating_add(CEILING_PER_PERSON.saturating_mul(seen))
        .min(CEILING_MAX)
}

/// The kinds the job may read: ListenBrainz's only while its reads are on,
/// album lists and band links only while the MusicBrainz settings read.
pub(crate) fn kinds(listenbrainz_on: bool, musicbrainz_on: bool) -> Vec<FactKind> {
    let mut kinds = Vec::new();
    if listenbrainz_on {
        kinds.extend([FactKind::Neighbours, FactKind::Listeners]);
    }
    if musicbrainz_on {
        kinds.extend([FactKind::Albums, FactKind::BandLinks]);
    }
    if listenbrainz_on {
        kinds.push(FactKind::AlbumCounts);
    }
    kinds
}

/// How many reads the day has left: the ceiling for the people seen less
/// what was counted today.
///
/// # Errors
///
/// A read's failure.
pub(crate) async fn room(facts: &KeptFacts) -> Result<u64, DbError> {
    let seen = facts.seen().await?;
    let ceiling = ceiling(seen);
    if ceiling == 0 {
        return Ok(0);
    }
    Ok(ceiling.saturating_sub(facts.reads_today().await?))
}

/// Whether the refresh job has work now: a fact of `kinds` to read and room
/// in the day.
///
/// # Errors
///
/// A read's failure.
pub(crate) async fn work_waiting(facts: &KeptFacts, kinds: &[FactKind]) -> Result<bool, DbError> {
    if kinds.is_empty() || !facts.waiting(kinds).await? {
        return Ok(false);
    }
    Ok(room(facts).await? > 0)
}
