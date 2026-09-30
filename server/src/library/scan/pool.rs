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
    /// ever run concurrently.
    pub async fn run<F, R>(&self, job: F) -> R
    where
        F: FnOnce() -> R + Send + 'static,
        R: Send + 'static,
    {
        // The semaphore is owned by this pool and never closed; a
        // closure here means a bug, so the worker parks (logged) rather
        // than panicking the scan.
        let _permit = match self.semaphore.acquire().await {
            Ok(permit) => permit,
            Err(error) => {
                tracing::error!(%error, "blocking pool semaphore closed; parking worker");
                std::future::pending().await
            }
        };
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        let in_flight = Arc::clone(&self.in_flight);
        let completed = Arc::clone(&self.completed);
        // Jobs must not panic (crate rule); a panicking job parks
        // (logged) instead of taking the scan worker down with it.
        let result = match tokio::task::spawn_blocking(move || {
            let _guard = InFlightGuard(in_flight);
            job()
        })
        .await
        {
            Ok(result) => result,
            Err(error) => {
                tracing::error!(%error, "blocking pool job panicked; parking worker");
                std::future::pending().await
            }
        };
        completed.fetch_add(1, Ordering::Relaxed);
        result
    }
}

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
                .await;
            }));
        }
        for handle in handles {
            handle.await.expect("job joins");
        }
        assert!(peak.load(Ordering::SeqCst) <= 2, "peak exceeded the bound");
        assert_eq!(pool.completed(), 8);
    }
}
