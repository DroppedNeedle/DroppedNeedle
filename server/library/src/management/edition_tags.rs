//! Writing an album's chosen edition's tags to its files, from the
//! edition sheet: a management preview of the files placed on the
//! edition, planned with the folder's own profile for tags only, then its
//! apply and the undo of that apply. A curator asks; the job is theirs,
//! and another curator is told it does not exist. Every refusal comes
//! before a job is made.

use droppedneedle_auth::Principal;
use rusqlite::{Connection, OptionalExtension as _, params};

use super::models::{
    EditionPin, ItemFilter, JobDetail, JobIntent, JobMode, ManagementError, Origin, PlannedItem,
    PreviewHandle, Selection, SelectionKind,
};
use super::service::{ApplyRequest, JobSpec, Management, PAGE_MAX};
use super::store::Snapshot;
use super::{profile, reasons, takeover};
use crate::operations::reasons::Reason;

/// Why an edition's tags were not queued or reached.
#[derive(Debug)]
pub enum EditionTagsError {
    /// The album or its folder rules it out (409, with the reason).
    Refused(Reason),
    Management(ManagementError),
}

impl From<ManagementError> for EditionTagsError {
    fn from(error: ManagementError) -> Self {
        Self::Management(error)
    }
}

const fn refusal(code: &'static str, message: &'static str, action: &'static str) -> Reason {
    Reason {
        code,
        message,
        action,
    }
}

pub const SETTINGS_UNREADABLE: Reason = refusal(
    "LIBRARY_MANAGEMENT_UNREADABLE",
    "The Library Management settings could not be read.",
    "Open Settings > Library Management and save them again.",
);
pub const NO_EDITION: Reason = refusal(
    "EDITION_TAGS_NO_EDITION",
    "This album has no edition to take tags from.",
    "Choose an edition for it first.",
);
pub const CUSTOM_EDITION: Reason = refusal(
    "EDITION_TAGS_CUSTOM",
    "This album is kept as your own track list, so its files keep their tags.",
    "Choose an edition from MusicBrainz to write its tags.",
);
pub const STALE: Reason = refusal(
    "STALE_REVISION",
    "This album's edition changed since the page loaded.",
    "Reload the page and try again.",
);
pub const HELD: Reason = refusal(
    "EDITION_TAGS_HELD",
    "This album is left unmanaged, so its files keep their tags.",
    "Choose \"Manage it again\" on the album page first.",
);
pub const PLACING: Reason = refusal(
    "EDITION_REMAP_PENDING",
    "Still matching this album to the edition you chose.",
    "Try again in a moment.",
);
pub const PLACING_WAITS: Reason = refusal(
    "EDITION_REMAP_PENDING",
    "Still matching this album to the edition you chose.",
    "MusicBrainz is not answering, so this waits until it does.",
);
pub const TWO_FOLDERS: Reason = refusal(
    "EDITION_TAGS_TWO_FOLDERS",
    "This album's files sit in more than one library folder.",
    "Move them into one folder, then try again.",
);
pub const BY_ITSELF: Reason = refusal(
    "EDITION_TAGS_BY_ITSELF",
    "DroppedNeedle writes this album's tags by itself.",
    "Its tags are written without asking; see History.",
);
pub const NO_FILES: Reason = refusal(
    "EDITION_TAGS_NO_FILES",
    "None of this album's files sit on a track of this edition.",
    "Match the files to its tracks first.",
);
pub const NO_TAGS: Reason = refusal(
    "PROFILE_WRITES_NO_TAGS",
    "This folder's Library Management profile writes no tags.",
    "Turn tags on for it in Settings > Library Management.",
);

/// A tag write as the edition sheet reads it.
#[derive(Debug, Clone)]
pub struct EditionTagsJob {
    pub detail: JobDetail,
    /// The job puts an earlier tag write back.
    pub undo: bool,
    /// A write that has finished, all or in part, with files its undo can
    /// still put back.
    pub undo_available: bool,
    /// Every planned file, once planning is done.
    pub items: Vec<PlannedItem>,
}

