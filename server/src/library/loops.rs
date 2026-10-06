//! Library background loops and the ticks they drive.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use super::clock::{now_ms, now_unix, today_day};
use super::identify::models::{IdentifyJob, LocalAlbumFacts, LocalTrackFacts};
use super::identify::stores::QueueStore;
use super::publish::PublishError;
use super::publish::snapshots::SnapshotStore;
use super::scan::coordinator::ResolverSource as _;
use super::scan::store::CatalogStore;
use super::scan::supervisor::SupervisorInputs;
use super::scan::supervisor::{startup_recovery, supervise_once, supervise_once_with_shutdown};
use super::scan::watcher::{
    WatcherAction, WatcherSettings, WatcherState, clear_pending, watcher_request,
};
use super::wiring::LibrarySetup;

/// Scan supervisor idle ceiling. The supervisor's 47s recovery ceiling
/// cannot wait on shutdown, so the wired loop re-checks the watch
/// every 5s; operator-visible behavior is identical.
const SUPERVISOR_IDLE_CEILING: Duration = Duration::from_secs(5);

/// Identify queue poll cadence.
const IDENTIFY_POLL: Duration = Duration::from_secs(2);

/// Publish maintenance cadence (snapshot purge plus preview sweep).
const PUBLISH_MAINTENANCE: Duration = Duration::from_secs(3600);

impl LibrarySetup {
    /// Supervisor inputs, rebuilt per call so settings-swap rebuilds
    /// never strand the loop on stale getters.
    pub(crate) fn supervisor_inputs(&self) -> SupervisorInputs {
        SupervisorInputs {
            root_paths: self.root_dirs.clone(),
            schedule: {
                let config = self.config.clone();
                Arc::new(move || super::settings::schedule(&config))
            },
            inclusion_rules: {
                let registry = self.registry.clone();
                Arc::new(move || super::settings::inclusion_rules(registry.resolver().registry()))
            },
            dirty: self.dirty.clone(),
            wakeups: self.wakeups.clone(),
            now_unix: Arc::new(now_unix),
        }
    }

    /// Watcher settings, re-read from the config store every tick.
    pub(crate) fn watcher_settings(&self) -> WatcherSettings {
        super::settings::watcher(&self.config)
    }

    /// One-shot scan startup reconciliation (Hook A). The loop runs
    /// this in its preamble; tests drive it directly.
    pub async fn scan_startup_recovery(&self) {
        self.refresh_registry();
        startup_recovery(&self.coordinator, &self.supervisor_inputs()).await;
    }

    /// One supervisor iteration. Returns true when a run was driven.
    pub async fn supervisor_tick(&self) -> bool {
        self.refresh_registry();
        supervise_once(&self.coordinator, &self.supervisor_inputs()).await
    }

    /// One shutdown-aware supervisor iteration: same Hook B, schedule,
    /// and worker semantics as [`supervisor_tick`](Self::supervisor_tick),
    /// but a signalled shutdown stops the in-flight scan instead of
    /// waiting it out. The stop goes through the regular control latch,
    /// so the walk and index checkpoints settle the run to cancelled
    /// on their next check and the next start resumes cleanly. A
    /// pre-signalled shutdown claims no new work.
    pub async fn supervisor_tick_with_shutdown(&self, shutdown: &watch::Receiver<bool>) -> bool {
        self.refresh_registry();
        supervise_once_with_shutdown(&self.coordinator, &self.supervisor_inputs(), shutdown).await
    }

    /// One watcher tick over the persistent watcher state.
    pub async fn watcher_tick(&self) -> WatcherAction {
        // The state swaps out and back so no mutex guard crosses the
        // snapshot await.
        let mut state = {
            let mut guard = self
                .watcher_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            std::mem::replace(&mut *guard, WatcherState::new())
        };
        let settings = self.watcher_settings();
        let registry = self.live_registry();
        let action = super::scan::watcher::poll_once(
            &mut state,
            &settings,
            &registry,
            &(self.root_dirs)(),
            &self.pool,
            now_unix(),
        )
        .await;
        if matches!(action, WatcherAction::Due)
            && let Some(request) = watcher_request(&registry, &[])
        {
            match self.coordinator.request_run(&request) {
                Ok(result) => {
                    tracing::info!(
                        disposition = ?result.disposition,
                        "filesystem watcher requested incremental scan"
                    );
                    clear_pending(&mut state);
                    self.wakeups.notify("scan");
                }
                Err(error) => {
                    tracing::warn!(%error, "watcher scan request failed");
                    clear_pending(&mut state);
                }
            }
        }
        {
            let mut guard = self
                .watcher_state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *guard = state;
        }
        action
    }

    /// One identify tick: claim every due job, fill missing facts
    /// from the scan catalog plus disk tag reads, and run each
    /// attempt. Returns jobs attempted.
    pub async fn identify_tick(&self) -> usize {
        // No signal: the sender stays alive so the watch never fires
        // and the drain runs exactly as before.
        let (_live, quiet) = watch::channel(false);
        self.identify_tick_with_shutdown(&quiet).await
    }

