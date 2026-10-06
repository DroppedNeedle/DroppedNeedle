//! Landing: from a finished download to the library.
//!
//! When the download worker sees a transfer finish, the landing takes the
//! files from there, the way Lidarr's completed-download handling and v2's
//! file processor did:
//!
//! 1. [`probe`] walks what landed and reads tags and stream headers.
//! 2. The file checks in [`specs`] judge the files alone: present, audio,
//!    not samples, not a different edition or album, quality inside the
//!    policy band, an upgrade that really is one.
//! 3. [`matching`] scores the files against the requested release group
//!    with the library's matching engine, the pinned edition first.
//! 4. The match checks decide whether the match is close enough
//!    ([`decision`]); the file plan says which files import, which the
//!    library already holds, which are held, and which the release does
//!    not account for.
//! 5. Verified files go to the library through the [`ports`] seam (the
//!    staged publisher does the writing); files that are not confident
//!    enough are held for a person ([`hold`]).
//!
//! Every landing records its decision and each check's verdict in
//! `download_import_decisions`. The worker turns the result into the task
//! status: completed or partial, failed with files held, a failover to
//! the next candidate, or a wait.

pub mod decision;
pub mod hold;
pub mod matching;
pub mod ports;
pub mod probe;
pub mod quality;
pub mod specs;

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use self::decision::{Decision, Outcome, check_files, decide};
use self::hold::HoldItem;
use self::matching::{MatchSummary, find};
use self::ports::{ImportFailure, ImportFile, ImportRequest, LandingLibrary};
use self::probe::{LandedFile, Landing};
use self::specs::{Check, Disposition, QualityPolicy, Rejection, Target};
use super::dispatch::Journal;
use super::downloads::landing_rows::{HeldFile, ImportDecisionRow};
use super::downloads::manifest::DownloadManifest;
use super::downloads::store::{TaskDetails, TaskRow};

/// The library port, set once the library bundle exists.
pub type LibrarySlot = Arc<OnceLock<Arc<dyn LandingLibrary>>>;

/// Settings a landing reads, resolved per landing.
#[derive(Debug, Clone)]
pub struct LandingSettings {
    pub quality_min: String,
    pub quality_max: String,
}

impl Default for LandingSettings {
    fn default() -> Self {
        let policy = QualityPolicy::default();
        Self {
            quality_min: policy.quality_min,
            quality_max: policy.quality_max,
        }
    }
}

/// Resolves the landing settings; called once per landing.
pub type SettingsProvider = Arc<dyn Fn() -> LandingSettings + Send + Sync>;

/// What became of one landing.
#[derive(Debug, Clone, PartialEq)]
pub enum LandingResult {
    /// Files are in the library. `complete` when every requested track is
    /// there now; otherwise the download landed short.
    Imported { complete: bool },
    /// Nothing imported; the files are held for a person.
    Held { code: String, detail: String },
    /// Not importable; the disposition says what the worker does next.
    Rejected(Rejection),
}

/// The landing outcome plus counts for the task row and the log.
#[derive(Debug, Clone, PartialEq)]
pub struct LandingReport {
    pub result: LandingResult,
    pub files_imported: usize,
    pub files_held: usize,
}

/// The landing service the worker calls.
pub struct LandingService {
    journal: Arc<Journal>,
    library: LibrarySlot,
    settings: SettingsProvider,
    held_dir: PathBuf,
}

impl LandingService {
    pub fn new(
        journal: Arc<Journal>,
        library: LibrarySlot,
        settings: SettingsProvider,
        held_dir: PathBuf,
    ) -> Self {
        Self {
            journal,
            library,
            settings,
            held_dir,
        }
    }

    /// The library slot, for boot to fill.
    pub fn library_slot(&self) -> &LibrarySlot {
        &self.library
    }

