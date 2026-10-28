//! Landing: from a finished download to the library.
//!
//! When the download worker sees a transfer finish, the landing takes the
//! files from there, the way Lidarr's completed-download handling does:
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

pub mod confine;
pub mod decision;
pub mod hold;
pub mod library;
pub mod matching;
pub mod ports;
pub mod probe;
pub mod quality;
pub mod specs;
pub(crate) mod sql;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub use self::confine::{DownloadsFolder, Landed};
use self::confine::{Place, place};
use self::decision::{Decision, Outcome, check_files, decide};
use self::hold::HoldItem;
use self::matching::{FilePlan, FoundRelease, MatchSummary, find};
use self::ports::LandingLibrary;
use self::probe::{LandedFile, Landing};
use self::specs::{Check, Disposition, QualityPolicy, Rejection, Target, Verdict};
use super::downloads::journal::Journal;
use super::downloads::landing_rows::ImportDecisionRow;
use super::downloads::plan::{PlannedAlbum, TaskPlan};
use super::downloads::sources::SourceHandle;
use super::downloads::store::{StoreError, TaskDetails, TaskRow};
use crate::held::store::{
    EntryOrigin, FileState, GroupKey, HeldRoot, HeldRows, NewEntry, NewFile, Pairing as HeldPairing,
};
use crate::intake;
use crate::reason::Reason;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::DownloadPolicy;

/// The rejection a landing answers when its task records cannot be read.
fn journal_unreadable() -> Rejection {
    Rejection {
        code: Reason::JournalUnreadable,
        disposition: Disposition::Temporary,
        quarantine: None,
        detail: "the download's records cannot be read".to_owned(),
    }
}

/// One file or folder a download client reported, with the byte size it
/// advertised for that exact file when it did (Soulseek does).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reported {
    pub path: PathBuf,
    pub size: Option<u64>,
}

impl Reported {
    /// A path with no advertised size.
    pub fn path(path: PathBuf) -> Self {
        Self { path, size: None }
    }
}

/// `path` with `.` and `..` resolved by name alone, for paths that do not
/// exist (yet) and so cannot be canonicalized. `None` for a relative path
/// or one whose `..` climbs above the root.
pub fn lexical_absolute(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    if !path.is_absolute() {
        return None;
    }
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                resolved.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !resolved.pop() {
                    return None;
                }
            }
        }
    }
    Some(resolved)
}

/// What became of one landing.
#[derive(Debug, Clone, PartialEq)]
pub enum LandingResult {
    /// Files are in the library. `complete` when every requested track is
    /// there now; otherwise the download landed short.
    Imported { complete: bool },
    /// Nothing imported; the files are held for a person.
    Held { code: Reason, detail: String },
    /// Not importable; the disposition says what the worker does next.
    Rejected(Rejection),
}

/// The landing outcome plus counts for the task row and the log.
#[derive(Debug, Clone, PartialEq)]
pub struct LandingReport {
    pub result: LandingResult,
    pub files_imported: usize,
    pub files_held: usize,
    /// The landing fell short only for reasons on our side (a destination
    /// already taken, an upgrade waiting): another source would not help,
    /// so the task settles without blocklisting this one.
    pub local_only: bool,
}

/// The landing service the worker calls.
pub struct LandingService {
    journal: Arc<Journal>,
    /// The library files land in; `None` on test bundles that import
    /// nothing.
    library: Option<Arc<dyn LandingLibrary>>,
    /// The quality band landed files must sit in, read per landing.
    policy: Live<DownloadPolicy>,
    held_dir: PathBuf,
}

/// What one landing did, before it is recorded.
struct Acted {
    decision: Decision,
    imported: usize,
    held: usize,
    missing: Vec<(u32, u32)>,
    /// Short only for local reasons (see [`LandingReport::local_only`]).
    local_only: bool,
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
            local_only: false,
            imported_sources: Vec::new(),
        }
    }
}

/// One file that is held, with what it was paired to.
struct Held<'a> {
    file: &'a LandedFile,
    track: Option<usize>,
    code: Reason,
    detail: String,
}

impl LandingService {
    pub fn new(
        journal: Arc<Journal>,
        library: Option<Arc<dyn LandingLibrary>>,
        policy: Live<DownloadPolicy>,
        held_dir: PathBuf,
    ) -> Self {
        Self {
            journal,
            library,
            policy,
            held_dir,
        }
    }

