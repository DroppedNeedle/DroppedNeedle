//! Whether an album is already organized: every file was last written by
//! an applied run, or left as it was by a wide preview, of the same
//! profile, naming and tagging scripts, from the edition, release, track
//! match and kept tag values it has now, and is still where and as that
//! run left it. Automatic management and wide previews share this one
//! check. It reads rows and stats only, never tags or bytes, so a
//! re-preview of an organized folder is cheap.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension as _, params};
use serde::Deserialize;

use super::models::{Eligibility, ManagementError, PlannedItem};
use super::service::Management;
use super::store::{self, ManagedMark, TrackState};
use super::{automatic, custom, planner};
use crate::clock::LibraryClock as _;
use crate::identify::models::{AlbumIdentity, TrackIdentity};
use crate::identify::sqlite::{read_album_identity, read_track_identity};
use crate::publish::Sandbox;
use droppedneedle_kernel::db::Lane;
use droppedneedle_kernel::runtime_config::sections::TaggingScript;

/// One file of the album, where the catalog has it.
pub struct FileAt<'a> {
    pub track_id: &'a str,
    pub root_id: &'a str,
    pub relative_path: &'a str,
}

/// One tagging script as a job was pinned with it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ScriptPin {
    pub id: String,
    pub revision: String,
}

/// What this run would apply.
pub struct OrganizedPins<'a> {
    pub profile_id: &'a str,
    pub profile_revision: &'a str,
    pub naming_revision: &'a str,
    pub tagging_scripts: Vec<ScriptPin>,
    /// The library folder the run moves files into, when not their own.
    pub expected_root: Option<&'a str>,
}

/// The pins of a run's tagging scripts, in the order they run.
pub fn script_pins(scripts: &[TaggingScript]) -> Vec<ScriptPin> {
    scripts
        .iter()
        .map(|script| ScriptPin {
            id: script.id.clone(),
            revision: script.revision.clone(),
        })
        .collect()
}

/// The tagging scripts each applied job ran with, kept for a whole run so
/// a job's snapshot is read once. `None` when its snapshot does not say.
#[derive(Default)]
pub struct AppliedScripts(HashMap<String, Option<Vec<ScriptPin>>>);

/// The release an album's files are written from: its MusicBrainz release,
/// or for a custom edition the sealed manifest (a new one at every seal).
/// A match taken off and chosen again starts its revision again at 1, so
/// the revision alone cannot tell two releases apart.
pub(super) fn applied_release(
    conn: &Connection,
    album_id: &str,
    album: Option<&AlbumIdentity>,
) -> rusqlite::Result<Option<String>> {
    let Some(album) = album else {
        return Ok(None);
    };
    if let Some(release) = &album.release_mbid {
        return Ok(Some(release.clone()));
    }
    Ok(custom::active(conn, album_id)?.map(|release| release.id))
}

/// The release track a file is written from. A custom edition's file sits
/// on its manifest's track, so the manifest stands in for it.
pub(super) fn applied_release_track(
    track: Option<&TrackIdentity>,
    release: Option<&str>,
) -> Option<String> {
    track
        .and_then(|track| track.release_track_mbid.clone())
        .or_else(|| {
            release
                .filter(|release| release.starts_with(custom::CUSTOM_PREFIX))
                .map(str::to_owned)
        })
}

/// The match and kept tag values one file is written from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    track_revision: Option<u64>,
    release_track: Option<String>,
    override_revision: u64,
}

/// The match an album and its files are written from.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Identities {
    album_revision: Option<u64>,
    release: Option<String>,
    files: Vec<FileIdentity>,
}

