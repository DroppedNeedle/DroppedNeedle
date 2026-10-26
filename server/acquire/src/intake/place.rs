//! `place`: from a matched batch to the library, in one import.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use super::{Deferred, Gate, IntakeBatch, Origin, Outcome, Pairing, Placement};
use crate::landing::ports::{
    ExtraFile, ImportFailure, ImportFile, ImportOrigin, ImportRequest, LandingLibrary,
};
use crate::landing::probe::LandedFile;
use crate::landing::quality;
use crate::landing::specs::{self, Check, Verdict};
use crate::reason::Reason;
use droppedneedle_kernel::runtime_config::sections::tier_rank;
use droppedneedle_library::import::ImportJoin;

/// Place a batch in the library: replace the versions its plan names,
/// compare owned tracks for an upgrade, choose the copy the files join,
/// run the fingerprint gate, then import once. `heard` is AcoustID's
/// answer for every paired file when the door already asked.
///
/// When the library changed under the import (a destination it planned
/// for was taken by the time it published, and everything was put back),
/// the batch is planned again and imported once more at once; a second
/// change defers the whole batch. Nothing is compensated here: the
/// publisher takes replaced copies to the recycle bin with the files that
/// replace them, or puts everything back.
pub async fn place(
    library: &dyn LandingLibrary,
    batch: &IntakeBatch,
    pairing: Pairing,
    heard: Option<HashMap<String, Vec<String>>>,
) -> Result<Placement, Deferred> {
    let mut answers = Answers::new(heard);
    let mut retried = false;
    loop {
        let planned = plan(library, batch, &pairing, &mut answers).await;
        let failure = match import(library, batch, &pairing, planned).await {
            Ok(placement) => return Ok(placement),
            Err(failure) => failure,
        };
        let deferred = |reason, detail| Err(Deferred { reason, detail });
        match failure {
            ImportFailure::Occupied(detail) if !retried => {
                tracing::info!(%detail, "the library changed under an import; planning it again");
                retried = true;
            }
            ImportFailure::Occupied(detail) => return deferred(Reason::LibraryChanged, detail),
            ImportFailure::LocalFault(detail) => return deferred(Reason::LocalFault, detail),
            ImportFailure::FolderMissing(folder) => {
                return deferred(Reason::LibraryFolderMissing, folder);
            }
            ImportFailure::NamingScript(detail) => {
                return deferred(Reason::NamingScriptFailed, detail);
            }
        }
    }
}

/// AcoustID's answers, asked once per file across both tries.
struct Answers {
    known: HashMap<String, Vec<String>>,
    /// The door asked for every paired file already.
    complete: bool,
    asked: HashSet<usize>,
}

impl Answers {
    fn new(heard: Option<HashMap<String, Vec<String>>>) -> Self {
        Self {
            complete: heard.is_some(),
            known: heard.unwrap_or_default(),
            asked: HashSet::new(),
        }
    }

    /// The answers for `files`, keyed by file index as text. Empty when no
    /// AcoustID key is set or nothing could be looked up.
    async fn of(
        &mut self,
        library: &dyn LandingLibrary,
        batch: &IntakeBatch,
        files: &[(usize, usize)],
    ) -> HashMap<String, Vec<String>> {
        if !self.complete {
            let missing: Vec<(String, PathBuf)> = files
                .iter()
                .filter(|(file, _)| self.asked.insert(*file))
                .map(|(file, _)| (file.to_string(), batch.files[*file].file.path.clone()))
                .collect();
            if !missing.is_empty() {
                self.known.extend(library.fingerprints(missing).await);
            }
        }
        files
            .iter()
            .filter_map(|(file, _)| {
                let key = file.to_string();
                self.known.get(&key).map(|heard| (key, heard.clone()))
            })
            .collect()
    }
}

/// One try's plan.
struct Planned {
    outcomes: Vec<Outcome>,
    held: Vec<usize>,
    /// (file, release track, library tracks it replaces).
    import: Vec<(usize, usize, Vec<String>)>,
    join: Option<ImportJoin>,
    fingerprint: Option<Check>,
}