    /// The library files land in.
    pub fn library(&self) -> Option<&Arc<dyn LandingLibrary>> {
        self.library.as_ref()
    }

    /// Land one task's files: confine, probe, check, match, then import,
    /// hold, or reject. Never fails: every problem becomes a result the
    /// worker can act on, and the decision is recorded. `patient` lets
    /// missing files wait for another pass (a transfer that just finished
    /// may not be visible on the mount yet).
    pub async fn land(
        &self,
        task: &TaskRow,
        attempt_id: Option<&str>,
        landed: Landed,
        patient: bool,
    ) -> LandingReport {
        let library = self.library.clone();
        let library_dirs = library
            .as_ref()
            .map(|library| library.library_dirs())
            .unwrap_or_default();
        let confined = tokio::task::spawn_blocking(move || {
            let libraries = canonical_dirs(&library_dirs);
            let folders: Vec<PathBuf> = landed
                .mounts
                .iter()
                .filter_map(|mount| mount.resolve().ok())
                .collect();
            let confined = confine(landed, &libraries);
            (confined, libraries, folders)
        })
        .await;
        let (reported, libraries, folders) = match confined {
            Ok((Ok(reported), libraries, folders)) => (reported, libraries, folders),
            Ok((Err(rejection), _, _)) => {
                return self.reject_unprobed(task, attempt_id, rejection).await;
            }
            Err(error) => {
                let rejection = local_fault(format!("path check failed: {error}"));
                return self.reject_unprobed(task, attempt_id, rejection).await;
            }
        };
        // Details or a plan that do not read are never taken as empty: that
        // would drop the task's edition pin or the copy it joins. The
        // landing waits instead.
        let records = {
            let task_id = task.id.clone();
            let attempt = attempt_id.map(str::to_owned);
            self.journal
                .read("downloads.landing.records", move |store| {
                    let details = store.task_details(&task_id)?;
                    let plan = store.task_plan(&task_id)?;
                    let handle = match &attempt {
                        Some(attempt) => store.attempt_handle_json(attempt)?,
                        None => None,
                    };
                    Ok((details, plan, handle))
                })
                .await
        };
        let (details, plan, handle) = match records {
            Ok(records) => records,
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "task records unreadable");
                return self
                    .reject_unprobed(task, attempt_id, journal_unreadable())
                    .await;
            }
        };
        let mut target = target_for(task, &details, &plan);
        target.wait_for_files = patient;
        // A waiver covers only the attempt that fetched the candidate the
        // person confirmed, never a failover or a fallback search.
        target.quality_waived = plan.quality_waived && attempt_pinned(&task.id, handle.as_deref());
        // The album's chosen edition (manual identity, pin, best fit) is
        // the only acceptable release: matching, the missing positions and
        // the pinned-edition check all use it. A download fetched for one
        // copy is matched against that copy's release instead.
        if let Some(planned) = &target.planned_album {
            target.release_mbid = Some(planned.release_mbid.trim().to_ascii_lowercase());
        } else if let Some(library) = &library
            && !target.release_group_mbid.is_empty()
            && let Some(pin) = library.chosen_edition(&target.release_group_mbid).await
        {
            if target
                .release_mbid
                .as_deref()
                .is_some_and(|asked| asked != pin)
            {
                tracing::info!(task_id = %task.id, %pin, "landing follows the album's chosen edition");
            }
            target.release_mbid = Some(pin);
        }
        // Sizes key on the exact reported file, never on a bare name: two
        // discs can each hold a `01 - Intro.flac`.
        target.expected_sizes = reported
            .iter()
            .filter_map(|file| Some((file.path.clone(), file.size.filter(|size| *size > 0)?)))
            .collect();
        // A band that does not read is never the default band: the landing
        // waits, the files stay and the source is not blamed.
        let Some(settings) = self.policy.read() else {
            let rejection = Rejection {
                code: Reason::SettingsUnreadable,
                disposition: Disposition::Temporary,
                quarantine: None,
                detail: "the download settings cannot be read".to_owned(),
            };
            return self.reject_unprobed(task, attempt_id, rejection).await;
        };
        let paths: Vec<PathBuf> = reported.into_iter().map(|file| file.path).collect();
        let probed = tokio::task::spawn_blocking(move || probe::probe(&paths, &libraries)).await;
        let landing = match probed {
            Ok(landing) => Landing {
                downloads_folders: folders,
                remote_names: remote_names(&task.id, handle.as_deref()),
                ..landing
            },
            Err(error) => {
                let rejection = local_fault(format!("probe failed: {error}"));
                return self.reject_unprobed(task, attempt_id, rejection).await;
            }
        };
        if landing.nothing_found() {
            match self.already_landed(task, attempt_id).await {
                Ok(Some(report)) => return report,
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(task_id = %task.id, %error, "last landing unreadable");
                    return self
                        .reject_unprobed(task, attempt_id, journal_unreadable())
                        .await;
                }
            }
        }
        let mut policy = QualityPolicy {
            quality_min: settings.quality_min.clone(),
            quality_max: settings.quality_max.clone(),
            held_tier: None,
        };
        if target.origin == "upgrade"
            && let Some(library) = &library
            && !target.release_group_mbid.is_empty()
        {
            policy.held_tier = library
                .held_tier(&target.release_group_mbid, target.planned_album_id())
                .await;
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
                let heard = hear_untagged(&target, &landing, &matched, library.as_ref()).await;
                let decision = decide(&target, &landing, &policy, &matched, heard.as_ref(), checks);
                match &decision.outcome {
                    Outcome::Import(plan) => {
                        let plan = plan.clone();
                        self.act(
                            task, &target, &landing, &matched, heard, decision, plan, library,
                        )
                        .await
                    }
                    _ => {
                        let held = self.hold_decided(task, &target, &landing, &decision).await;
                        Acted::decided(decision, held)
                    }
                }
            }
        };
        let report = self.finish(task, attempt_id, acted, &landing).await;
        if target.album_version && matches!(report.result, LandingResult::Imported { .. }) {
            self.discard_album_version_holds(task).await;
        }
        report
    }

    /// The song came in from its album: the copies earlier downloads held
    /// as "only the album version" have nothing left to decide, so they
    /// are discarded. Best effort: a failure leaves them held.
    async fn discard_album_version_holds(&self, task: &TaskRow) {
        let Some(recording) = task.recording_mbid.clone() else {
            return;
        };
        let (user_id, task_id) = (task.user_id.clone(), task.id.clone());
        let clock = self.journal.db().clock().clone();
        let discarded = self
            .journal
            .db()
            .write_background("downloads.album_version_discard", move |tx| {
                let rows = HeldRows::new(tx);
                let files = rows.album_version_files(&user_id, &recording, &task_id)?;
                let now = clock.now_ms();
                for file in &files {
                    rows.resolve(file.entry_id, &[file.id], FileState::Discarded, now)?;
                }
                Ok::<_, droppedneedle_kernel::db::DbError>(files)
            })
            .await;
        match discarded {
            Ok(files) => {
                for file in files {
                    super::held::remove_copy(&self.held_dir, &file.path).await;
                }
            }
            Err(error) => {
                tracing::warn!(task_id = %task.id, %error, "album version holds not discarded");
            }
        }
    }

    /// Record a rejection made before anything on disk was read.
    async fn reject_unprobed(
        &self,
        task: &TaskRow,
        attempt_id: Option<&str>,
        rejection: Rejection,
    ) -> LandingReport {
        let decision = Decision {
            checks: Vec::new(),
            outcome: Outcome::Reject(rejection),
            release_mbid: None,
            distance: None,
        };
        self.finish(
            task,
            attempt_id,
            Acted::decided(decision, 0),
            &Landing::default(),
        )
        .await
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
                code: *code,
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
    ) -> Result<Option<LandingReport>, StoreError> {
        let task_id = task.id.clone();
        let attempt = attempt_id.map(str::to_owned);
        let last = self
            .journal
            .read("downloads.landing.last", move |store| {
                store.last_landing_of_attempt(&task_id, attempt.as_deref())
            })
            .await?;
        let Some(last) = last else {
            return Ok(None);
        };
        let complete = match last.outcome.as_str() {
            "imported" => true,
            "partial" => false,
            _ => return Ok(None),
        };
        tracing::info!(task_id = %task.id, "landing already imported this attempt");
        Ok(Some(LandingReport {
            result: LandingResult::Imported { complete },
            files_imported: usize::try_from(last.files_imported).unwrap_or(0),
            files_held: usize::try_from(last.files_held).unwrap_or(0),
            // Short with nothing left to ask a source for: local holds.
            local_only: !complete && last.missing_positions.trim() == "[]",
        }))
    }

    /// Carry out an import plan through the one intake: place the files,
    /// hold what must be held, and work out what is still missing. `heard`
    /// is what AcoustID already said about the paired files, when it was
    /// asked.
    #[allow(clippy::too_many_arguments)]
    async fn act(
        &self,
        task: &TaskRow,
        target: &Target,
        landing: &Landing,
        matched: &MatchSummary,
        heard: Option<HashMap<String, Vec<String>>>,
        mut decision: Decision,
        plan: FilePlan,
        library: &Arc<dyn LandingLibrary>,
    ) -> Acted {
        let Some(found) = matched.best() else {
            return Acted::decided(decision, 0);
        };
        let held: Vec<intake::Hold> = plan
            .held
            .iter()
            .map(|(file, track)| intake::Hold {
                file: *file,
                track: Some(*track),
                reason: Reason::TagMismatch,
                detail: found
                    .scored
                    .pair_for(*file)
                    .and_then(|pair| specs::artist_conflict(target, landing, found, pair))
                    .unwrap_or_else(|| {
                        "the file's tags name a different track than the release has here"
                            .to_owned()
                    }),
            })
            .collect();
        let pairing = intake::Pairing {
            import: plan.import.clone(),
            owned: plan
                .owned
                .iter()
                .filter_map(|file| Some((*file, found.scored.pair_for(*file)?.track)))
                .collect(),
            held,
            // Files the release does not list are left out of a download.
            extras: Vec::new(),
        };
        let upgrade = target.origin == "upgrade";
        let batch = intake::IntakeBatch {
            files: landing
                .audio
                .iter()
                .map(|file| intake::IntakeFile {
                    file: file.clone(),
                    held_for: None,
                })
                .collect(),
            target: intake::Target {
                release: found.release.clone(),
                join: target.planned_album.clone(),
                asked: target.album_wanted.clone(),
            },
            origin: intake::Origin::Download {
                task_id: task.id.clone(),
                upgrade,
                staging: None,
                downloads_folders: Some(landing.downloads_folders.clone()),
            },
            gate: intake::Gate::Automatic,
            owner: task.user_id.clone(),
        };
        let placed = match intake::place(library.as_ref(), &batch, pairing, heard).await {
            Ok(placed) => placed,
            Err(deferred) => {
                decision.outcome = Outcome::Reject(match deferred.reason {
                    // The library changed under the import twice; the next
                    // pass decides file by file again.
                    Reason::LibraryChanged => Rejection {
                        code: Reason::LibraryChanged,
                        disposition: Disposition::Temporary,
                        quarantine: None,
                        detail: deferred.detail,
                    },
                    code => Rejection {
                        code,
                        ..local_fault(deferred.detail)
                    },
                });
                return Acted::decided(decision, 0);
            }
        };
        decision.checks.extend(placed.fingerprint.clone());

        let held: Vec<Held<'_>> = placed
            .held
            .iter()
            .filter_map(|file| match &placed.outcomes[*file] {
                intake::Outcome::Held {
                    track,
                    reason,
                    detail,
                } => Some(Held {
                    file: &landing.audio[*file],
                    track: *track,
                    code: *reason,
                    detail: detail.clone(),
                }),
                _ => None,
            })
            .collect();
        let mut imported_tracks: Vec<usize> = Vec::new();
        let mut imported_sources = Vec::new();
        let mut owned_tracks: HashSet<usize> = HashSet::new();
        for (file, outcome) in placed.outcomes.iter().enumerate() {
            match outcome {
                intake::Outcome::Imported {
                    track: Some(track), ..
                } => {
                    imported_tracks.push(*track);
                    imported_sources.push(landing.audio[file].path.clone());
                }
                intake::Outcome::AlreadyOwned { track } => {
                    owned_tracks.insert(*track);
                }
                _ => {}
            }
        }
        let first_hold = held.first().map(|item| (item.code, item.detail.clone()));
        // Tracks held for a reason on our side: a new source would bring
        // the same files to the same taken destination, so they are not
        // asked for again.
        let locally_held: HashSet<usize> = held
            .iter()
            .filter(|item| !item.code.against_source())
            .filter_map(|item| item.track)
            .collect();
        let any_local = held.iter().any(|item| !item.code.against_source());
        self.record_wrong_product(task, target, &held, imported_tracks.len())
            .await;
        let held_count = self.hold(task, target, landing, Some(found), held).await;

        // What the release still lacks after this landing: the next
        // candidate is asked for these positions only.
        let covered: HashSet<usize> = imported_tracks.iter().copied().collect();
        let lacking: Vec<usize> = if target.is_track {
            Vec::new()
        } else {
            found
                .release
                .tracks
                .iter()
                .enumerate()
                .filter(|(index, track)| {
                    !covered.contains(index)
                        && !matched.owns(track)
                        && !owned_tracks.contains(index)
                })
                .map(|(index, _)| index)
                .collect()
        };
        let missing: Vec<(u32, u32)> = lacking
            .iter()
            .filter(|index| !locally_held.contains(index))
            .map(|index| {
                let track = &found.release.tracks[*index];
                (track.disc.max(1), track.position)
            })
            .collect();
        let requested_landed = if target.is_track {
            found.requested_pair(target).is_some_and(|pair| {
                covered.contains(&pair.track)
                    || (!upgrade && matched.owns(&found.release.tracks[pair.track]))
            })
        } else {
            lacking.is_empty()
        };
        // Short only for local reasons when nothing is left to ask a source
        // for: every track still lacking sits behind a local hold, so
        // another source would bring the same files to the same place.
        let local_only = if target.is_track {
            !requested_landed
                && found
                    .requested_pair(target)
                    .is_some_and(|pair| locally_held.contains(&pair.track))
        } else {
            any_local && missing.is_empty()
        };
        let anything_landed = !imported_tracks.is_empty() || (!upgrade && !owned_tracks.is_empty());
        decision.outcome = if anything_landed {
            Outcome::Import(FilePlan {
                complete: requested_landed,
                ..plan
            })
        } else if let Some((code, detail)) = first_hold {
            Outcome::Hold { code, detail }
        } else if upgrade {
            Outcome::Reject(Rejection {
                code: Reason::NotAnUpgrade,
                disposition: Disposition::Permanent,
                quarantine: None,
                detail: "no file beats the copy the library holds".to_owned(),
            })
        } else {
            Outcome::Hold {
                code: Reason::NoTracks,
                detail: "no file could be imported".to_owned(),
            }
        };
        Acted {
            decision,
            imported: imported_tracks.len(),
            held: held_count,
            missing,
            local_only,
            imported_sources,
        }
    }

    /// The wrong-product verdict: an album whose files all failed on
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
            || held.iter().any(|item| item.code != Reason::TagMismatch)
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
        let clock = self.journal.db().clock().clone();
        if let Err(error) = self
            .journal
            .run("downloads.wrong_product", move |store| {
                let now = crate::db::unix_secs_f64(clock.as_ref());
                store.record_wrong_product_verdict(&task_id, &folder, now)
            })
            .await
        {
            tracing::warn!(task_id = %task.id, %error, "wrong-product verdict not recorded");
        }
    }

    /// Hold files, each with the release track it was paired to, in the
    /// download's entry of the held list.
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
                hold_item(
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
        let entry = NewEntry {
            user_id: task.user_id.clone(),
            origin: EntryOrigin::Download,
            key: GroupKey::for_download(
                &task.download_type,
                task.recording_mbid.as_deref(),
                Some(target.release_group_mbid.as_str()),
                &task.id,
            ),
            release_group_mbid: non_empty(&target.release_group_mbid),
            recording_mbid: (task.download_type == "track")
                .then(|| task.recording_mbid.clone())
                .flatten(),
            drop_item_id: None,
            artist_name: non_empty(&target.artist_name),
            album_title: non_empty(&target.album_title),
        };
        hold::hold(
            self.journal.db(),
            &self.held_dir,
            &landing.downloads_folders,
            &entry,
            items,
        )
        .await
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
            local_only,
            imported_sources,
        } = acted;
        let result = match &decision.outcome {
            Outcome::Import(plan) => LandingResult::Imported {
                complete: plan.complete,
            },
            Outcome::Hold { code, detail } => LandingResult::Held {
                code: *code,
                detail: detail.clone(),
            },
            Outcome::Reject(rejection) => LandingResult::Rejected(rejection.clone()),
        };
        let (outcome, reason_code, detail) = match &result {
            LandingResult::Imported { complete: true } => ("imported", None, None),
            LandingResult::Imported { complete: false } => {
                ("partial", Some(Reason::TracksMissing), None)
            }
            LandingResult::Held { code, detail } => ("held", Some(*code), Some(detail.clone())),
            LandingResult::Rejected(rejection) => (
                if rejection.disposition == Disposition::Temporary {
                    "deferred"
                } else {
                    "rejected"
                },
                Some(rejection.code),
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
            decided_at: self.journal.db().now_unix_f64(),
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
        intake::dispose::remove_downloaded(imported_sources, landing.downloads_folders.clone())
            .await;
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
            local_only,
        }
    }
}

