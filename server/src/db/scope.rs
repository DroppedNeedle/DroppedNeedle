//! Which write and read closures the current thread is inside, and the
//! checked way for synchronous code to wait on a future.
//!
//! A write closure runs on one thread from start to end: the caller's for
//! a blocking write, the lane's for an async one. The lane records it here
//! for exactly that long, and reads the record before queueing a write, so
//! a closure that writes to its own lane is refused instead of parking
//! behind itself for ever. [`block_on`] reads it too, so no closure holds
//! the lane while it waits on the network.
//!
//! A read closure is recorded the same way by the read pools, under the
//! lane of the database it reads. Inside one, a write, another pooled read
//! and [`block_on`] are each refused: the closure holds a connection and
//! pins the WAL for as long as it runs. And a pooled read inside a write
//! closure is refused, because it would not see the transaction's own rows.
//!
//! The record is per thread. A closure that waits on another thread or task
//! which itself reads or writes the same database is not seen here.

use std::{
    cell::RefCell,
    future::Future,
    marker::PhantomData,
    sync::atomic::{AtomicU64, Ordering},
    thread::LocalKey,
};

use tokio::runtime::Handle;

use super::error::DbError;

thread_local! {
    /// The lanes whose write closures this thread is inside, outermost first.
    static WRITES: RefCell<Vec<LaneId>> = const { RefCell::new(Vec::new()) };
    /// The lanes of the databases whose read closures this thread is inside.
    static READS: RefCell<Vec<LaneId>> = const { RefCell::new(Vec::new()) };
}

type Record = LocalKey<RefCell<Vec<LaneId>>>;

/// One writer lane, told apart from every other lane in the process. A
/// database has one lane, so this is also what tells databases apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LaneId(u64);

impl LaneId {
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Marks the current thread as inside a closure on one lane's database
/// until dropped, on return and on unwind alike. Not `Send`: it must be
/// dropped on the thread that made it.
pub(crate) struct Inside {
    record: &'static Record,
    lane: LaneId,
    same_thread: PhantomData<*const ()>,
}

fn enter(record: &'static Record, lane: LaneId) -> Inside {
    record.with_borrow_mut(|open| open.push(lane));
    Inside {
        record,
        lane,
        same_thread: PhantomData,
    }
}

pub(crate) fn enter_write(lane: LaneId) -> Inside {
    enter(&WRITES, lane)
}

pub(crate) fn enter_read(lane: LaneId) -> Inside {
    enter(&READS, lane)
}

impl Drop for Inside {
    fn drop(&mut self) {
        // The thread's record may already be gone if the thread is ending.
        let _ = self.record.try_with(|open| {
            let mut open = open.borrow_mut();
            if let Some(at) = open.iter().rposition(|lane| *lane == self.lane) {
                open.remove(at);
            }
        });
    }
}

/// Drive a future to completion from synchronous code: the one way sync
/// code that also writes may wait on async work, such as a network call.
/// `what` names the wait in the error.
///
/// Refused on a thread that is inside a write closure, on any database: the
/// lane would be held for as long as the wait lasts. Do the wait first and
/// write what it returned. Refused inside a read closure too, on any
/// database, where a connection and its snapshot would be held across the
/// wait. Like `Handle::block_on`, this is for a blocking thread and panics
/// on a thread that is driving async tasks.
pub fn block_on<T>(
    handle: &Handle,
    what: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, DbError> {
    if inside_any(&WRITES) {
        return Err(DbError::WaitInsideWrite {
            operation: what.to_owned(),
        });
    }
    if inside_any(&READS) {
        return Err(DbError::InsideRead {
            operation: what.to_owned(),
        });
    }
    Ok(handle.block_on(future))
}

/// A thread whose record is already gone is ending, not inside a closure.
fn inside_any(record: &'static Record) -> bool {
    record
        .try_with(|open| !open.borrow().is_empty())
        .unwrap_or(false)
}

fn inside(record: &'static Record, lane: LaneId) -> bool {
    record
        .try_with(|open| open.borrow().contains(&lane))
        .unwrap_or(false)
}

/// True when the current thread is inside a write closure on `lane`.
pub(crate) fn inside_write_on(lane: LaneId) -> bool {
    inside(&WRITES, lane)
}

/// True when the current thread is inside a read closure on the database
/// `lane` writes.
pub(crate) fn inside_read_on(lane: LaneId) -> bool {
    inside(&READS, lane)
}