    /// Land one task's files: probe, check, match, then import, hold, or
    /// reject. Never fails: every problem becomes a result the worker can
    /// act on, and the decision is recorded.
    pub async fn land(
        &self,
        task: &TaskRow,
        attempt_id: Option<&str>,
        manifest: Option<&DownloadManifest>,
        paths: Vec<PathBuf>,
    ) -> LandingReport {
        let details = {
            let task_id = task.id.clone();
            self.journal
                .run("downloads.landing.details", move |store| {
                    store.task_details(&task_id)
                })
                .await
                .unwrap_or_else(|error| {
                    tracing::warn!(task_id = %task.id, %error, "task details unreadable");
                    TaskDetails::default()
                })
        };
        let target = target_for(task, &details, manifest);
        let landing = match tokio::task::spawn_blocking(move || probe::probe(&paths)).await {
            Ok(landing) => landing,
            Err(error) => {
                let rejection = local_fault(format!("probe failed: {error}"));
                return self
                    .finish(
                        task,
                        attempt_id,
                        &Decision {
                            checks: Vec::new(),
                            outcome: Outcome::Reject(rejection),
                            release_mbid: None,
                            distance: None,
                        },
                        &Landing::default(),
                        0,
                        0,
                    )
                    .await;
            }
        };
        let library = self.library.get().cloned();
        let settings = (self.settings)();
        let mut policy = QualityPolicy {
            quality_min: settings.quality_min,
            quality_max: settings.quality_max,
            held_tier: None,
        };
        if target.origin == "upgrade"
            && let Some(library) = &library
            && !target.release_group_mbid.is_empty()
        {
            policy.held_tier = library.held_tier(&target.release_group_mbid).await;
        }
        let (checks, early) = check_files(&target, &landing, &policy);
        let decision = match (early, &library) {
            (Some(outcome), _) => Decision {
                checks,
                outcome,
                release_mbid: None,
                distance: None,
            },
            (None, None) => Decision {
                checks,
                outcome: Outcome::Reject(local_fault(
                    "the library is not ready to take imports".to_owned(),
                )),
                release_mbid: None,
                distance: None,
            },
            (None, Some(library)) => {
                let matched = find(library.as_ref(), &target, &landing).await;
                let decision = decide(&target, &landing, &policy, &matched, checks);
                return self
                    .act(
                        task, attempt_id, &target, &landing, &matched, decision, library,
                    )
                    .await;
            }
        };
        let held = match &decision.outcome {
            Outcome::Hold { code, detail } => {
                self.hold_all(task, &target, &landing, code, detail).await
            }
            _ => 0,
        };
        self.finish(task, attempt_id, &decision, &landing, 0, held)
            .await
    }

    /// Act on a decision reached after matching.
    #[allow(clippy::too_many_arguments)]
    async fn act(
        &self,
        task: &TaskRow,
        attempt_id: Option<&str>,
        target: &Target,
        landing: &Landing,
        matched: &MatchSummary,
        mut decision: Decision,
        library: &Arc<dyn LandingLibrary>,
    ) -> LandingReport {
        let plan = match &decision.outcome {
            Outcome::Import(plan) => plan.clone(),
            Outcome::Hold { code, detail } => {
                let held = self.hold_all(task, target, landing, code, detail).await;
                return self
                    .finish(task, attempt_id, &decision, landing, 0, held)
                    .await;
            }
            Outcome::Reject(_) => {
                return self
                    .finish(task, attempt_id, &decision, landing, 0, 0)
                    .await;
            }
        };
        let Some(found) = matched.best() else {
            return self
                .finish(task, attempt_id, &decision, landing, 0, 0)
                .await;
        };
        let mut imported = 0;
        if !plan.import.is_empty() {
            let request = ImportRequest {
                task_id: task.id.clone(),
                release: found.release.clone(),
                files: plan
                    .import
                    .iter()
                    .map(|(file, track)| ImportFile {
                        path: landing.audio[*file].path.clone(),
                        track: *track,
                    })
                    .collect(),
            };
            match library.import(request).await {
                Ok(receipt) => {
                    imported = plan.import.len();
                    tracing::info!(
                        task_id = %task.id,
                        bundle = %receipt.bundle_id,
                        album = %receipt.album_id,
                        files = imported,
                        "download imported into the library"
                    );
                    let sources: Vec<PathBuf> = plan
                        .import
                        .iter()
                        .map(|(file, _)| landing.audio[*file].path.clone())
                        .collect();
                    if let Err(error) =
                        tokio::task::spawn_blocking(move || remove_sources(&sources)).await
                    {
                        tracing::warn!(%error, "imported source cleanup did not run");
                    }
                }
                Err(ImportFailure::LocalFault(detail)) => {
                    decision.outcome = Outcome::Reject(local_fault(detail));
                    return self
                        .finish(task, attempt_id, &decision, landing, 0, 0)
                        .await;
                }
                Err(ImportFailure::Occupied(detail)) => {
                    let items = plan
                        .import
                        .iter()
                        .map(|(file, track)| {
                            self.hold_item(
                                task,
                                target,
                                &landing.audio[*file],
                                Some((found, *track)),
                                "target_occupied",
                                &detail,
                            )
                        })
                        .collect();
                    let held = hold::hold(&self.journal, &self.held_dir, items, now()).await;
                    decision.outcome = Outcome::Hold {
                        code: "target_occupied",
                        detail,
                    };
                    return self
                        .finish(task, attempt_id, &decision, landing, 0, held)
                        .await;
                }
            }
        }
        let held = if plan.held.is_empty() {
            0
        } else {
            let items = plan
                .held
                .iter()
                .map(|(file, track)| {
                    self.hold_item(
                        task,
                        target,
                        &landing.audio[*file],
                        Some((found, *track)),
                        "tag_mismatch",
                        "the file's tags name a different track than the release has here",
                    )
                })
                .collect();
            hold::hold(&self.journal, &self.held_dir, items, now()).await
        };
        if imported == 0 && plan.owned.is_empty() {
            decision.outcome = Outcome::Hold {
                code: "tag_mismatch",
                detail: "no file could be imported".to_owned(),
            };
        }
        self.finish(task, attempt_id, &decision, landing, imported, held)
            .await
    }