/// What the task asked for, from its row, details, and plan.
pub fn target_for(task: &TaskRow, details: &TaskDetails, plan: &TaskPlan) -> Target {
    Target {
        artist_name: task.artist_name.clone(),
        album_title: task.album_title.clone(),
        release_group_mbid: task.release_group_mbid.trim().to_ascii_lowercase(),
        release_mbid: details
            .release_mbid
            .clone()
            .or_else(|| plan.release_mbid.clone())
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
        album_version: task.download_type == "track" && plan.album_version,
        album_positions: plan.track_album.as_ref().map(|album| {
            (
                plan.release_mbid
                    .as_deref()
                    .unwrap_or_default()
                    .to_ascii_lowercase(),
                album.wanted.clone(),
            )
        }),
        expected_sizes: HashMap::new(),
        wait_for_files: false,
        album_wanted: plan
            .album_wanted
            .iter()
            .map(|at| (at.disc.max(1), at.track))
            .collect(),
        planned_album: planned_album(details, plan),
        quality_waived: false,
    }
}

/// The copy the download joins, only while the plan's release is the one
/// asked for.
fn planned_album(details: &TaskDetails, plan: &TaskPlan) -> Option<PlannedAlbum> {
    let planned = plan.planned_album.clone()?;
    let asked = details
        .release_mbid
        .as_deref()
        .or(plan.release_mbid.as_deref())?
        .trim();
    let same = [
        Some(planned.release_mbid.as_str()),
        plan.release_mbid.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|held| held.trim().eq_ignore_ascii_case(asked));
    same.then_some(planned)
}