impl Planned {
    fn hold(&mut self, file: usize, track: Option<usize>, reason: Reason, detail: String) {
        self.outcomes[file] = Outcome::Held {
            track,
            reason,
            detail,
        };
        self.held.push(file);
    }

    /// A file on a track the library holds as well or better. A person
    /// who imported it by hand is never answered by dropping it: it stays
    /// held and they are told.
    fn owned(&mut self, gate: Gate, file: usize, track: usize) {
        if gate.imports_held() {
            self.hold(
                file,
                Some(track),
                Reason::NoBetterThanYours,
                "the library already holds this track".to_owned(),
            );
        } else {
            self.outcomes[file] = Outcome::AlreadyOwned { track };
        }
    }
}

/// Steps 1 to 4: versions, upgrades, the join, the fingerprint gate.
async fn plan(
    library: &dyn LandingLibrary,
    batch: &IntakeBatch,
    pairing: &Pairing,
    answers: &mut Answers,
) -> Planned {
    let release = &batch.target.release;
    let gate = batch.gate;
    let held_for = |file: usize| batch.files[file].held_for;
    let tier = |file: usize| batch.files[file].file.tier();
    let mut planned = Planned {
        outcomes: vec![Outcome::Untouched; batch.files.len()],
        held: Vec::new(),
        import: Vec::new(),
        join: None,
        fingerprint: None,
    };
    for hold in &pairing.held {
        planned.hold(hold.file, hold.track, hold.reason, hold.detail.clone());
    }
    let mut import: Vec<(usize, usize)> = pairing.import.clone();
    let mut owned: Vec<(usize, usize)> = pairing.owned.clone();
    let mut replaces: HashMap<usize, Vec<String>> = HashMap::new();

    // 1. "Replace tracks you have in another version": each file on a
    // release track the planned copy holds in another version replaces
    // that version, while the release is the planned one and the copy
    // still names it. The old track must be on the copy, on no track of
    // the release, its album not held, and not an alias: an indexed one
    // goes to the recycle bin when the new file is as good or better, a
    // missing one is simply replaced. A worse file is held and the old one
    // stays. Otherwise the file is dropped as an owned one is.
    let mut handled: HashSet<usize> = HashSet::new();
    let mut replaced_versions: Vec<(String, String)> = Vec::new();
    if let Some(copy) = batch
        .target
        .join
        .as_ref()
        .filter(|copy| !copy.replace.is_empty() && release.answers_to(&copy.release_mbid))
    {
        for version in &copy.replace {
            let Some(track) = release
                .tracks
                .iter()
                .position(|track| track.id.eq_ignore_ascii_case(&version.release_track_mbid))
            else {
                continue;
            };
            let file = match import.iter().position(|(_, on)| *on == track) {
                Some(at) => import.remove(at).0,
                None => match owned.iter().position(|(_, on)| *on == track) {
                    Some(at) => owned.remove(at).0,
                    None => continue,
                },
            };
            handled.insert(track);
            let old = library
                .version_row(&version.local_track_id, copy, release)
                .await
                .filter(|row| {
                    row.on_planned
                        && !row.alias
                        && !row.on_release
                        && !row.held
                        && matches!(row.availability.as_str(), "indexed" | "missing")
                });
            let Some(old) = old else {
                if !gate.imports_held() {
                    planned.owned(gate, file, track);
                } else if held_for(file) == Some(Reason::VersionLowerQuality) {
                    planned.hold(
                        file,
                        Some(track),
                        Reason::VersionLowerQuality,
                        "your other version of this track can no longer be replaced".to_owned(),
                    );
                } else {
                    // Held for another reason: compared with the library's
                    // copies like any file a person imports.
                    handled.remove(&track);
                    owned.push((file, track));
                }
                continue;
            };
            let chosen = gate == Gate::PersonChoseFile
                && held_for(file) == Some(Reason::VersionLowerQuality);
            if !chosen && quality::beats(old.tier, tier(file)) {
                planned.hold(
                    file,
                    Some(track),
                    Reason::VersionLowerQuality,
                    format!(
                        "the new file is {}, your version is {}",
                        tier(file),
                        old.tier
                    ),
                );
                continue;
            }
            if old.availability == "indexed" {
                replaces.insert(file, vec![version.local_track_id.clone()]);
            }
            replaced_versions.push((
                release.tracks[track].id.clone(),
                version.local_track_id.clone(),
            ));
            import.push((file, track));
        }
    }

    // 2. The upgrade comparison: a file replaces the library's copies of
    // its track only when it is strictly better than every one of them;
    // they go to the recycle bin in the publish that places it. An upload
    // and a person's import always compare; a download compares on an
    // upgrade or for the owned tracks it asked for again.
    let join_album = batch
        .target
        .join
        .as_ref()
        .map(|copy| copy.album_id.as_str());
    let mut replaced_on: Vec<(String, String)> = Vec::new();
    let mut group_owned: Option<crate::landing::ports::OwnedTracks> = None;
    for (file, track) in owned {
        if handled.contains(&track) {
            planned.owned(gate, file, track);
            continue;
        }
        let release_track = &release.tracks[track];
        let compares = match &batch.origin {
            _ if gate.imports_held() => true,
            Origin::Upload { .. } => true,
            Origin::Download { upgrade, .. } => {
                *upgrade
                    || batch
                        .target
                        .asked
                        .contains(&(release_track.disc.max(1), release_track.position))
            }
        };
        if !compares {
            planned.owned(gate, file, track);
            continue;
        }
        let copies = library
            .owned_copies(
                &release.release_group_id,
                join_album,
                &release.id,
                &release_track.id,
                &release_track.recording_id,
            )
            .await;
        if copies.is_empty() {
            // Owned elsewhere than this release's copy: a person's import
            // is placed as asked, anything else is left as owned. A retry
            // nobody asked for never adds a second copy: the file waits
            // for a person.
            if gate == Gate::HeldRetry {
                let group_owns = match &group_owned {
                    Some(owned) => owned,
                    None => {
                        group_owned.insert(library.owned(&release.release_group_id, None).await)
                    }
                };
                if group_owns
                    .release_tracks
                    .contains(&release_track.id.to_ascii_lowercase())
                    || group_owns
                        .recordings
                        .contains(&release_track.recording_id.to_ascii_lowercase())
                {
                    planned.hold(file, Some(track), Reason::OwnedElsewhere, String::new());
                } else {
                    import.push((file, track));
                }
            } else if gate.imports_held() {
                import.push((file, track));
            } else {
                planned.owned(gate, file, track);
            }
            continue;
        }
        // A copy of the track has turned up on this album since the file
        // was held: a retry nobody asked for never replaces it. Only a file
        // whose replacement was already decided (the recycle bin refused
        // the old copy) goes on to the comparison.
        if gate == Gate::HeldRetry && held_for(file) != Some(Reason::UpgradeBlocked) {
            planned.hold(file, Some(track), Reason::OwnedOnAlbum, String::new());
            continue;
        }
        let chosen =
            gate == Gate::PersonChoseFile && held_for(file) == Some(Reason::NoBetterThanYours);
        if chosen
            || copies
                .iter()
                .all(|copy| quality::beats(tier(file), copy.tier))
        {
            for copy in &copies {
                let on = (copy.album_id.clone(), copy.release_mbid.clone());
                if !replaced_on.contains(&on) {
                    replaced_on.push(on);
                }
            }
            replaces.insert(file, copies.into_iter().map(|copy| copy.track_id).collect());
            import.push((file, track));
        } else if gate.imports_held() {
            let best = copies
                .iter()
                .map(|copy| copy.tier)
                .max_by_key(|tier| tier_rank(tier).unwrap_or(0))
                .unwrap_or_default();
            planned.hold(
                file,
                Some(track),
                Reason::NoBetterThanYours,
                format!("this file is {}, your copy is {best}", tier(file)),
            );
        } else {
            planned.owned(gate, file, track);
        }
    }

    // 3. The files join the copy they were fetched for, else the one album
    // every replaced copy sat on.
    planned.join = match (&batch.target.join, replaced_on.as_slice()) {
        (Some(copy), _) => Some(ImportJoin {
            album_id: copy.album_id.clone(),
            release_mbid: copy.release_mbid.clone(),
            replaced: replaced_versions,
        }),
        (None, [(album_id, release_mbid)]) if !release_mbid.is_empty() => Some(ImportJoin {
            album_id: album_id.clone(),
            release_mbid: release_mbid.clone(),
            replaced: Vec::new(),
        }),
        _ => None,
    };

    // 4. The fingerprint gate.
    let checked: Vec<(usize, usize)> = match gate {
        Gate::Automatic => import.clone(),
        Gate::PersonChoseAlbum => import
            .iter()
            .filter(|(file, _)| replaces.get(file).is_some_and(|old| !old.is_empty()))
            .copied()
            .collect(),
        Gate::PersonChoseFile | Gate::HeldRetry => Vec::new(),
    };
    if !checked.is_empty() {
        let heard = answers.of(library, batch, &checked).await;
        let mut refused: HashSet<usize> = HashSet::new();
        match gate {
            // A file whose audio AcoustID confidently names other
            // recordings, and whose length does not vouch for the track,
            // is held. No answer holds nothing.
            Gate::Automatic if !heard.is_empty() => {
                for (file, track) in &checked {
                    let release_track = &release.tracks[*track];
                    if specs::fingerprint_disagrees(
                        heard.get(&file.to_string()).map_or(&[], Vec::as_slice),
                        &release_track.recording_id,
                        batch.files[*file].file.header.duration_seconds,
                        release_track.length_ms.map(|ms| ms as f64 / 1000.0),
                    ) {
                        refused.insert(*file);
                        planned.hold(
                            *file,
                            Some(*track),
                            Reason::FingerprintMismatch,
                            "AcoustID identified a different recording".to_owned(),
                        );
                    }
                }
                planned.fingerprint = Some(Check {
                    spec: "fingerprint_agrees",
                    verdict: Verdict::Accept {
                        note: (!refused.is_empty()).then(|| {
                            format!(
                                "{} file(s) held: AcoustID heard another recording",
                                refused.len()
                            )
                        }),
                    },
                });
            }
            Gate::Automatic => {}
            // A person picked the album, so nothing vetted the pairing: a
            // file replaces a library copy only when AcoustID names its
            // recording. Audio that is clearly another recording is held
            // for review; another recording whose length vouches for the
            // track leaves the copy as it is; no answer keeps the file held.
            Gate::PersonChoseAlbum => {
                for (file, track) in &checked {
                    let release_track = &release.tracks[*track];
                    let recordings = heard.get(&file.to_string()).map_or(&[][..], Vec::as_slice);
                    if recordings.is_empty() {
                        refused.insert(*file);
                        planned.hold(
                            *file,
                            Some(*track),
                            Reason::FingerprintUnconfirmed,
                            "AcoustID could not confirm the recording".to_owned(),
                        );
                    } else if specs::fingerprint_disagrees(
                        recordings,
                        &release_track.recording_id,
                        batch.files[*file].file.header.duration_seconds,
                        release_track.length_ms.map(|ms| ms as f64 / 1000.0),
                    ) {
                        refused.insert(*file);
                        planned.hold(
                            *file,
                            Some(*track),
                            Reason::FingerprintMismatch,
                            "AcoustID identified a different recording".to_owned(),
                        );
                    } else if !recordings
                        .iter()
                        .any(|heard| heard.eq_ignore_ascii_case(&release_track.recording_id))
                    {
                        refused.insert(*file);
                        planned.owned(gate, *file, *track);
                    }
                }
            }
            Gate::PersonChoseFile | Gate::HeldRetry => {}
        }
        import.retain(|(file, _)| !refused.contains(file));
    }
    planned.import = import
        .into_iter()
        .map(|(file, track)| (file, track, replaces.remove(&file).unwrap_or_default()))
        .collect();
    planned
}