    /// Hold every readable audio file of a landing.
    async fn hold_all(
        &self,
        task: &TaskRow,
        target: &Target,
        landing: &Landing,
        code: &str,
        detail: &str,
    ) -> usize {
        let items = landing
            .audio
            .iter()
            .map(|file| self.hold_item(task, target, file, None, code, detail))
            .collect();
        hold::hold(&self.journal, &self.held_dir, items, now()).await
    }

    /// The held row for one file, with the release track it was paired
    /// to when there is one.
    fn hold_item(
        &self,
        task: &TaskRow,
        target: &Target,
        file: &LandedFile,
        paired: Option<(&matching::FoundRelease, usize)>,
        code: &str,
        detail: &str,
    ) -> HoldItem {
        let track = paired.map(|(found, index)| (&found.release, &found.release.tracks[index]));
        let tag = &file.tag;
        HoldItem {
            source: file.path.clone(),
            row: HeldFile {
                user_id: task.user_id.clone(),
                held_path: String::new(),
                original_filename: file.file_name(),
                reason: code.to_owned(),
                reason_detail: Some(detail.to_owned()),
                source: task.source.clone(),
                source_task_id: task.id.clone(),
                origin: task.origin.clone(),
                release_group_mbid: non_empty(&target.release_group_mbid),
                release_mbid: track
                    .map(|(release, _)| release.id.clone())
                    .or_else(|| target.release_mbid.clone()),
                release_track_mbid: track.map(|(_, track)| track.id.clone()),
                recording_mbid: track
                    .map(|(_, track)| track.recording_id.clone())
                    .or_else(|| tag.musicbrainz_recording_id.clone()),
                track_number: Some(i64::from(
                    track.map_or(tag.track_number, |(_, track)| track.position),
                ))
                .filter(|number| *number > 0),
                disc_number: Some(i64::from(
                    track.map_or(tag.disc_number.max(1), |(_, track)| track.disc.max(1)),
                )),
                track_title: track
                    .map(|(_, track)| track.title.clone())
                    .or_else(|| non_empty(&tag.title)),
                artist_name: non_empty(&target.artist_name),
                artist_mbid: None,
                album_title: non_empty(&target.album_title),
                year: target.year.map(i64::from),
                file_format: Some(file.format.as_str().to_owned()),
                duration_seconds: file.header.duration_seconds,
                expected_duration_seconds: track
                    .and_then(|(_, track)| track.length_ms)
                    .map(|ms| ms as f64 / 1000.0),
                evidence_title: non_empty(&tag.title),
                evidence_artist: non_empty(&tag.artist),
                evidence_score: None,
            },
        }
    }

