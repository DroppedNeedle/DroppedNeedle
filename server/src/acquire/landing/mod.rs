//! Landing: from a finished download to the library.
//!
//! When the download worker sees a transfer finish, the landing takes the
//! files from there, the way Lidarr's completed-download handling and v2's
//! file processor did:
//!
//! 1. [`probe`] walks what landed and reads tags and stream headers.
//! 2. The file checks in [`specs`] judge the files alone: present and
//!    complete on disk, audio, not samples, not a different edition or
//!    album, quality inside the policy band, an upgrade that really is one.
//! 3. [`matching`] scores the files against the requested release group
//!    with the library's matching engine, the pinned edition first.
//! 4. The match checks decide whether the match is close enough
//!    ([`decision`]); the file plan says which files import, which the
//!    library already holds, which are held, and which the release does
//!    not account for. When an AcoustID key is set, files about to import
//!    are fingerprinted and one whose audio is clearly another recording
//!    is held on its own.
//! 5. Verified files go to the library through the [`ports`] seam (the
//!    staged publisher does the writing); files that are not confident
//!    enough are held for a person ([`hold`]).
//!
//! Every landing records its decision, each check's verdict, and the
//! release tracks still missing in `download_import_decisions`. The worker
//! turns the result into the task status: completed, a failover to the
//! next candidate for what is still missing, partial or held once the
//! candidates run out, or a wait.

pub mod decision;
pub mod hold;
pub mod library;
pub mod matching;
pub mod ports;
pub mod probe;
pub mod quality;
pub mod specs;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use self::decision::{Decision, Outcome, check_files, decide};
use self::hold::HoldItem;
use self::matching::{FilePlan, FoundRelease, MatchSummary, find};
use self::ports::{ImportFailure, ImportFile, ImportRequest, LandingLibrary};
use self::probe::{LandedFile, Landing};
use self::specs::{Check, Disposition, QualityPolicy, Rejection, Target, Verdict};
use super::dispatch::Journal;
use super::downloads::landing_rows::{HeldFile, ImportDecisionRow};
use super::downloads::manifest::DownloadManifest;
use super::downloads::store::{TaskDetails, TaskRow};

/// The library port, set once the library bundle exists.
pub type LibrarySlot = Arc<OnceLock<Arc<dyn LandingLibrary>>>;

/// Hold code for better files of an album the library already holds:
/// kept until the upgrade replaces the old files.
pub const UPGRADE_PENDING: &str = "upgrade_pending";

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

/// What one landing did, before it is recorded.
struct Acted {
    decision: Decision,
    imported: usize,
    held: usize,
    missing: Vec<(u32, u32)>,
    /// Imported source files, removed once the decision is recorded.
    imported_sources: Vec<PathBuf>,
}

impl Acted {
    fn decided(decision: Decision, held: usize) -> Self {
        Self {
            decision,
            imported: 0,
            held,
            missing: Vec::new(),
            imported_sources: Vec::new(),
        }
    }
}

