//! The loop behind the change events: wait for the database to commit,
//! read a value, publish it when it moved.
//!
//! The lane announces every commit that changed a row ([`Db::commits`]).
//! The first commit after a quiet spell is read at once; within a burst,
//! reads come at most every [`MIN_GAP`], so a burst of commits becomes one
//! read and at most one event. A read that fails is tried again after
//! [`MIN_GAP`] plus jitter rather than left for a commit that may not come.
//! The loop ends when `stop` flips or the lane shuts down.

use std::future::Future;
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

use crate::db::{Db, DbError};
use crate::jobs::{Schedule, SplitMix64};

/// Shortest gap between two reads.
pub const MIN_GAP: Duration = Duration::from_secs(2);

/// Most jitter added to the wait after a failed read.
const RETRY_JITTER: Duration = Duration::from_millis(500);

/// Follow the lane's commits: `read` after each change (paced), and
/// `publish` each value that differs from the last one published.
pub(super) async fn follow<T, R, Fut, P>(
    db: Db,
    mut stop: watch::Receiver<bool>,
    name: &'static str,
    read: R,
    publish: P,
) where
    T: PartialEq,
    R: Fn(Db) -> Fut,
    Fut: Future<Output = Result<T, DbError>>,
    P: Fn(&T),
{
    let mut commits = db.commits();
    let retry = Schedule::new(MIN_GAP).with_jitter(RETRY_JITTER);
    let mut rng = SplitMix64::new(SplitMix64::seed_from_time());
    let mut previous: Option<T> = None;
    loop {
        if *stop.borrow() || commits.has_changed().is_err() {
            return;
        }
        // Seen before reading, so a commit that lands during the read
        // wakes the next round.
        commits.mark_unchanged();
        match read(db.clone()).await {
            Ok(value) => {
                if previous.as_ref() != Some(&value) {
                    publish(&value);
                    previous = Some(value);
                }
            }
            Err(DbError::LaneClosed) => return,
            Err(error) => {
                tracing::warn!(%error, read = name, "change event read failed; retrying");
                if !pause(&mut stop, Instant::now() + retry.next_delay(&mut rng, 0)).await {
                    return;
                }
                continue;
            }
        }
        let read_at = Instant::now();
        tokio::select! {
            changed = commits.changed() => {
                if changed.is_err() {
                    return;
                }
            }
            changed = stop.changed() => {
                if changed.is_err() {
                    return;
                }
                continue;
            }
        }
        if !pause(&mut stop, read_at + MIN_GAP).await {
            return;
        }
    }
}

/// Wait until `until`, or until `stop` flips. False when the loop should
/// end.
async fn pause(stop: &mut watch::Receiver<bool>, until: Instant) -> bool {
    tokio::select! {
        () = tokio::time::sleep_until(until) => true,
        changed = stop.changed() => changed.is_ok() && !*stop.borrow(),
    }
}