/// What the album holds, read in one go.
struct Album {
    roots: Vec<String>,
    /// Each file's root and path.
    files: Vec<(String, String)>,
    identity: Option<crate::identify::models::PriorAlbumIdentity>,
    custom: bool,
    held: bool,
    placing: Option<i64>,
    mapped: Vec<String>,
}

fn album(conn: &Connection, album_id: &str, release_mbid: &str) -> rusqlite::Result<Option<Album>> {
    let mut statement = conn.prepare(
        "SELECT root_id, relative_path FROM local_tracks \
         WHERE local_album_id = ?1 AND availability = 'indexed' ORDER BY root_id",
    )?;
    let files = statement
        .query_map(params![album_id], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(String, String)>>>()?;
    if files.is_empty() {
        return Ok(None);
    }
    let mut roots: Vec<String> = files.iter().map(|(root, _)| root.clone()).collect();
    roots.dedup();
    let custom = conn
        .query_row(
            "SELECT 1 FROM library_custom_edition_active WHERE local_album_id = ?1",
            params![album_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let excluded = conn
        .query_row(
            "SELECT 1 FROM library_management_exclusions WHERE local_album_id = ?1",
            params![album_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    let placing = conn
        .query_row(
            "SELECT attempts FROM library_edition_remap_queue WHERE local_album_id = ?1",
            params![album_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(Some(Album {
        roots,
        files,
        identity: crate::identify::sqlite::album_identity_row(conn, album_id)?,
        custom,
        held: excluded || takeover::is_marked(conn, album_id)?,
        placing,
        mapped: crate::operations::choice::mapped_track_ids(conn, album_id, release_mbid)?,
    }))
}

impl Management {
    /// Queue a preview of `release_mbid`'s tags in the album's files, as
    /// the album's folder profile writes them, without renaming or moving
    /// anything. Refused when the edition is not the album's at
    /// `expected_identity_revision`, or the sheet would not offer it.
    /// Blocking.
    pub fn create_edition_tags(
        &self,
        album_id: &str,
        release_mbid: &str,
        expected_identity_revision: i64,
        caller: &Principal,
    ) -> Result<PreviewHandle, EditionTagsError> {
        let found = self
            .setup
            .db
            .read_blocking("management.edition_tags_album", |conn| {
                Ok::<_, ManagementError>(album(conn, album_id, release_mbid)?)
            })?
            .ok_or(ManagementError::NotFound(
                "This album is not in the library.",
            ))?;
        let refused = |reason| Err(EditionTagsError::Refused(reason));
        let Some(identity) = found.identity.as_ref() else {
            return refused(NO_EDITION);
        };
        let Some(current) = identity.release_mbid.as_deref() else {
            return refused(if found.custom {
                CUSTOM_EDITION
            } else {
                NO_EDITION
            });
        };
        if !current.eq_ignore_ascii_case(release_mbid)
            || identity.row_revision != Some(expected_identity_revision)
        {
            return refused(STALE);
        }
        if found.held {
            return refused(HELD);
        }
        match found.placing {
            Some(attempts) if attempts > 0 => return refused(PLACING_WAITS),
            Some(_) => return refused(PLACING),
            None => {}
        }
        let [root] = found.roots.as_slice() else {
            return refused(TWO_FOLDERS);
        };
        let registry = self.setup.core.live_registry();
        if found
            .files
            .iter()
            .any(|(root, path)| super::planner::keeps_own_tags(&registry, root, path))
        {
            return refused(reasons::KEEPS_OWN_TAGS);
        }
        let Some(stored) = self.readable_settings() else {
            return refused(SETTINGS_UNREADABLE);
        };
        if self.writes_by_itself(album_id)?.by_itself {
            return refused(BY_ITSELF);
        }
        if found.mapped.is_empty() {
            return refused(NO_FILES);
        }
        let pinned = profile::tags_only(&stored, root)?;
        if !pinned.profile.metadata.enabled {
            return refused(NO_TAGS);
        }
        let selection = Selection {
            kind: SelectionKind::Tracks,
            ids: found.mapped,
            catalog_filter: None,
        };
        Ok(self.start_job(JobSpec {
            mode: JobMode::Preview,
            origin: Origin::Manual,
            actor: Some(caller.user_id()),
            idempotency_key: None,
            revisions: profile::revisions(&stored, &pinned, registry.policy_revision()),
            selection_json: serde_json::to_string(&selection).map_err(ManagementError::from)?,
            pinned: &pinned,
            target_root_id: None,
            linked_job_id: None,
            intent: JobIntent {
                edition: Some(EditionPin {
                    local_album_id: album_id.to_owned(),
                    release_mbid: current.to_owned(),
                    identity_revision: expected_identity_revision,
                }),
                ..JobIntent::default()
            },
            proposed_settings_revision: None,
            retention_hours: stored.preview_retention_hours,
        })?)
    }

    /// The album's tag write, or the undo of one, when the caller made it
    /// or is an admin. Anything else does not exist to them.
    fn edition_tags_snapshot(
        &self,
        album_id: &str,
        job_id: &str,
        caller: &Principal,
    ) -> Result<Snapshot, EditionTagsError> {
        let snapshot = self.snapshot(job_id)?;
        let pin = match snapshot.intent().edition {
            Some(pin) => Some(pin),
            None if snapshot.job_mode() == Some(JobMode::Undo) => match &snapshot.linked_job_id {
                Some(source) => self.snapshot(source)?.intent().edition,
                None => None,
            },
            None => None,
        };
        let ours = pin.is_some_and(|pin| pin.local_album_id == album_id);
        let theirs = caller.is_admin()
            || snapshot.job.requested_by_user_id.as_deref() == Some(caller.user_id());
        if !(ours && theirs) {
            return Err(ManagementError::NotFound("This tag write does not exist.").into());
        }
        Ok(snapshot)
    }

    /// Where the album's tag write stands and, once planned, each file's
    /// changes. Blocking.
    pub fn edition_tags(
        &self,
        album_id: &str,
        job_id: &str,
        caller: &Principal,
    ) -> Result<EditionTagsJob, EditionTagsError> {
        let snapshot = self.edition_tags_snapshot(album_id, job_id, caller)?;
        let detail = self.detail(job_id)?;
        let mut items = Vec::new();
        if snapshot.phase != crate::jobs::ManagementPhase::Planning {
            let mut after = -1;
            loop {
                let page = self.items(job_id, after, PAGE_MAX, &ItemFilter::default())?;
                items.extend(page.items);
                match page.next_after_ordinal {
                    Some(next) if page.has_more => after = next,
                    _ => break,
                }
            }
        }
        Ok(EditionTagsJob {
            undo: snapshot.job_mode() == Some(JobMode::Undo),
            undo_available: snapshot.job_mode() == Some(JobMode::Apply)
                && snapshot.phase == crate::jobs::ManagementPhase::Complete
                && detail.undo_available_count > 0,
            detail,
            items,
        })
    }

    /// Write the album's planned tag write, or put an earlier one back.
    /// Blocking.
    pub fn apply_edition_tags(
        &self,
        album_id: &str,
        job_id: &str,
        request: ApplyRequest,
        caller: &Principal,
    ) -> Result<EditionTagsJob, EditionTagsError> {
        self.edition_tags_snapshot(album_id, job_id, caller)?;
        self.apply(job_id, request)?;
        self.edition_tags(album_id, job_id, caller)
    }

    /// Plan putting the album's applied tag write back, as a job the sheet
    /// applies like the write itself. Blocking.
    pub fn undo_edition_tags(
        &self,
        album_id: &str,
        job_id: &str,
        expected_row_revision: i64,
        idempotency_key: &str,
        caller: &Principal,
    ) -> Result<PreviewHandle, EditionTagsError> {
        self.edition_tags_snapshot(album_id, job_id, caller)?;
        if self.readable_settings().is_none() {
            return Err(EditionTagsError::Refused(SETTINGS_UNREADABLE));
        }
        Ok(self.undo_preview(
            job_id,
            expected_row_revision,
            idempotency_key,
            caller.user_id(),
        )?)
    }
}