/// Read on `conn`, so a caller's statements agree.
fn read_identities(
    conn: &Connection,
    album_id: &str,
    files: &[FileAt<'_>],
) -> rusqlite::Result<Identities> {
    let album = read_album_identity(conn, album_id)?;
    let release = applied_release(conn, album_id, album.as_ref())?;
    let mut identities = Vec::with_capacity(files.len());
    for file in files {
        let track = read_track_identity(conn, file.track_id)?;
        identities.push(FileIdentity {
            track_revision: track.as_ref().map(|track| track.row_revision),
            release_track: applied_release_track(track.as_ref(), release.as_deref()),
            override_revision: planner::overrides(conn, album_id, file.track_id)?.revision,
        });
    }
    Ok(Identities {
        album_revision: album.map(|album| album.row_revision),
        release,
        files: identities,
    })
}

/// What is known of one file.
struct FileFacts<'a> {
    state: &'a TrackState,
    applied_scripts: Option<&'a [ScriptPin]>,
    album_revision: Option<u64>,
    release: Option<&'a str>,
    identity: &'a FileIdentity,
    live_path_revision: Option<&'a str>,
    root_id: &'a str,
}

/// Whether one file is as this run would leave it.
fn file_organized(pins: &OrganizedPins<'_>, facts: &FileFacts<'_>) -> bool {
    let state = facts.state;
    let identity = facts.identity;
    let revision = |value: Option<u64>| value.and_then(|value| i64::try_from(value).ok());
    state.last_run_succeeded()
        && state.profile_id.as_deref() == Some(pins.profile_id)
        && state.profile_revision.as_deref() == Some(pins.profile_revision)
        && state.naming_revision.as_deref() == Some(pins.naming_revision)
        && facts.applied_scripts == Some(pins.tagging_scripts.as_slice())
        && state.root_id.as_deref() == Some(pins.expected_root.unwrap_or(facts.root_id))
        // The edition, release and file's match it was written from.
        && state.album_identity_revision.is_some()
        && state.album_identity_revision == revision(facts.album_revision)
        && state.applied_release_mbid.is_some()
        && state.applied_release_mbid.as_deref() == facts.release
        && state.track_identity_revision.is_some()
        && state.track_identity_revision == revision(identity.track_revision)
        && state.applied_release_track_mbid.is_some()
        && state.applied_release_track_mbid == identity.release_track
        // The file is still where, and as, the run left it.
        && state.path_revision.is_some()
        && state.path_revision.as_deref() == facts.live_path_revision
        && state.override_revision.as_deref()
            == Some(identity.override_revision.to_string().as_str())
}

/// Whether a planned file is left exactly as it is: eligible with no
/// warning (an outside source that could not be read plans with one), no
/// reason, nothing to change and nothing to move.
fn plans_no_change(item: &PlannedItem, file: &FileAt<'_>) -> bool {
    item.eligibility == Eligibility::Eligible
        && item.reason().is_none()
        && item.facts.blockers.is_empty()
        && !item.diff.changes_anything()
        && !item.fingerprint.is_empty()
        && item.source_root == file.root_id
        && item.source_rel == file.relative_path
        && item
            .dest_root
            .as_deref()
            .is_none_or(|root| root == file.root_id)
        && item
            .dest_rel
            .as_deref()
            .is_none_or(|rel| rel == file.relative_path)
}

/// The tagging scripts job `job_id` was pinned with.
fn job_tagging_scripts(
    conn: &Connection,
    job_id: &str,
) -> rusqlite::Result<Option<Vec<ScriptPin>>> {
    let text: Option<String> = conn
        .prepare_cached(super::sql::JOB_TAGGING_SCRIPTS.text())?
        .query_row(params![job_id], |row| row.get(0))
        .optional()?
        .flatten();
    Ok(text.and_then(|text| serde_json::from_str(&text).ok()))
}

/// An album's matches and file places, taken before it is planned: what a
/// record that it is organized is made from, and checked against.
pub struct BeforePlan {
    identities: Identities,
    paths: Vec<Option<String>>,
    /// The files' management state revisions, so a record written by
    /// anything else meanwhile (an upgrade, a plugin's file) is not
    /// overwritten.
    states: HashMap<String, i64>,
}

