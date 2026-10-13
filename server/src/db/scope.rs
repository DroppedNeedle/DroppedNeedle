//! Which write closures the current thread is inside, and the checked way
//! for synchronous code to wait on a future.
//!
//! A write closure runs on one thread from start to end: the caller's for
//! a blocking write, the lane's for an async one. The lane records it here
//! for exactly that long, and reads the record before queueing a write, so
//! a closure that writes to its own lane is refused instead of parking
//! behind itself for ever. [`block_on`] reads it too, so no closure holds
//! the lane while it waits on the network.
//!
//! The record is per thread. A closure that waits on another thread or task
//! which itself writes to the same lane still hangs the lane, and nothing
//! here can see it.

use std::{
    cell::RefCell,
    future::Future,
    marker::PhantomData,
    sync::atomic::{AtomicU64, Ordering},
};

use tokio::runtime::Handle;

use super::error::DbError;

thread_local! {
    /// The lanes whose write closures this thread is inside, outermost first.
    static WRITES: RefCell<Vec<LaneId>> = const { RefCell::new(Vec::new()) };
}

/// One writer lane, told apart from every other lane in the process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LaneId(u64);

impl LaneId {
    pub(crate) fn next() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self(NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// Marks the current thread as inside a write closure on one lane until
/// dropped, on return and on unwind alike. Not `Send`: it must be dropped
/// on the thread that made it.
pub(crate) struct InsideWrite {
    lane: LaneId,
    same_thread: PhantomData<*const ()>,
}

pub(crate) fn enter_write(lane: LaneId) -> InsideWrite {
    WRITES.with_borrow_mut(|writes| writes.push(lane));
    InsideWrite {
        lane,
        same_thread: PhantomData,
    }
}

impl Drop for InsideWrite {
    fn drop(&mut self) {
        // The thread's record may already be gone if the thread is ending.
        let _ = WRITES.try_with(|writes| {
            let mut writes = writes.borrow_mut();
            if let Some(at) = writes.iter().rposition(|lane| *lane == self.lane) {
                writes.remove(at);
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
/// write what it returned. Like `Handle::block_on`, this is for a blocking
/// thread and panics on a thread that is driving async tasks.
pub fn block_on<T>(
    handle: &Handle,
    what: &'static str,
    future: impl Future<Output = T>,
) -> Result<T, DbError> {
    if WRITES.with_borrow(|writes| !writes.is_empty()) {
        return Err(DbError::WaitInsideWrite {
            operation: what.to_owned(),
        });
    }
    Ok(handle.block_on(future))
}

/// True when the current thread is inside a write closure on `lane`.
pub(crate) fn inside_write_on(lane: LaneId) -> bool {
    WRITES.with_borrow(|writes| writes.contains(&lane))
}