/// One file that is held, with what it was paired to.
struct Held<'a> {
    file: &'a LandedFile,
    track: Option<usize>,
    code: &'static str,
    detail: String,
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
    /// act on, and the decision is recorded. `patient` lets missing files
    /// wait for another pass (a transfer that just finished may not be
    /// visible on the mount yet).
    pub async fn land(
        &self,
        task: &TaskRow,
        attempt_id: Option<&str>,
        manifest: Option<&DownloadManifest>,
        paths: Vec<PathBuf>,
        patient: bool,
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
        let mut target = target_for(task, &details, manifest);
        target.wait_for_files = patient;
        let library = self.library.get().cloned();
        let library_dirs = library
            .as_ref()
            .map(|library| library.library_dirs())
            .unwrap_or_default();
        let probed =
            tokio::task::spawn_blocking(move || probe::probe(&confine(paths, &library_dirs))).await;
        let landing = match probed {
            Ok(landing) => landing,
            Err(error) => {
                let decision = Decision {
                    checks: Vec::new(),
                    outcome: Outcome::Reject(local_fault(format!("probe failed: {error}"))),
                    release_mbid: None,
                    distance: None,
                };
                return self
                    .finish(
                        task,
                        attempt_id,
                        Acted::decided(decision, 0),
                        &Landing::default(),
                    )
                    .await;
            }
        };
        if landing.nothing_found()
            && let Some(report) = self.already_landed(task, attempt_id).await
        {
            return report;
        }
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
        let acted = match (early, &library) {
            (Some(outcome), _) => {
                let decision = Decision {
                    checks,
                    outcome,
                    release_mbid: None,
                    distance: None,
                };
                let held = self.hold_decided(task, &target, &landing, &decision).await;
                Acted::decided(decision, held)
            }
            (None, None) => Acted::decided(
                Decision {
                    checks,
                    outcome: Outcome::Reject(local_fault(
                        "the library is not ready to take imports".to_owned(),
                    )),
                    release_mbid: None,
                    distance: None,
                },
                0,
            ),
            (None, Some(library)) => {
                let matched = find(library.as_ref(), &target, &landing).await;
                let decision = decide(&target, &landing, &policy, &matched, checks);
                match &decision.outcome {
                    Outcome::Import(plan) => {
                        let plan = plan.clone();
                        self.act(task, &target, &landing, &matched, decision, plan, library)
                            .await
                    }
                    _ => {
                        let held = self.hold_decided(task, &target, &landing, &decision).await;
                        Acted::decided(decision, held)
                    }
                }
            }
        };
        self.finish(task, attempt_id, acted, &landing).await
    }

    /// Hold every audio file of a decision that holds.
    async fn hold_decided(
        &self,
        task: &TaskRow,
        target: &Target,
        landing: &Landing,
        decision: &Decision,
    ) -> usize {
        let Outcome::Hold { code, detail } = &decision.outcome else {
            return 0;
        };
        let held: Vec<Held<'_>> = landing
            .audio
            .iter()
            .map(|file| Held {
                file,
                track: None,
                code,
                detail: detail.clone(),
            })
            .collect();
        self.hold(task, target, landing, None, held).await
    }

    /// A landing that finds no files because this attempt already imported
    /// them (the process stopped between recording the import and settling
    /// the task): answer the recorded result again instead of failing.
    async fn already_landed(
        &self,
        task: &TaskRow,
        attempt_id: Option<&str>,
    ) -> Option<LandingReport> {
        let task_id = task.id.clone();
        let last = self
            .journal
            .run("downloads.landing.last", move |store| {
                store.latest_import_decision(&task_id)
            })
            .await
            .ok()
            .flatten()?;
        if last.attempt_id.as_deref() != attempt_id {
            return None;
        }
        let complete = match last.outcome.as_str() {
            "imported" => true,
            "partial" => false,
            _ => return None,
        };
        tracing::info!(task_id = %task.id, "landing already imported this attempt");
        Some(LandingReport {
            result: LandingResult::Imported { complete },
            files_imported: usize::try_from(last.files_imported).unwrap_or(0),
            files_held: usize::try_from(last.files_held).unwrap_or(0),
        })
    }

    /// Carry out an import plan: fingerprint, publish, hold what must be
    /// held, and work out what is still missing.
    #[allow(clippy::too_many_arguments)]
    async fn act(
        &self,
        task: &TaskRow,
        target: &Target,
        landing: &Landing,
        matched: &MatchSummary,
        mut decision: Decision,
        mut plan: FilePlan,
        library: &Arc<dyn LandingLibrary>,
    ) -> Acted {
        let Some(found) = matched.best() else {
            return Acted::decided(decision, 0);
        };
        let mut held: Vec<Held<'_>> = Vec::new();
        for (file, track) in &plan.held {
            held.push(Held {
                file: &landing.audio[*file],
                track: Some(*track),
                code: "tag_mismatch",
                detail: "the file's tags name a different track than the release has here"
                    .to_owned(),
            });
        }
        // An upgrade brings better files for tracks the library holds: they
        // wait for the replacement step instead of counting as landed.
        let upgrade = target.origin == "upgrade";
        if upgrade {
            for file in &plan.owned {
                let track = found.scored.pair_for(*file).map(|pair| pair.track);
                held.push(Held {
                    file: &landing.audio[*file],
                    track,
                    code: UPGRADE_PENDING,
                    detail: "better files for tracks the library holds; kept for the upgrade"
                        .to_owned(),
                });
            }
        }
        let heard_check = self
            .fingerprint_check(landing, found, &mut plan, &mut held, library)
            .await;
        decision.checks.extend(heard_check);

        let mut imported_tracks: Vec<usize> = Vec::new();
        let mut imported_sources = Vec::new();
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
                    let skipped: HashMap<usize, String> = receipt.skipped.into_iter().collect();
                    for (index, (file, track)) in plan.import.iter().enumerate() {
                        match skipped.get(&index) {
                            Some(detail) => held.push(Held {
                                file: &landing.audio[*file],
                                track: Some(*track),
                                code: "target_occupied",
                                detail: detail.clone(),
                            }),
                            None => {
                                imported_tracks.push(*track);
                                imported_sources.push(landing.audio[*file].path.clone());
                            }
                        }
                    }
                    tracing::info!(
                        task_id = %task.id,
                        bundle = %receipt.bundle_id,
                        album = %receipt.album_id,
                        files = imported_tracks.len(),
                        "download imported into the library"
                    );
                }
                Err(ImportFailure::LocalFault(detail)) => {
                    decision.outcome = Outcome::Reject(local_fault(detail));
                    return Acted::decided(decision, 0);
                }
                Err(ImportFailure::Occupied(detail)) => {
                    for (file, track) in &plan.import {
                        held.push(Held {
                            file: &landing.audio[*file],
                            track: Some(*track),
                            code: "target_occupied",
                            detail: detail.clone(),
                        });
                    }
                }
            }
        }
        let first_hold = held.first().map(|item| (item.code, item.detail.clone()));
        self.record_wrong_product(task, target, &held, imported_tracks.len())
            .await;
        let held_count = self.hold(task, target, landing, Some(found), held).await;

        // What the release still lacks after this landing: the next
        // candidate is asked for these positions only.
        let covered: HashSet<usize> = imported_tracks.iter().copied().collect();
        let missing: Vec<(u32, u32)> = if target.is_track {
            Vec::new()
        } else {
            found
                .release
                .tracks
                .iter()
                .enumerate()
                .filter(|(index, track)| !covered.contains(index) && !matched.owns(track))
                .map(|(_, track)| (track.disc.max(1), track.position))
                .collect()
        };
        let requested_landed = if target.is_track {
            found.requested_pair(target).is_some_and(|pair| {
                covered.contains(&pair.track)
                    || (!upgrade && matched.owns(&found.release.tracks[pair.track]))
            })
        } else {
            missing.is_empty()
        };
        let anything_landed = !imported_tracks.is_empty() || (!upgrade && !plan.owned.is_empty());
        decision.outcome = if anything_landed {
            Outcome::Import(FilePlan {
                complete: requested_landed,
                ..plan
            })
        } else if let Some((code, detail)) = first_hold {
            Outcome::Hold { code, detail }
        } else {
            Outcome::Hold {
                code: "no_tracks",
                detail: "no file could be imported".to_owned(),
            }
        };
        Acted {
            decision,
            imported: imported_tracks.len(),
            held: held_count,
            missing,
            imported_sources,
        }
    }

    /// AcoustID check on the files about to import (v2
    /// `_fingerprint_disagrees`): a file whose audio AcoustID confidently
    /// names other recordings, and whose length does not vouch for the
    /// track, is held on its own. Off without an AcoustID key.
    async fn fingerprint_check<'a>(
        &self,
        landing: &'a Landing,
        found: &FoundRelease,
        plan: &mut FilePlan,
        held: &mut Vec<Held<'a>>,
        library: &Arc<dyn LandingLibrary>,
    ) -> Option<Check> {
        if plan.import.is_empty() {
            return None;
        }
        let files: Vec<(String, PathBuf)> = plan
            .import
            .iter()
            .map(|(file, _)| (file.to_string(), landing.audio[*file].path.clone()))
            .collect();
        let heard = library.fingerprints(files).await;
        if heard.is_empty() {
            return None;
        }
        let mut kept = Vec::with_capacity(plan.import.len());
        let mut disagreed = 0;
        for (file, track) in plan.import.drain(..) {
            let release_track = &found.release.tracks[track];
            let disagrees = specs::fingerprint_disagrees(
                heard
                    .get(&file.to_string())
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
                &release_track.recording_id,
                landing.audio[file].header.duration_seconds,
                release_track.length_ms.map(|ms| ms as f64 / 1000.0),
            );
            if disagrees {
                disagreed += 1;
                held.push(Held {
                    file: &landing.audio[file],
                    track: Some(track),
                    code: "fingerprint_mismatch",
                    detail: "AcoustID identified a different recording".to_owned(),
                });
            } else {
                kept.push((file, track));
            }
        }
        plan.import = kept;
        Some(Check {
            spec: "fingerprint_agrees",
            verdict: if disagreed == 0 {
                Verdict::Accept { note: None }
            } else {
                Verdict::Accept {
                    note: Some(format!(
                        "{disagreed} file(s) held: AcoustID heard another recording"
                    )),
                }
            },
        })
    }

    /// v2's wrong-product verdict: an album whose files all failed on
    /// their own tags is a different product than asked for. Recorded on
    /// the task once, for the review card.
    async fn record_wrong_product(
        &self,
        task: &TaskRow,
        target: &Target,
        held: &[Held<'_>],
        imported: usize,
    ) {
        if target.is_track
            || imported > 0
            || held.len() < 2
            || held.iter().any(|item| item.code != "tag_mismatch")
        {
            return;
        }
        let folder = held
            .first()
            .and_then(|item| item.file.path.parent())
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let task_id = task.id.clone();
        if let Err(error) = self
            .journal
            .run("downloads.wrong_product", move |store| {
                store.record_wrong_product_verdict(&task_id, &folder, now())
            })
            .await
        {
            tracing::warn!(task_id = %task.id, %error, "wrong-product verdict not recorded");
        }
    }

    /// Hold files, each with the release track it was paired to.
    async fn hold(
        &self,
        task: &TaskRow,
        target: &Target,
        landing: &Landing,
        found: Option<&FoundRelease>,
        held: Vec<Held<'_>>,
    ) -> usize {
        if held.is_empty() {
            return 0;
        }
        let root = common_parent(&landing.audio);
        let items = held
            .iter()
            .map(|item| {
                let paired = found.zip(item.track);
                self.hold_item(
                    task,
                    target,
                    item.file,
                    paired,
                    &root,
                    item.code,
                    &item.detail,
                )
            })
            .collect();
        hold::hold(&self.journal, &self.held_dir, items, now()).await
    }

    /// The held row for one file, with the release track it was paired
    /// to when there is one. The file is named by its path inside the
    /// download, so two discs' `01.flac` stay apart.
    #[allow(clippy::too_many_arguments)]
    fn hold_item(
        &self,
        task: &TaskRow,
        target: &Target,
        file: &LandedFile,
        paired: Option<(&FoundRelease, usize)>,
        root: &Path,
        code: &str,
        detail: &str,
    ) -> HoldItem {
        let track = paired.map(|(found, index)| (&found.release, &found.release.tracks[index]));
        let tag = &file.tag;
        let relative = file
            .path
            .strip_prefix(root)
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| file.file_name());
        HoldItem {
            source: file.path.clone(),
            row: HeldFile {
                user_id: task.user_id.clone(),
                held_path: String::new(),
                original_filename: relative,
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

    /// Record the decision, then remove imported sources, and report.
    async fn finish(
        &self,
        task: &TaskRow,
        attempt_id: Option<&str>,
        acted: Acted,
        landing: &Landing,
    ) -> LandingReport {
        let Acted {
            decision,
            imported,
            held,
            missing,
            imported_sources,
        } = acted;
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
            missing_positions: serde_json::to_string(&missing).unwrap_or_else(|_| "[]".into()),
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
        // Only after the decision is durable: a restart in between then
        // still finds the files, sees them in the library, and lands again.
        if !imported_sources.is_empty()
            && let Err(error) =
                tokio::task::spawn_blocking(move || remove_sources(&imported_sources)).await
        {
            tracing::warn!(%error, "imported source cleanup did not run");
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
    let expected_sizes = manifest
        .map(|manifest| {
            manifest
                .target_files
                .iter()
                .filter(|file| file.size > 0)
                .map(|file| {
                    (
                        specs::base_name(&file.filename).to_lowercase(),
                        file.size as u64,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
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
        expected_sizes,
        wait_for_files: false,
    }
}

/// Reported paths, resolved, without anything inside the library: a
/// download client (or plugin) naming library files must never have them
/// imported, held, or removed as download sources.
fn confine(paths: Vec<PathBuf>, library_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let libraries: Vec<PathBuf> = library_dirs
        .iter()
        .map(|dir| dir.canonicalize().unwrap_or_else(|_| dir.clone()))
        .collect();
    paths
        .into_iter()
        .filter_map(|path| {
            let resolved = match path.canonicalize() {
                Ok(resolved) => resolved,
                // A missing path stays as reported so the probe counts it.
                Err(_) => return Some(path),
            };
            if libraries.iter().any(|dir| resolved.starts_with(dir)) {
                tracing::warn!(
                    path = %path.display(),
                    "download client reported a path inside the library; ignored"
                );
                return None;
            }
            Some(resolved)
        })
        .collect()
}

/// The deepest folder holding every landed audio file.
fn common_parent(files: &[LandedFile]) -> PathBuf {
    let mut parents = files.iter().filter_map(|file| file.path.parent());
    let Some(first) = parents.next() else {
        return PathBuf::new();
    };
    let mut common = first.to_path_buf();
    for parent in parents {
        while !parent.starts_with(&common) {
            if !common.pop() {
                return PathBuf::new();
            }
        }
    }
    common
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
        // Succeeds only once the folder is empty; a folder still holding
        // other files (held ones, extras) is meant to stay.
        if std::fs::remove_dir(parent).is_err() {
            tracing::debug!(dir = %parent.display(), "download folder kept: not empty");
        }
    }
}

fn now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|span| span.as_secs_f64())
        .unwrap_or(0.0)
}