/// The reported paths as they sit inside the client's downloads folders,
/// without anything inside the library: a download client (or plugin)
/// naming library files must never have them imported, held, or removed as
/// download sources. One path outside the folders, or reached through a
/// link, refuses the whole landing before anything is read. A path whose
/// folder cannot be resolved right now is left out, so the landing reads
/// as missing files and waits like any other. Blocking.
fn confine(landed: Landed, libraries: &[PathBuf]) -> Result<Vec<Reported>, Rejection> {
    let mut kept = Vec::with_capacity(landed.files.len());
    let mut refused = 0usize;
    for file in landed.files {
        let resolved = match place(&file.path, &landed.mounts) {
            Place::Inside(resolved) => resolved,
            Place::Outside => {
                tracing::warn!(
                    path = %file.path.display(),
                    "reported path is outside the client's downloads folder or behind a link; refused"
                );
                refused += 1;
                continue;
            }
            Place::Unknown => {
                tracing::warn!(
                    path = %file.path.display(),
                    "the client's downloads folder cannot be resolved right now; path not read"
                );
                continue;
            }
        };
        if libraries.iter().any(|dir| resolved.starts_with(dir)) {
            tracing::warn!(
                path = %file.path.display(),
                "download client reported a path inside the library; ignored"
            );
            continue;
        }
        kept.push(Reported {
            path: resolved,
            size: file.size,
        });
    }
    if refused > 0 {
        return Err(Rejection {
            code: Reason::OutsideDownloadsFolder,
            disposition: Disposition::LocalFault,
            quarantine: None,
            detail: format!(
                "{refused} reported path(s) are outside the client's downloads folder or reached through a link"
            ),
        });
    }
    Ok(kept)
}