    /// Record the decision and report the result.
    async fn finish(
        &self,
        task: &TaskRow,
        attempt_id: Option<&str>,
        decision: &Decision,
        landing: &Landing,
        imported: usize,
        held: usize,
    ) -> LandingReport {
        let result = match &decision.outcome {
            Outcome::Import(plan) => LandingResult::Imported {
                complete: plan.complete,
            },
            Outcome::Hold { code, detail } => LandingResult::Held {
                code: (*code).to_owned(),
                detail: detail.clone(),
            },
            Outcome::Reject(rejection) => LandingResult::Rejected(rejection.clone()),
        };
        let (outcome, reason_code, detail) = match &result {
            LandingResult::Imported { complete: true } => ("imported", None, None),
            LandingResult::Imported { complete: false } => ("partial", None, None),
            LandingResult::Held { code, detail } => {
                ("held", Some(code.clone()), Some(detail.clone()))
            }
            LandingResult::Rejected(rejection) => (
                if rejection.disposition == Disposition::Temporary {
                    "deferred"
                } else {
                    "rejected"
                },
                Some(rejection.code.to_owned()),
                Some(rejection.detail.clone()),
            ),
        };
        let row = ImportDecisionRow {
            task_id: task.id.clone(),
            attempt_id: attempt_id.map(str::to_owned),
            outcome: outcome.to_owned(),
            reason_code,
            detail,
            release_mbid: decision.release_mbid.clone(),
            distance: decision.distance,
            files_total: landing.audio.len() as i64,
            files_imported: imported as i64,
            files_held: held as i64,
            checks_json: checks_json(&decision.checks),
            decided_at: now(),
        };
        if let Err(error) = self
            .journal
            .run("downloads.landing.decision", move |store| {
                store.record_import_decision(&row)
            })
            .await
        {
            tracing::warn!(task_id = %task.id, %error, "import decision not recorded");
        }
        tracing::info!(
            task_id = %task.id,
            outcome,
            imported,
            held,
            "download landing decided"
        );
        LandingReport {
            result,
            files_imported: imported,
            files_held: held,
        }
    }
}

/// What the task asked for, from its row, details, and manifest.
pub fn target_for(
    task: &TaskRow,
    details: &TaskDetails,
    manifest: Option<&DownloadManifest>,
) -> Target {
    Target {
        artist_name: task.artist_name.clone(),
        album_title: task.album_title.clone(),
        release_group_mbid: task.release_group_mbid.trim().to_ascii_lowercase(),
        release_mbid: details
            .release_mbid
            .clone()
            .or_else(|| manifest.and_then(|manifest| manifest.release_mbid.clone()))
            .map(|mbid| mbid.trim().to_ascii_lowercase())
            .filter(|mbid| !mbid.is_empty()),
        year: details.year,
        is_track: task.download_type == "track",
        recording_mbid: task
            .recording_mbid
            .as_deref()
            .map(str::trim)
            .filter(|mbid| !mbid.is_empty())
            .map(str::to_ascii_lowercase),
        track_title: details.track_title.clone(),
        origin: task.origin.clone(),
        hold_on_wrong_track: manifest.is_some_and(|manifest| manifest.hold_on_wrong_track),
    }
}

fn local_fault(detail: String) -> Rejection {
    Rejection {
        code: "local_fault",
        disposition: Disposition::LocalFault,
        quarantine: None,
        detail,
    }
}

fn checks_json(checks: &[Check]) -> String {
    serde_json::to_string(checks).unwrap_or_else(|error| {
        tracing::warn!(%error, "import checks not serialized");
        "[]".to_owned()
    })
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// Remove imported source files (the library holds them now), then the
/// folder they leave empty. Best effort: a leftover is the orphan sweep's.
fn remove_sources(paths: &[PathBuf]) {
    let mut parents: Vec<&Path> = Vec::new();
    for path in paths {
        if let Err(error) = std::fs::remove_file(path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::info!(file = %path.display(), %error, "imported source file kept");
        }
        if let Some(parent) = path.parent()
            && !parents.contains(&parent)
        {
            parents.push(parent);
        }
    }
    for parent in parents {
        // Fails (and keeps the folder) unless it is empty now.
        let _ = std::fs::remove_dir(parent);
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}