    /// Shutdown-aware drain: same claim order and per-attempt
    /// semantics as [`identify_tick`](Self::identify_tick), but a
    /// signalled shutdown abandons the drain instead of pacing out
    /// the whole queue at one MusicBrainz gate slot per attempt. The
    /// signal is checked before each claim, before each gated
    /// attempt, and across the attempt itself, so SIGTERM mid-drain
    /// yields promptly. Claimed-but-unfinished jobs stay Running in
    /// memory; a restart clears them, same as any mid-drain crash
    /// today.
    pub async fn identify_tick_with_shutdown(&self, shutdown: &watch::Receiver<bool>) -> usize {
        let mut attempted = 0;
        let mut shutdown = shutdown.clone();
        loop {
            if *shutdown.borrow() {
                break;
            }
            let claimed = self
                .identify_queue
                .claim(now_ms(), super::identify::queue::LEASE_SECONDS * 1000);
            let Some(job) = claimed else { break };
            self.fill_facts(&job).await;
            if *shutdown.borrow() {
                break;
            }
            let attempt = self.identify.run_claimed_job(&job.id, now_ms());
            tokio::select! {
                biased;
                _ = shutdown.changed() => break,
                report = attempt => match report {
                    Some(report) => {
                        attempted += 1;
                        tracing::info!(
                            job_id = report.job.id,
                            outcome = ?report.outcome,
                            reason = report.reason_code,
                            "identify attempt finished"
                        );
                    }
                    None => {
                        tracing::warn!(job_id = job.id, "identify job vanished mid-claim");
                    }
                },
            }
        }
        attempted
    }

    /// One publish maintenance tick: purge expired operation
    /// snapshots plus expired preview seals. Returns snapshots purged.
    pub fn publish_tick(&self) -> Result<usize, PublishError> {
        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let registry = self.live_registry();
        // Reconcile-on-reopen can resume renames; a no-op refresh
        // takes no guards and bumps no revisions.
        let _guards = if cell.needs_refresh(&registry) {
            self.publish_guards(&registry)
        } else {
            Vec::new()
        };
        cell.refresh(&registry, &self.root_dirs)?;
        let purged = match cell.cell.as_mut() {
            Some(open) => {
                SnapshotStore::new(open.publisher.connection()).purge_expired(today_day())?
            }
            None => 0,
        };
        drop(cell);
        self.sweep_previews();
        Ok(purged)
    }

    /// Fill missing album facts from the scan catalog plus live disk
    /// tag reads. Scan keys read `root::directory`; anything else
    /// keeps its seeded facts (HTTP manual enqueue always seeds).
    /// Albums with no surviving files seed empty facts so the job
    /// reaches a terminal outcome instead of sticking.
    ///
    /// Tag reads and probes ride the blocking pool: a full decode on
    /// a slow disk must never stall the async runtime.
    async fn fill_facts(&self, job: &IdentifyJob) {
        use super::identify::stores::IdentityStore;

        if self.identities.album_facts(&job.local_album_id).is_some() {
            return;
        }
        let Some((root_id, parent)) = job.local_album_id.split_once("::") else {
            return;
        };
        let dirs = (self.root_dirs)();
        let Some(root_dir) = dirs.get(root_id) else {
            self.identities.save_album_facts(LocalAlbumFacts {
                local_album_id: job.local_album_id.clone(),
                ..LocalAlbumFacts::default()
            });
            return;
        };
        let prefix = if parent == "." {
            String::new()
        } else {
            format!("{parent}/")
        };
        // Memory-only fan-out first; every disk read below rides the pool.
        let wanted: Vec<(String, String)> = self
            .scan_store
            .catalog_entries(root_id)
            .into_iter()
            .filter(|(relative_path, _)| relative_path.starts_with(&prefix))
            .map(|(relative_path, entry)| (relative_path, entry.track_id))
            .collect();
        let mut tracks = Vec::new();
        for (relative_path, track_id) in wanted {
            let file = root_dir.join(&relative_path);
            let pool = self.pool.clone();
            let facts = pool.run(move || read_track_facts(&file)).await;
            tracks.push(LocalTrackFacts {
                local_track_id: track_id,
                title: facts.title,
                artist_name: facts.artist,
                track_number: facts.track_number,
                disc_number: facts.disc_number,
                duration_secs: facts.duration_secs,
                recording_mbid: facts.recording,
                release_track_mbid: facts.release_track,
                release_mbid: facts.release,
                release_group_mbid: facts.group,
                fingerprint: None,
            });
        }
        // Album title and artist from the first tagged track; empty
        // when nothing survived, which still terminates.
        let first_tagged = tracks
            .iter()
            .find(|track| !track.title.is_empty())
            .map(|track| (track.local_track_id.clone(), track.artist_name.clone()));
        let (title, artist) = match first_tagged {
            Some((track_id, artist)) => {
                let file =
                    root_dir.join(self.track_relative(root_id, &track_id).unwrap_or_default());
                let pool = self.pool.clone();
                let album = pool
                    .run(move || {
                        super::tags::format_for_path(&file)
                            .ok()
                            .and_then(|format| super::tags::read::read_tag_only(&file, format).ok())
                            .map(|tag| tag.album)
                            .unwrap_or_default()
                    })
                    .await;
                (album, artist)
            }
            None => (String::new(), String::new()),
        };
        self.identities.save_album_facts(LocalAlbumFacts {
            local_album_id: job.local_album_id.clone(),
            title,
            album_artist_name: artist,
            tracks,
            locked_track_ids: Vec::new(),
            is_compilation: false,
        });
    }