/// Whether this attempt fetched exactly the candidate a person picked, as
/// its journaled handle records. A handle that does not decode is not.
fn attempt_pinned(task_id: &str, handle: Option<&str>) -> bool {
    let Some(json) = handle.filter(|json| !json.is_empty()) else {
        return false;
    };
    match serde_json::from_str::<SourceHandle>(json) {
        Ok(handle) => handle.pinned,
        Err(error) => {
            tracing::warn!(task_id, %error, "attempt handle does not decode; not a pinned pick");
            false
        }
    }
}

/// The file names the client reported for this attempt, as it named them
/// in the attempt's journaled handle.
fn remote_names(task_id: &str, handle: Option<&str>) -> Vec<String> {
    let Some(json) = handle.filter(|json| !json.is_empty()) else {
        return Vec::new();
    };
    match serde_json::from_str::<SourceHandle>(json) {
        Ok(handle) => {
            let mut names = handle.filenames;
            names.sort();
            names.dedup();
            names
        }
        Err(error) => {
            tracing::warn!(task_id, %error, "attempt handle does not decode; no remote names");
            Vec::new()
        }
    }
}

/// AcoustID's answer for every paired file, asked only when the files are
/// untagged and nothing else shows who they are by: then a fingerprint is
/// the last evidence that can let them import. `None` when not asked.
async fn hear_untagged(
    target: &Target,
    landing: &Landing,
    matched: &MatchSummary,
    library: &dyn LandingLibrary,
) -> Option<HashMap<String, Vec<String>>> {
    let found = matched.best()?;
    if found.scored.pairs.is_empty() || specs::artist_proof(target, landing, found, None).is_some()
    {
        return None;
    }
    let files = found
        .scored
        .pairs
        .iter()
        .map(|pair| {
            (
                pair.local.to_string(),
                landing.audio[pair.local].path.clone(),
            )
        })
        .collect();
    Some(library.fingerprints(files).await)
}