/// Steps 5 and 6: the one library import, and each file's outcome.
async fn import(
    library: &dyn LandingLibrary,
    batch: &IntakeBatch,
    pairing: &Pairing,
    mut planned: Planned,
) -> Result<Placement, ImportFailure> {
    let mut placement = Placement {
        outcomes: Vec::new(),
        held: Vec::new(),
        paths: Vec::new(),
        tags_kept: 0,
        fingerprint: planned.fingerprint.take(),
    };
    if planned.import.is_empty() && pairing.extras.is_empty() {
        placement.outcomes = planned.outcomes;
        placement.held = planned.held;
        return Ok(placement);
    }
    let (task_id, origin, staging, read_beneath) = match &batch.origin {
        Origin::Download {
            task_id,
            staging,
            downloads_folders,
            ..
        } => (
            task_id.clone(),
            ImportOrigin::Download,
            staging.clone(),
            downloads_folders.clone(),
        ),
        Origin::Upload { drop_item_id } => (
            format!("drop-{drop_item_id}"),
            ImportOrigin::Drop,
            None,
            None,
        ),
    };
    let request = ImportRequest {
        task_id: task_id.clone(),
        origin,
        staging,
        read_beneath,
        release: batch.target.release.clone(),
        join: planned.join.clone(),
        files: planned
            .import
            .iter()
            .map(|(file, track, replaces)| ImportFile {
                path: batch.files[*file].file.path.clone(),
                track: *track,
                replaces: replaces.clone(),
            })
            .collect(),
        extras: pairing
            .extras
            .iter()
            .map(|file| extra_file(&batch.files[*file].file))
            .collect(),
        actor: batch.owner.clone(),
    };
    let receipt = library.import(request).await?;
    let skipped: HashMap<usize, String> = receipt.skipped.into_iter().collect();
    let blocked: HashMap<usize, String> = receipt.blocked.into_iter().collect();
    let imported = std::mem::take(&mut planned.import);
    for (index, (file, track, replaces)) in imported.iter().enumerate() {
        match (skipped.get(&index), blocked.get(&index)) {
            (Some(detail), _) => {
                planned.hold(*file, Some(*track), Reason::TargetOccupied, detail.clone());
            }
            (None, Some(detail)) => {
                planned.hold(*file, Some(*track), Reason::UpgradeBlocked, detail.clone());
            }
            (None, None) => {
                planned.outcomes[*file] = Outcome::Imported {
                    track: Some(*track),
                    upgraded: !replaces.is_empty(),
                };
            }
        }
    }
    for (offset, file) in pairing.extras.iter().enumerate() {
        match skipped.get(&(imported.len() + offset)) {
            Some(detail) => planned.hold(*file, None, Reason::TargetOccupied, detail.clone()),
            None => {
                planned.outcomes[*file] = Outcome::Imported {
                    track: None,
                    upgraded: false,
                };
            }
        }
    }
    tracing::info!(
        task_id,
        bundle = %receipt.bundle_id,
        album = %receipt.album_id,
        files = receipt.paths.len(),
        "files imported into the library"
    );
    placement.outcomes = planned.outcomes;
    placement.held = planned.held;
    placement.paths = receipt.paths;
    placement.tags_kept = receipt.tags_kept;
    Ok(placement)
}

/// An extra file as the album will carry it: its own title (else the
/// file name) and its own disc and track numbers.
fn extra_file(file: &LandedFile) -> ExtraFile {
    let title = file.tag.title.trim();
    ExtraFile {
        path: file.path.clone(),
        title: if title.is_empty() {
            file.path
                .file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else {
            title.to_owned()
        },
        disc: file.tag.disc_number.max(1),
        position: file.tag.track_number,
    }
}