    /// Relative path for one catalog track id, if still present.
    fn track_relative(&self, root_id: &str, track_id: &str) -> Option<String> {
        self.scan_store
            .catalog_entries(root_id)
            .into_iter()
            .find(|(_, entry)| entry.track_id == track_id)
            .map(|(relative_path, _)| relative_path)
    }
}

/// Disk facts for one catalog file: tag-only read plus probe.
/// Blocking (a probe fully decodes); always runs on the pool.
struct DiskTrackFacts {
    title: String,
    artist: String,
    track_number: u32,
    disc_number: u32,
    duration_secs: Option<u64>,
    recording: Option<String>,
    release_track: Option<String>,
    release: Option<String>,
    group: Option<String>,
}

fn read_track_facts(file: &std::path::Path) -> DiskTrackFacts {
    let (title, artist, track_number, disc_number, recording, release_track, release, group) =
        match super::tags::format_for_path(file)
            .ok()
            .and_then(|format| super::tags::read::read_tag_only(file, format).ok())
        {
            Some(tag) => (
                tag.title,
                tag.artist,
                tag.track_number,
                tag.disc_number,
                tag.musicbrainz_recording_id,
                tag.musicbrainz_release_track_id,
                tag.musicbrainz_release_id,
                tag.musicbrainz_release_group_id,
            ),
            None => (String::new(), String::new(), 0, 0, None, None, None, None),
        };
    let duration_secs = super::tags::probe(file)
        .ok()
        .map(|info| info.duration_seconds as u64);
    DiskTrackFacts {
        title,
        artist,
        track_number,
        disc_number,
        duration_secs,
        recording,
        release_track,
        release,
        group,
    }
}

// ---------------------------------------------------------------------------
// Background loops. Thin shutdown-aware shells over the tick methods.
// ---------------------------------------------------------------------------

/// Scan supervisor loop: Hook A preamble, then drive-until-idle with
/// a shutdown-checked ceiling. The tick itself is shutdown-aware, so a
/// SIGTERM landing mid-scan stops the run instead of waiting it out.
pub(crate) async fn scan_loop(setup: LibrarySetup, mut shutdown: watch::Receiver<bool>) {
    setup.scan_startup_recovery().await;
    loop {
        if *shutdown.borrow() {
            break;
        }
        let revision = setup.wakeups.revision("scan");
        if setup.supervisor_tick_with_shutdown(&shutdown).await {
            continue;
        }
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = setup.wakeups.wait("scan", revision, SUPERVISOR_IDLE_CEILING) => {}
        }
    }
}

/// Filesystem watcher loop: snapshot, batch, request on due.
pub(crate) async fn watcher_loop(setup: LibrarySetup, mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        let action = setup.watcher_tick().await;
        let sleep_secs = match action {
            WatcherAction::Idle { sleep_secs } | WatcherAction::Batching { sleep_secs } => {
                sleep_secs.max(0.0)
            }
            WatcherAction::Due => setup.watcher_settings().poll_interval_seconds.max(1.0),
        };
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(Duration::from_secs_f64(sleep_secs)) => {}
        }
    }
}

/// Identify queue loop: drain every due job each tick.
pub(crate) async fn identify_loop(setup: LibrarySetup, mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            break;
        }
        setup.identify_tick_with_shutdown(&shutdown).await;
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(IDENTIFY_POLL) => {}
        }
    }
}

/// Publish maintenance loop: snapshot purge plus preview sweep.
pub(crate) async fn publish_loop(setup: LibrarySetup, mut shutdown: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = shutdown.changed() => break,
            _ = tokio::time::sleep(PUBLISH_MAINTENANCE) => {}
        }
        if *shutdown.borrow() {
            break;
        }
        // The tick is sync blocking work (mutexes, spin-guards, sqlite):
        // keep it off the async runtime.
        let tick_setup = setup.clone();
        match tokio::task::spawn_blocking(move || tick_setup.publish_tick()).await {
            Ok(Ok(_)) => {}
            Ok(Err(error)) => tracing::warn!(%error, "publish maintenance tick failed"),
            Err(error) => tracing::warn!(%error, "publish maintenance tick panicked"),
        }
    }
}