/// Library folders as the filesystem resolves them.
pub(crate) fn canonical_dirs(dirs: &[PathBuf]) -> Vec<PathBuf> {
    dirs.iter()
        .map(|dir| {
            dir.canonicalize()
                .ok()
                .or_else(|| lexical_absolute(dir))
                .unwrap_or_else(|| dir.clone())
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
        code: Reason::LocalFault,
        disposition: Disposition::LocalFault,
        quarantine: None,
        detail,
    }
}

/// Every check's verdict as JSON; a hold or reject also carries its plain
/// sentence (`message`) and suggested `action` from its reason.
fn checks_json(checks: &[Check]) -> String {
    let explained: Vec<serde_json::Value> = checks
        .iter()
        .map(|check| {
            let mut value = serde_json::to_value(check).unwrap_or_default();
            let code = match &check.verdict {
                Verdict::Hold { code, .. } => Some(*code),
                Verdict::Reject(rejection) => Some(rejection.code),
                Verdict::Accept { .. } => None,
            };
            if let (Some(code), Some(fields)) = (code, value.as_object_mut()) {
                fields.insert("message".into(), code.message().into());
                fields.insert("action".into(), code.action().into());
            }
            value
        })
        .collect();
    serde_json::to_string(&explained).unwrap_or_else(|error| {
        tracing::warn!(%error, "import checks not serialized");
        "[]".to_owned()
    })
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// The held file for one landed file, with the release track it was
/// paired to when there is one. The file is named by its path inside the
/// download, so two discs' `01.flac` stay apart.
fn hold_item(
    task: &TaskRow,
    target: &Target,
    file: &LandedFile,
    paired: Option<(&FoundRelease, usize)>,
    root: &Path,
    code: Reason,
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
        file: NewFile {
            root: HeldRoot::Held,
            path: String::new(),
            original_name: relative,
            pairing: HeldPairing {
                release_mbid: track
                    .map(|(release, _)| release.id.clone())
                    .or_else(|| target.release_mbid.clone()),
                release_track_mbid: track.map(|(_, track)| track.id.clone()),
                recording_mbid: track
                    .map(|(_, track)| track.recording_id.clone())
                    .or_else(|| tag.musicbrainz_recording_id.clone()),
                disc_number: Some(i64::from(
                    track.map_or(tag.disc_number.max(1), |(_, track)| track.disc.max(1)),
                )),
                track_number: Some(i64::from(
                    track.map_or(tag.track_number, |(_, track)| track.position),
                ))
                .filter(|number| *number > 0),
                track_title: track
                    .map(|(_, track)| track.title.clone())
                    .or_else(|| non_empty(&tag.title)),
                expected_duration_seconds: track
                    .and_then(|(_, track)| track.length_ms)
                    .map(|ms| ms as f64 / 1000.0),
            },
            file_format: Some(file.format.as_str().to_owned()),
            duration_seconds: file.header.duration_seconds,
            evidence_title: non_empty(&tag.title),
            evidence_artist: non_empty(&tag.artist),
            evidence_score: None,
            source: task.source.clone(),
            source_task_id: Some(task.id.clone()),
            reason: code,
            reason_detail: Some(detail.to_owned()).filter(|detail| !detail.is_empty()),
        },
    }
}