impl Management {
    /// Whether every file of the album is already organized under `pins`
    /// (see the module). An album with no files never is. A file whose
    /// place cannot be read (a link, gone) never is. Blocking.
    pub fn already_organized(
        &self,
        album_id: &str,
        files: &[FileAt<'_>],
        pins: &OrganizedPins<'_>,
        sandbox: &Sandbox,
        applied: &mut AppliedScripts,
    ) -> Result<bool, ManagementError> {
        if files.is_empty() {
            return Ok(false);
        }
        let known = &applied.0;
        let read = self
            .parts
            .db
            .read_blocking("management.already_organized", |conn| {
                // A background read whose statements must agree: one
                // snapshot, so a run committing in between cannot pair one
                // file's old state with another's new one.
                let mut states = Vec::with_capacity(files.len());
                let mut scripts: Vec<(String, Option<Vec<ScriptPin>>)> = Vec::new();
                for file in files {
                    let Some(state) = store::track_state(conn, file.track_id)? else {
                        return Ok::<_, ManagementError>(None);
                    };
                    if !state.last_run_succeeded() {
                        return Ok(None);
                    }
                    if let Some(job) = state.last_operation_job_id.as_deref()
                        && !known.contains_key(job)
                        && !scripts.iter().any(|(id, _)| id == job)
                    {
                        scripts.push((job.to_owned(), job_tagging_scripts(conn, job)?));
                    }
                    states.push(state);
                }
                Ok(Some((
                    states,
                    read_identities(conn, album_id, files)?,
                    scripts,
                )))
            })?;
        let Some((states, identities, scripts)) = read else {
            return Ok(false);
        };
        applied.0.extend(scripts);
        for ((file, state), identity) in files.iter().zip(&states).zip(&identities.files) {
            let live = automatic::path_revision(sandbox, file.root_id, file.relative_path);
            let facts = FileFacts {
                state,
                applied_scripts: state
                    .last_operation_job_id
                    .as_deref()
                    .and_then(|job| applied.0.get(job))
                    .and_then(Option::as_deref),
                album_revision: identities.album_revision,
                release: identities.release.as_deref(),
                identity,
                live_path_revision: live.as_deref(),
                root_id: file.root_id,
            };
            if !file_organized(pins, &facts) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// The album's matches and its files' places, before it is planned.
    /// Blocking.
    pub(super) fn before_plan(
        &self,
        album_id: &str,
        files: &[FileAt<'_>],
        sandbox: &Sandbox,
    ) -> Result<BeforePlan, ManagementError> {
        let paths = files
            .iter()
            .map(|file| automatic::path_revision(sandbox, file.root_id, file.relative_path))
            .collect();
        let (identities, states) =
            self.parts
                .db
                .read_blocking("management.before_plan", |conn| {
                    // Statements that must agree: the album's and its
                    // files' matches and records as one snapshot.
                    Ok::<_, ManagementError>((
                        read_identities(conn, album_id, files)?,
                        super::takeover::state_revisions(conn, &[album_id.to_owned()])?,
                    ))
                })?;
        Ok(BeforePlan {
            identities,
            paths,
            states,
        })
    }

    /// Record the album as organized by the preview `job_id` when its plan
    /// leaves every file exactly as it is, so the next wide preview leaves
    /// it out. Nothing is recorded when a file plans a change or has a
    /// reason, when a file's place or stat moved while it was planned (a
    /// hand edit landing then), when its matches or kept values moved, or
    /// over a file restored to its original. Writes management state only,
    /// never a file. Returns whether it recorded. Blocking.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn record_organized(
        &self,
        album_id: &str,
        files: &[FileAt<'_>],
        items: &[PlannedItem],
        before: &BeforePlan,
        pins: &OrganizedPins<'_>,
        job_id: &str,
        sandbox: &Sandbox,
    ) -> Result<bool, ManagementError> {
        if files.is_empty() || items.len() != files.len() {
            return Ok(false);
        }
        let mut found = Vec::with_capacity(files.len());
        for (file, path) in files.iter().zip(&before.paths) {
            let Some(item) = items
                .iter()
                .find(|item| item.track_id.as_deref() == Some(file.track_id))
            else {
                return Ok(false);
            };
            let now = automatic::path_revision(sandbox, file.root_id, file.relative_path);
            if !plans_no_change(item, file) || path.is_none() || now != *path {
                return Ok(false);
            }
            found.push(item.fingerprint.as_str());
        }
        let now = self.parts.clock.now_secs();
        self.parts
            .db
            .write_blocking(Lane::Background, "management.record_organized", |tx| {
                if read_identities(tx, album_id, files)? != before.identities
                    || super::takeover::state_revisions(tx, &[album_id.to_owned()])?
                        != before.states
                {
                    return Ok::<_, ManagementError>(false);
                }
                for file in files {
                    if store::track_state(tx, file.track_id)?.is_some_and(|state| state.restored())
                    {
                        return Ok(false);
                    }
                }
                let identities = &before.identities;
                for (((file, identity), path), sha) in files
                    .iter()
                    .zip(&identities.files)
                    .zip(&before.paths)
                    .zip(&found)
                {
                    store::mark_managed(
                        tx,
                        &ManagedMark {
                            track_id: file.track_id,
                            root_id: file.root_id,
                            profile_id: pins.profile_id,
                            profile_revision: pins.profile_revision,
                            naming_revision: pins.naming_revision,
                            override_revision: identity.override_revision,
                            job_id,
                            album_identity_revision: identities.album_revision,
                            track_identity_revision: identity.track_revision,
                            release_mbid: identities.release.as_deref(),
                            release_track_mbid: identity.release_track.as_deref(),
                            path_revision: path.clone(),
                            found_sha256: Some(sha),
                        },
                        now,
                    )?;
                }
                Ok(true)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn organized_state() -> TrackState {
        TrackState {
            profile_id: Some("profile".to_owned()),
            profile_revision: Some("profile-rev".to_owned()),
            naming_revision: Some("naming-rev".to_owned()),
            override_revision: Some("0".to_owned()),
            root_id: Some("music".to_owned()),
            outcome: Some("succeeded".to_owned()),
            managed_at: Some(1.0),
            album_identity_revision: Some(3),
            track_identity_revision: Some(5),
            path_revision: Some("path-rev".to_owned()),
            last_operation_job_id: Some("job".to_owned()),
            applied_release_mbid: Some("release".to_owned()),
            applied_release_track_mbid: Some("release-track".to_owned()),
        }
    }

    fn scripts() -> Vec<ScriptPin> {
        vec![
            ScriptPin {
                id: "a".to_owned(),
                revision: "1".to_owned(),
            },
            ScriptPin {
                id: "b".to_owned(),
                revision: "1".to_owned(),
            },
        ]
    }

    fn pins(expected_root: Option<&str>) -> OrganizedPins<'_> {
        OrganizedPins {
            profile_id: "profile",
            profile_revision: "profile-rev",
            naming_revision: "naming-rev",
            tagging_scripts: scripts(),
            expected_root,
        }
    }

    /// One input changed from the organized case.
    struct Case {
        name: &'static str,
        state: TrackState,
        applied: Option<Vec<ScriptPin>>,
        album: Option<u64>,
        release: Option<&'static str>,
        track: Option<u64>,
        release_track: Option<&'static str>,
        overrides: u64,
        live: Option<&'static str>,
        expected_root: Option<&'static str>,
    }

    fn organized() -> Case {
        Case {
            name: "organized",
            state: organized_state(),
            applied: Some(scripts()),
            album: Some(3),
            release: Some("release"),
            track: Some(5),
            release_track: Some("release-track"),
            overrides: 0,
            live: Some("path-rev"),
            expected_root: None,
        }
    }

    fn judge(case: &Case) -> bool {
        file_organized(
            &pins(case.expected_root),
            &FileFacts {
                state: &case.state,
                applied_scripts: case.applied.as_deref(),
                album_revision: case.album,
                release: case.release,
                identity: &FileIdentity {
                    track_revision: case.track,
                    release_track: case.release_track.map(str::to_owned),
                    override_revision: case.overrides,
                },
                live_path_revision: case.live,
                root_id: "music",
            },
        )
    }

    /// A file counts as organized only while every input matches what its
    /// last applied run recorded: each one changed alone plans it again.
    #[test]
    fn a_file_is_organized_only_while_every_input_matches_its_run() {
        assert!(judge(&organized()), "the unchanged file is organized");
        let changed = |name, edit: &dyn Fn(&mut Case)| {
            let mut case = organized();
            case.name = name;
            edit(&mut case);
            case
        };
        let cases = [
            changed("an undo left it managed", &|case| {
                case.state.outcome = Some("managed".to_owned())
            }),
            changed("restored to its original", &|case| {
                case.state.outcome = Some("restored".to_owned())
            }),
            changed("another profile", &|case| {
                case.state.profile_id = Some("other".to_owned())
            }),
            changed("the profile was edited", &|case| {
                case.state.profile_revision = Some("old".to_owned())
            }),
            changed("the naming script was edited", &|case| {
                case.state.naming_revision = Some("old".to_owned())
            }),
            changed("a tagging script was edited", &|case| {
                case.applied.as_mut().expect("scripts")[1].revision = "0".to_owned()
            }),
            changed("the tagging scripts run in another order", &|case| {
                case.applied.as_mut().expect("scripts").reverse()
            }),
            changed("a tagging script was added", &|case| {
                case.applied.as_mut().expect("scripts").pop();
            }),
            changed("the applied job's scripts are unknown", &|case| {
                case.applied = None
            }),
            changed("the run moves it into another folder", &|case| {
                case.expected_root = Some("archive")
            }),
            changed("it was organized in another folder", &|case| {
                case.state.root_id = Some("archive".to_owned())
            }),
            changed("another edition", &|case| case.album = Some(4)),
            changed("the album has no edition", &|case| case.album = None),
            changed("the state names no edition", &|case| {
                case.state.album_identity_revision = None
            }),
            changed("the match was chosen again on another release", &|case| {
                case.release = Some("other")
            }),
            changed("the album has no release", &|case| case.release = None),
            changed("the state names no release", &|case| {
                case.state.applied_release_mbid = None;
                case.release = None;
            }),
            changed("another track match", &|case| case.track = Some(6)),
            changed("the file sits on another release track", &|case| {
                case.release_track = Some("other")
            }),
            changed("the state names no release track", &|case| {
                case.state.applied_release_track_mbid = None;
                case.release_track = None;
            }),
            changed("the file has no track match", &|case| case.track = None),
            changed("the state names no track match", &|case| {
                case.state.track_identity_revision = None
            }),
            changed("the file changed or moved", &|case| {
                case.live = Some("other")
            }),
            changed("the file cannot be read", &|case| case.live = None),
            changed("no place was recorded", &|case| {
                case.state.path_revision = None;
                case.live = None;
            }),
            changed("a kept tag value changed", &|case| case.overrides = 1),
        ];
        for case in &cases {
            assert!(!judge(case), "{}: must be planned again", case.name);
        }
        let mut moved_there = organized();
        moved_there.state.root_id = Some("archive".to_owned());
        moved_there.expected_root = Some("archive");
        assert!(
            judge(&moved_there),
            "already in the folder the run moves files into"
        );
    }

    /// A file planned with a warning (an outside source that could not be
    /// reached, a cover that could not be fetched) is never recorded as
    /// left as it is, so a preview made during an outage looks again.
    #[test]
    fn only_a_file_planned_without_a_warning_is_left_as_it_is() {
        use super::super::models::{DesiredDocument, ItemDiff, ItemFacts};
        let file = FileAt {
            track_id: "track",
            root_id: "music",
            relative_path: "a/01.flac",
        };
        let item = |eligibility| PlannedItem {
            ordinal: 0,
            bundle_ordinal: 0,
            album_id: "album".to_owned(),
            album_revision: 1,
            track_id: Some("track".to_owned()),
            source_root: "music".to_owned(),
            source_rel: "a/01.flac".to_owned(),
            dest_root: None,
            dest_rel: None,
            eligibility,
            estimated_bytes: 0,
            fingerprint: "sha".to_owned(),
            desired: DesiredDocument::default(),
            diff: ItemDiff::default(),
            facts: ItemFacts::default(),
        };
        assert!(plans_no_change(&item(Eligibility::Eligible), &file));
        assert!(
            !plans_no_change(&item(Eligibility::Warning), &file),
            "a warning is looked at again"
        );
    }
}
