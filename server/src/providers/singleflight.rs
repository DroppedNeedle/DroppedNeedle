//! Singleflight request coalescing.
//!
//! When several tasks ask for the same upstream object at once (a page load
//! fanning out the same artist lookup, a stampede after a cache sweep),
//! [`Singleflight`] runs the fetch once and shares the result. Followers get
//! a clone of the leader's `Arc`'d value or error; a new key, or a key whose
//! flight already landed, starts a fresh flight. Ports v2's
//! `RequestDeduplicator`, including `clear()` for endpoint changes.
//!
//! Cancellation is safe: if the leader's future drops, followers keep
//! driving the shared flight to completion instead of inheriting a
//! cancellation, and latecomers start a new flight rather than reading a
//! stale one.

use std::{collections::HashMap, sync::Arc};

use futures_util::{
    FutureExt as _,
    future::{BoxFuture, Shared},
};
use tokio::sync::Mutex;

use super::error::ProviderError;

type FlightResult<T, E> = Result<Arc<T>, Arc<E>>;
type Flight<T, E> = Shared<BoxFuture<'static, FlightResult<T, E>>>;

/// Coalesces concurrent identical fetches. `Clone` shares the flight table;
/// each provider client holds one per response shape it coalesces. The
/// error type defaults to the core [`ProviderError`]; services that
/// coalesce whole page builds share their own typed error instead.
pub struct Singleflight<T, E = ProviderError> {
    pending: Arc<Mutex<HashMap<String, Flight<T, E>>>>,
}

impl<T, E> Clone for Singleflight<T, E> {
    fn clone(&self) -> Self {
        Self {
            pending: Arc::clone(&self.pending),
        }
    }
}

impl<T, E> std::fmt::Debug for Singleflight<T, E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Singleflight").finish_non_exhaustive()
    }
}

impl<T: Send + Sync + 'static, E: Send + Sync + 'static> Singleflight<T, E> {
    /// An empty flight table.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Run `factory` under `key`, coalescing with any flight already in the
    /// air for that key. The result (value or typed error) is shared with
    /// every follower.
    pub async fn run<F, Fut>(&self, key: &str, factory: F) -> FlightResult<T, E>
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = Result<T, E>> + Send + 'static,
    {
        let flight = {
            let mut pending = self.pending.lock().await;
            match pending.get(key) {
                Some(existing) if existing.peek().is_none() => existing.clone(),
                _ => {
                    // No flight, or a landed one a cancelled leader left
                    // behind: this caller becomes the leader.
                    let flight = async move { factory().await.map(Arc::new).map_err(Arc::new) }
                        .boxed()
                        .shared();
                    pending.insert(key.to_owned(), flight.clone());
                    flight
                }
            }
        };
        let outcome = flight.await;
        // Leaders clean up after landing; a `try_lock` miss just leaves the
        // entry for the peek check above to sweep on the next call.
        if let Ok(mut pending) = self.pending.try_lock() {
            pending.remove(key);
        }
        outcome
    }

    /// Drop every in-flight entry (for endpoint or credential changes).
    /// Current waiters still receive their flight's result; new callers
    /// start fresh flights.
    pub async fn clear(&self) {
        self.pending.lock().await.clear();
    }

    /// Flights currently in the air.
    pub async fn pending_count(&self) -> usize {
        self.pending.lock().await.len()
    }
}

impl<T: Send + Sync + 'static, E: Send + Sync + 'static> Default for Singleflight<T, E> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn concurrent_callers_share_one_flight() {
        let flights = Singleflight::<String>::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let flights = flights.clone();
            let runs = Arc::clone(&runs);
            handles.push(tokio::spawn(async move {
                let owned = runs;
                flights
                    .run("mb:rg:detail:x", move || {
                        let runs = owned;
                        async move {
                            runs.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
                            Ok::<_, ProviderError>("album".to_owned())
                        }
                    })
                    .await
            }));
        }
        for handle in handles {
            let outcome = handle.await.expect("joins");
            assert_eq!(outcome.as_deref().map(String::as_str), Ok("album"));
        }
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(flights.pending_count().await, 0);
    }

    #[tokio::test]
    async fn failures_are_shared_not_retried_per_follower() {
        let flights = Singleflight::<String>::new();
        let runs = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&runs);
        let first = {
            let flights = flights.clone();
            tokio::spawn(async move {
                let owned = Arc::clone(&counter);
                flights
                    .run("lfm:x", move || {
                        let runs = owned;
                        async move {
                            runs.fetch_add(1, Ordering::SeqCst);
                            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                            Err::<String, _>(ProviderError::Transport {
                                provider: "lastfm",
                                message: "reset".to_owned(),
                            })
                        }
                    })
                    .await
            })
        };
        // Start the follower while the leader still flies.
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        let second = flights.run("lfm:x", || async {
            Ok::<_, ProviderError>("must-not-run".to_owned())
        });
        let (first, second) = tokio::join!(first, second);
        let first = first.expect("joins");
        assert!(first.is_err());
        assert!(second.is_err());
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn different_keys_fly_separately_and_landed_keys_refly() {
        let flights = Singleflight::<u32>::new();
        let (left, right) = tokio::join!(
            flights.run("a", || async { Ok::<_, ProviderError>(1) }),
            flights.run("b", || async { Ok::<_, ProviderError>(2) }),
        );
        assert_eq!(left.as_deref(), Ok(&1));
        assert_eq!(right.as_deref(), Ok(&2));
        // After landing, the same key runs again instead of replaying.
        let runs = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let runs = Arc::clone(&runs);
            flights
                .run("a", || async move {
                    runs.fetch_add(1, Ordering::SeqCst);
                    Ok::<_, ProviderError>(3)
                })
                .await
                .expect("refly succeeds");
        }
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }
}
