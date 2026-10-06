//! Stage-13 fix B: the identify drain yields to shutdown promptly.
//!
//! Briefs (small seeded drains, no provider waits):
//!
//! - `identify_tick_pre_signalled_shutdown_attempts_nothing`: quiet
//!   shutdown sanity, zero jobs attempted.
//! - `identify_tick_abandons_drain_after_mid_attempt_shutdown`: a
//!   shutdown landing mid-drain stops after the running attempt; the
//!   rest stay queued and untouched.
//! - `identify_tick_abandons_in_flight_gate_wait_on_shutdown`: a
//!   shutdown landing while an attempt waits yields at once instead
//!   of waiting out the gate.
//! - `identify_tick_without_signal_still_drains_everything`: no
//!   signal keeps the old full-drain behavior.

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use droppedneedle::auth::prod::ProdAuth;
use droppedneedle::auth::users::stores::SystemClock;
use droppedneedle::auth::wiring::AuthSetup;
use droppedneedle::db::{DbConfig, DbRuntime, open_runtime};
use droppedneedle::http_client::HttpClientFactory;
use droppedneedle::ids::{IdGenerator, UuidGenerator};
use droppedneedle::library::identify::models::{
    IdentifyKind, JobState, LocalAlbumFacts, RecallResult,
};
use droppedneedle::library::identify::providers::{IdentifyProviders, RecallOutcome};
use droppedneedle::library::identify::stores::{IdentityStore, QueueStore};
use droppedneedle::library::wiring::LibrarySetup;
use droppedneedle::providers::musicbrainz::Criticality;
use droppedneedle::runtime_config::{ConfigStore, Crypto};
use tokio::sync::{oneshot, watch};

/// Scratch-dir sequence so parallel tests never share a database.
static SCRATCH_SEQ: AtomicU64 = AtomicU64::new(0);

/// Empty recall, no side effects: every attempt lands NoCandidate.
struct EmptyRecall;

impl IdentifyProviders for EmptyRecall {
    fn recall_candidates(
        &self,
        _facts: &LocalAlbumFacts,
        _limit: u32,
    ) -> Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        Box::pin(async move {
            RecallOutcome {
                result: RecallResult::default(),
                recall_criticality: Some(Criticality::IdentityCritical),
            }
        })
    }
}

/// Signals shutdown once, on the first recall: a deterministic
/// SIGTERM landing mid-drain.
struct SignalOnFirstRecall {
    shutdown_tx: watch::Sender<bool>,
    signalled: AtomicBool,
}

impl SignalOnFirstRecall {
    fn new(shutdown_tx: watch::Sender<bool>) -> Self {
        Self {
            shutdown_tx,
            signalled: AtomicBool::new(false),
        }
    }
}

impl IdentifyProviders for SignalOnFirstRecall {
    fn recall_candidates(
        &self,
        _facts: &LocalAlbumFacts,
        _limit: u32,
    ) -> Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        if !self.signalled.swap(true, Ordering::SeqCst) {
            let _ = self.shutdown_tx.send(true);
        }
        Box::pin(async move {
            RecallOutcome {
                result: RecallResult::default(),
                recall_criticality: Some(Criticality::IdentityCritical),
            }
        })
    }
}

/// Blocks inside recall until dropped: a stuck gate wait. Rendezvous
/// on `entered` before signalling shutdown so the test never races.
struct GateWait {
    entered: std::sync::Mutex<Option<oneshot::Sender<()>>>,
}

impl GateWait {
    fn new(entered: oneshot::Sender<()>) -> Self {
        Self {
            entered: std::sync::Mutex::new(Some(entered)),
        }
    }
}

impl IdentifyProviders for GateWait {
    fn recall_candidates(
        &self,
        _facts: &LocalAlbumFacts,
        _limit: u32,
    ) -> Pin<Box<dyn Future<Output = RecallOutcome> + Send + '_>> {
        if let Ok(mut slot) = self.entered.lock()
            && let Some(entered) = slot.take()
        {
            let _ = entered.send(());
        }
        Box::pin(async move {
            std::future::pending::<()>().await;
            RecallOutcome::default()
        })
    }
}

/// Scratch library bundle over scripted providers.
struct Lib {
    /// Held, never read: dropping it would close the pool out from under
    /// the adapters.
    #[allow(dead_code)]
    runtime: DbRuntime,
    library: LibrarySetup,
}

