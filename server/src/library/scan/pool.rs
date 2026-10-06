//! Bounded blocking/CPU pool for filesystem and tag work.
//!
//! v2 runs directory walks, root probes, and tag reads on threads outside
//! the event loop (`asyncio.to_thread`, a single-worker probe executor).
//! This pool is the same idea with an explicit bound: at most `max_workers`
//! blocking jobs run at once, and every job rides `spawn_blocking` so the
//! async executor never stalls on disk I/O.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::Semaphore;

/// Default worker count when the caller does not pick one.
pub const DEFAULT_MAX_WORKERS: usize = 4;

/// Bounded pool over `spawn_blocking`. Clone is cheap; all clones share one
/// semaphore, so the bound holds process-wide per pool.
#[derive(Debug, Clone)]
pub struct BlockingPool {
    semaphore: Arc<Semaphore>,
    max_workers: usize,
    in_flight: Arc<AtomicUsize>,
    completed: Arc<AtomicUsize>,
}

impl BlockingPool {
    pub fn new(max_workers: usize) -> Self {
        let max_workers = max_workers.max(1);
        Self {
            semaphore: Arc::new(Semaphore::new(max_workers)),
            max_workers,
            in_flight: Arc::new(AtomicUsize::new(0)),
            completed: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn with_default_workers() -> Self {
        let parallelism = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        Self::new(parallelism.clamp(1, DEFAULT_MAX_WORKERS))
    }

    pub fn max_workers(&self) -> usize {
        self.max_workers
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight.load(Ordering::Relaxed)
    }

    pub fn completed(&self) -> usize {
        self.completed.load(Ordering::Relaxed)
    }

    /// Run `job` on a blocking thread, waiting for a free permit first.
    /// The permit is held for the whole job, so at most `max_workers` jobs
    /// ever run concurrently. A job that panics comes back as an error
    /// the caller records, so one bad file never parks the worker.
    pub async fn run<F, R>(&self, job: F) -> Result<R, PoolError>
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        let _permit = self.semaphore.acquire().await.map_err(|error| {
            tracing::error!(%error, "blocking pool semaphore closed");
            PoolError
        })?;
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        let in_flight = Arc::clone(&self.in_flight);
        let joined = tokio::task::spawn_blocking(move || {
            let _guard = InFlightGuard(in_flight);
            job()
        })
        .await;
        self.completed.fetch_add(1, Ordering::Relaxed);
        joined.map_err(|error| {
            tracing::error!(%error, "blocking pool job panicked");
            PoolError
        })
    }
}

/// A pool job that panicked or could not start. The cause is logged where
/// it happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("blocking pool job failed")]
pub struct PoolError;

struct InFlightGuard(Arc<AtomicUsize>);

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn pool_bounds_concurrency() {
        let pool = BlockingPool::new(2);
        let peak = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let pool = pool.clone();
            let peak = Arc::clone(&peak);
            let current = Arc::clone(&current);
            handles.push(tokio::spawn(async move {
                pool.run(move || {
                    let now = current.fetch_add(1, Ordering::SeqCst) + 1;
                    peak.fetch_max(now, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(10));
                    current.fetch_sub(1, Ordering::SeqCst);
                })
                .await
                .expect("job runs");
            }));
        }
        for handle in handles {
            handle.await.expect("job joins");
        }
        assert!(peak.load(Ordering::SeqCst) <= 2, "peak exceeded the bound");
        assert_eq!(pool.completed(), 8);
    }

    #[tokio::test]
    async fn panicking_job_returns_an_error() {
        let pool = BlockingPool::new(1);
        let outcome = pool.run(|| -> u8 { panic!("bad file") }).await;
        assert_eq!(outcome, Err(PoolError));
        assert_eq!(pool.run(|| 7u8).await, Ok(7));
    }
}
