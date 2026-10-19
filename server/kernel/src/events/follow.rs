//! The loop behind the change events: wait for the database to commit,
//! read a value, publish it when it moved.
//!
//! The lane announces every commit that changed a row ([`Db::commits`]).
//! The loop runs on the supervisor, woken by that counter. The first commit
//! after a quiet spell is read at once; within a burst, reads come at most
//! every [`MIN_GAP`], so a burst of commits becomes one read and at most one
//! event. A read that fails is tried again after a jittered backoff rather
//! than left for a commit that may not come. The loop ends when the
//! supervisor stops it or the lane shuts down.

use std::future::Future;
use std::time::Duration;

use crate::db::{Db, DbError};
use crate::jobs::{
    AlreadyRunning, Next, Schedule, Supervisor, Tick, TickCtx, TickError, TickResult,
};

/// Shortest gap between two reads.
pub const MIN_GAP: Duration = Duration::from_secs(2);

/// Most jitter added to the wait after a failed read.
const RETRY_JITTER: Duration = Duration::from_millis(500);

/// Row code of a read that failed.
const READ_FAILED: &str = "EVENT_READ_FAILED";

/// One read of the followed value, publishing it when it moved.
struct Follow<T, R, P> {
    db: Db,
    name: &'static str,
    read: R,
    publish: P,
    previous: Option<T>,
}

impl<T, R, Fut, P> Tick for Follow<T, R, P>
where
    T: PartialEq + Send + 'static,
    R: Fn(Db) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, DbError>> + Send,
    P: Fn(&T) + Send + 'static,
{
    async fn tick(&mut self, _ctx: &TickCtx) -> TickResult {
        match (self.read)(self.db.clone()).await {
            Ok(value) => {
                if self.previous.as_ref() != Some(&value) {
                    (self.publish)(&value);
                    self.previous = Some(value);
                }
                Ok(Next::Continue)
            }
            Err(DbError::LaneClosed) => Ok(Next::Finish),
            Err(error) => Err(TickError::new(
                READ_FAILED,
                format!("{}: {error}", self.name),
            )),
        }
    }
}

/// Follow the lane's commits under `name`: `read` after each change
/// (paced), and `publish` each value that differs from the last one
/// published. The first read runs at once.
pub(super) fn start<T, R, Fut, P>(
    supervisor: &Supervisor,
    name: &'static str,
    db: Db,
    read: R,
    publish: P,
) -> Result<(), AlreadyRunning>
where
    T: PartialEq + Send + 'static,
    R: Fn(Db) -> Fut + Send + 'static,
    Fut: Future<Output = Result<T, DbError>> + Send,
    P: Fn(&T) + Send + 'static,
{
    let mut commits = db.commits();
    commits.mark_changed();
    supervisor.on_wakeup(
        name,
        commits,
        Schedule::new(Duration::MAX)
            .with_min_delay(MIN_GAP)
            .with_jitter(RETRY_JITTER)
            .with_backoff(Duration::from_secs(1), Duration::from_secs(30)),
        Follow {
            db,
            name,
            read,
            publish,
            previous: None,
        },
    )
}