impl Lib {
    async fn open(tag: &str, providers: Arc<dyn IdentifyProviders>) -> Self {
        let seq = SCRATCH_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "droppedneedle-lib-ident-shutdown-{tag}-{}-{seq}",
            std::process::id()
        ));
        let runtime = open_runtime(&DbConfig::new(&dir.join("app.db")))
            .await
            .expect("scratch runtime opens");
        let crypto = Arc::new(Crypto::from_key_bytes(&[7u8; 32]).expect("test key"));
        let store = Arc::new(
            ConfigStore::open(
                &dir.join("config.json"),
                Crypto::from_key_bytes(&[7u8; 32]).expect("test key"),
            )
            .expect("scratch config opens"),
        );
        let ids = Arc::new(UuidGenerator);
        let clock = Arc::new(SystemClock);
        let bundle = ProdAuth::new(
            runtime.pool(),
            runtime.lane(),
            Arc::clone(&crypto),
            Arc::clone(&ids) as Arc<dyn IdGenerator>,
            Arc::clone(&clock) as Arc<dyn droppedneedle::auth::users::stores::Clock>,
            &dir.join("avatars"),
        );
        let http = HttpClientFactory::new().expect("http factory builds");
        let users = AuthSetup::build(
            bundle,
            Arc::clone(&store),
            Arc::clone(&crypto),
            &http,
            Arc::clone(&ids) as Arc<dyn IdGenerator>,
            Arc::clone(&clock) as Arc<dyn droppedneedle::auth::users::stores::Clock>,
            "",
        )
        .expect("prod auth bundle builds")
        .users;
        let library =
            LibrarySetup::for_tests_with_providers(users, ids as Arc<dyn IdGenerator>, providers)
                .expect("library bundle builds");
        Self { runtime, library }
    }
}

/// Seed `count` due jobs with facts, so no disk reads slow the drain.
fn seed_jobs(library: &LibrarySetup, count: usize) {
    for n in 0..count {
        let album = format!("album-{n}");
        library.identities.save_album_facts(LocalAlbumFacts {
            local_album_id: album.clone(),
            title: format!("Album {n}"),
            ..LocalAlbumFacts::default()
        });
        library.identify.enqueue_album(
            &format!("job-{n}"),
            &album,
            IdentifyKind::Automatic,
            "rev-1",
            None,
            0,
        );
    }
}

fn job_state(library: &LibrarySetup, job_id: &str) -> JobState {
    library
        .identify_queue
        .job(job_id)
        .unwrap_or_else(|| panic!("{job_id} present"))
        .state
}

#[tokio::test]
async fn identify_tick_pre_signalled_shutdown_attempts_nothing() {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let lib = Lib::open("quiet", Arc::new(EmptyRecall)).await;
    seed_jobs(&lib.library, 3);
    shutdown_tx.send(true).expect("shutdown sends");
    let attempted = lib.library.identify_tick_with_shutdown(&shutdown_rx).await;
    assert_eq!(attempted, 0, "pre-signalled shutdown attempts nothing");
    for n in 0..3 {
        assert_eq!(
            job_state(&lib.library, &format!("job-{n}")),
            JobState::Queued
        );
    }
}

#[tokio::test]
async fn identify_tick_abandons_drain_after_mid_attempt_shutdown() {
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let lib = Lib::open("mid-drain", Arc::new(SignalOnFirstRecall::new(shutdown_tx))).await;
    seed_jobs(&lib.library, 5);
    let attempted = lib.library.identify_tick_with_shutdown(&shutdown_rx).await;
    assert_eq!(
        attempted, 1,
        "drain stops after the attempt running at shutdown"
    );
    assert_eq!(job_state(&lib.library, "job-0"), JobState::Succeeded);
    for n in 1..5 {
        let job = lib
            .library
            .identify_queue
            .job(&format!("job-{n}"))
            .unwrap_or_else(|| panic!("job-{n} present"));
        assert_eq!(job.state, JobState::Queued);
        assert_eq!(job.attempts, 0, "unclaimed jobs stay untouched");
    }
}

#[tokio::test]
async fn identify_tick_abandons_in_flight_gate_wait_on_shutdown() {
    let (entered_tx, entered_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let lib = Lib::open("gate-wait", Arc::new(GateWait::new(entered_tx))).await;
    seed_jobs(&lib.library, 3);
    let library = lib.library.clone();
    let tick = tokio::spawn(async move { library.identify_tick_with_shutdown(&shutdown_rx).await });
    entered_rx.await.expect("recall entered its gate wait");
    let start = std::time::Instant::now();
    shutdown_tx.send(true).expect("shutdown sends");
    let attempted = tokio::time::timeout(Duration::from_secs(5), tick)
        .await
        .expect("tick yields instead of waiting out the gate")
        .expect("tick joins");
    let elapsed = start.elapsed();
    println!("shutdown yield after gate-wait abandon: {elapsed:?}");
    assert_eq!(attempted, 0, "abandoned attempt is not counted");
    assert!(
        elapsed < Duration::from_secs(1),
        "yielded promptly, took {elapsed:?}"
    );
    // The claimed job keeps its Running row in memory; a restart
    // clears it, same as any mid-drain crash today.
    assert_eq!(job_state(&lib.library, "job-0"), JobState::Running);
    for n in 1..3 {
        assert_eq!(
            job_state(&lib.library, &format!("job-{n}")),
            JobState::Queued
        );
    }
}

#[tokio::test]
async fn identify_tick_without_signal_still_drains_everything() {
    let lib = Lib::open("full-drain", Arc::new(EmptyRecall)).await;
    seed_jobs(&lib.library, 3);
    let attempted = lib.library.identify_tick().await;
    assert_eq!(attempted, 3, "no signal keeps the full drain");
}
