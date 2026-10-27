//! Automatic management: once an imported or newly identified album is
//! ready, run its library folder's profile on it without asking, as v2
//! did after a scan or an import. It only runs for folders whose
//! automatic switch is on and whose dry run is still current, never for
//! an album whose match is unconfirmed (unless "Also manage unconfirmed
//! matches" is on), and always from the album's chosen edition: a new
//! edition re-tags and re-paths the files. A run with a blocked file
//! waits on the operations page with its reasons.
//!
//! Albums wait in `library_management_automatic_queue`. The worker looks
//! at one only a moment after it was last queued and its identification
//! last landed, while no identification of it is queued, running or
//! deferred (so its own writes have settled, and never right after a
//! start), and while no scan runs.
//! Changes to other albums never hold it back. An album it skips for a
//! reason the person can act on stays listed with that reason until it is
//! queued again.

use droppedneedle_kernel::db::Lane;
use rusqlite::{Connection, OptionalExtension as _, params};

use super::models::{JobIntent, JobMode, ManagementError, Origin, Selection, SelectionKind};
use super::reasons::{self, ItemReason};
use super::service::{JobSpec, Management};
use super::{planner, profile, store, takeover};
use crate::clock::LibraryClock as _;
use crate::identify::models::AlbumIdentity;
use crate::management::settings::{activation_is_current, migration_carry_applies};
use crate::operations::status;
use crate::publish::Sandbox;
use crate::publish::snapshots::sha256_hex;
use crate::scan::sqlite_store::{holds_new_files, judge_remembered, remembered};
use droppedneedle_kernel::runtime_config::sections::{
    LibraryManagement, LibraryManagementRootAssignment,
};

/// How long after it was last queued automatic management looks at an
/// album (v2's settle window, per album).
pub const SETTLE_SECONDS: f64 = 30.0;
/// How many times an album that is not ready yet is looked at again.
const MAX_ATTEMPTS: i64 = 8;
/// First wait before looking again; it doubles each time, up to six hours.
const FIRST_RETRY_SECONDS: f64 = 60.0;
/// `not_before` of an album skipped until something changes.
pub(super) const PARKED: f64 = 1.0e15;
/// Skip code of an album with files a scan found in a folder it holds
/// back.
pub const SCAN_HELD_BACK: &str = "SCAN_HELD_BACK";

/// When the worker first looked, so the first looks after a start wait.
#[derive(Debug, Clone, Default)]
pub struct Settle {
    started_at: Option<f64>,
}

/// Whether the app writes an album's files by itself, and whether it is
/// still to do so.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AutomaticWrites {
    pub by_itself: bool,
    pub pending: bool,
}

/// The first checks of a look at an album: whether automatic management
/// would write it at all as things stand.
enum Gate {
    /// Never written by itself until something changes.
    Closed(Verdict),
    /// A scan followed a retag made outside the app: the album is marked
    /// taken over, then parked.
    Follow,
    /// Not ready yet; looked at again later.
    NotYet(Verdict),
    /// A lifted hold let some of the album's files go before the album was
    /// judged as a retag: it is judged now, then the gate is passed again.
    /// `writes` says whether the folder writes the album by itself.
    Judge { writes: bool },
    /// The rest of the look goes on.
    Open(Box<Opened>),
}

/// What the rest of a look needs once the gate is open.
struct Opened {
    files: Vec<AlbumFile>,
    stored: LibraryManagement,
    assignment: LibraryManagementRootAssignment,
    origin: Origin,
    policy: String,
    states: Vec<Option<store::TrackState>>,
    identity: AlbumIdentity,
    /// Each file and the revision of the track match it is written from.
    mapping: Vec<(String, u64)>,
}

/// What automatic management decided for one album.
enum Verdict {
    /// A run was queued.
    Started(String),
    /// Nothing to do; nothing to tell anyone.
    Done(&'static str),
    /// Skipped until something changes; the reason is shown.
    Park(&'static str),
    /// Not ready yet; look again later (then park).
    Wait(&'static str),
    /// Look again at this time without counting an attempt.
    RetryAt(&'static str, f64),
}

/// An album automatic management skipped, and why.
#[derive(Debug, Clone)]
pub struct SkippedAlbum {
    pub album_id: String,
    pub album_title: String,
    pub artist_name: Option<String>,
    pub reason: ItemReason,
    pub attempts: i64,
    pub updated_at: f64,
}

fn reason(code: &str, message: &str, action: &str) -> ItemReason {
    ItemReason {
        code: code.to_owned(),
        message: message.to_owned(),
        action: action.to_owned(),
    }
}

/// A skip code in words, with what to do about it.
pub fn skip_reason(code: &str) -> ItemReason {
    match code {
        "ACTIVATION_STALE" => reason(
            code,
            "The library folder's dry run is out of date, so automatic management is paused for it.",
            "Run and confirm a new dry run in Settings > Library Management.",
        ),
        "RESTORED" => reason(
            code,
            "You put this album back to its original files, so automatic management leaves it alone.",
            "Make a preview by hand if you want it managed again.",
        ),
        "EXCLUDED" => reason(
            code,
            "This album is kept out of file organizing.",
            "Re-enable organizing on the album page.",
        ),
        code if code == reasons::KEEPS_OWN_TAGS.code => reasons::keeps_own_tags(),
        takeover::TAKEN_OVER => reason(
            code,
            "This album's files were changed outside DroppedNeedle, so it is left unmanaged.",
            "Choose \"Manage it again\" on the album page to have DroppedNeedle retag and rename it from its edition.",
        ),
        "SPANS_FOLDERS" => reason(
            code,
            "This album's files are in more than one library folder.",
            "Move them into one folder, or manage the album with a preview by hand.",
        ),
        "PREVIEW_BLOCKED" => reason(
            code,
            "The last automatic run for this album has files that can't be managed.",
            "Open it on the operations page; each file says why. It is tried again when that preview expires.",
        ),
        "RELEASE_NOT_SELECTED" => reasons::release_not_selected(),
        "CUSTOM_EDITION" => reason(
            code,
            "This album is a custom edition, and the library folder does not manage custom editions automatically.",
            "Turn on \"Custom editions\" for the folder in Settings > Library Management, or manage the album with a preview by hand.",
        ),
        "METADATA_UNAVAILABLE" => reason(
            code,
            "MusicBrainz credits for this album could not be read, so it waits.",
            "Nothing to do: it is looked at again by itself, at least every six hours.",
        ),
        "ACOUSTID_UNAVAILABLE" => reason(
            code,
            "AcoustID could not be reached for this album's AcoustID IDs, so it waits.",
            "Nothing to do: it is looked at again by itself, at least every six hours.",
        ),
        "UNCONFIRMED_MATCH" => reasons::unconfirmed_match(),
        "IDENTITY_NOT_ACCEPTED" => reasons::identity_not_accepted(),
        "TRACK_NOT_MAPPED" => reasons::track_not_mapped(),
        SCAN_HELD_BACK => reason(
            code,
            "The last scan of this album's library folder found most of its files gone at once, and this album's files arrived after that. DroppedNeedle leaves them alone in case they are your own retag.",
            "Usually nothing to do: the next full scan of the folder lets the album go ahead. If a drive or share is not mounted, mount it and scan again. If you deleted files there on purpose, the scan holds the folder until you remove the albums they belonged to from the library. To have this album written now, preview it by itself.",
        ),
        _ => reason(
            "MANAGEMENT_FAILED",
            "Automatic management could not look at this album.",
            "It tries again when the album changes; the server log has the details.",
        ),
    }
}

/// Queue one album for automatic management (an import landed, its
/// identification settled, or its edition changed). A queued album is
/// looked at again from the start.
pub fn queue(conn: &Connection, album_id: &str, now: f64) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO library_management_automatic_queue (local_album_id, queued_at, not_before) \
         SELECT ?1, ?2, ?2 WHERE EXISTS (SELECT 1 FROM local_albums WHERE id = ?1) \
         ON CONFLICT (local_album_id) DO UPDATE SET queued_at = excluded.queued_at, \
         not_before = excluded.not_before, attempts = 0, last_reason_code = NULL",
        params![album_id, now],
    )?;
    Ok(())
}

/// Queue an album a scan followed across a retag, with the stamp that
/// makes automatic management leave it unmanaged before it plans anything
/// (`retag_followed_at`, in milliseconds). Only for an album with an
/// edition someone chose and no file the app has a record of: the app
/// would otherwise write the edition's old name over the retag. [`queue`]
/// keeps the stamp; marking the album or handing it back clears it.
pub(crate) fn stamp_retag_followed(
    conn: &Connection,
    album_id: &str,
    now: f64,
) -> rusqlite::Result<()> {
    queue(conn, album_id, now)?;
    conn.execute(
        "UPDATE library_management_automatic_queue SET retag_followed_at = ?2 \
         WHERE local_album_id = ?1",
        params![album_id, (now * 1000.0) as i64],
    )?;
    Ok(())
}

/// Whether a person asked for the album's taken-over mark to be lifted and
/// the worker has yet to do it.
fn hand_back_stamped(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM library_album_takeovers \
             WHERE local_album_id = ?1 AND handed_back_at IS NOT NULL",
            params![album_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Whether the album waits with a scan's retag stamp.
fn retag_followed(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM library_management_automatic_queue \
             WHERE local_album_id = ?1 AND retag_followed_at IS NOT NULL",
            params![album_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

/// Look again at the skipped albums whose reason may have gone (`None`
/// wakes every one).
pub fn wake(conn: &Connection, code: Option<&str>, now: f64) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE library_management_automatic_queue SET not_before = ?1, attempts = 0 \
         WHERE last_reason_code IS NOT NULL AND not_before > ?1 \
         AND (?2 IS NULL OR last_reason_code = ?2)",
        params![now, code],
    )
}

/// A file's place and stat signature: changes when the file is moved,
/// rewritten or touched, without hashing its bytes.
pub fn path_revision(sandbox: &Sandbox, root_id: &str, relative_path: &str) -> Option<String> {
    let path = sandbox.resolve_no_symlink(root_id, relative_path).ok()?;
    let meta = std::fs::symlink_metadata(&path).ok()?;
    let modified = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(sha256_hex(
        format!(
            "{root_id}\u{0}{relative_path}\u{0}{}\u{0}{modified}",
            meta.len()
        )
        .as_bytes(),
    ))
}

/// Queue an album for automatic management. A failure only logs: the
/// album is then managed by hand, never left half done. Blocking.
pub(crate) fn queue_automatic_management(
    db: &droppedneedle_kernel::db::Db,
    clock: &dyn droppedneedle_kernel::clock::Clock,
    album_id: &str,
) {
    let queued = db.write_blocking(Lane::Background, "management.queue_automatic", |tx| {
        Ok::<_, droppedneedle_kernel::db::DbError>(queue(tx, album_id, clock.now_secs())?)
    });
    if let Err(error) = queued {
        tracing::warn!(%error, album_id, "album not queued for automatic management");
    }
}

/// One indexed file of an album.
struct AlbumFile {
    id: String,
    root_id: String,
    relative_path: String,
    ingest_source: String,
    stat_revision: String,
}

fn album_files(conn: &Connection, album_id: &str) -> rusqlite::Result<Vec<AlbumFile>> {
    let mut statement = conn.prepare_cached(
        "SELECT id, root_id, relative_path, ingest_source, stat_revision FROM local_tracks \
         WHERE local_album_id = ?1 AND availability = 'indexed' ORDER BY id",
    )?;
    let rows = statement.query_map(params![album_id], |row| {
        Ok(AlbumFile {
            id: row.get(0)?,
            root_id: row.get(1)?,
            relative_path: row.get(2)?,
            ingest_source: row.get(3)?,
            stat_revision: row.get(4)?,
        })
    })?;
    rows.collect()
}

/// How the album came in, when its folder manages albums that came in
/// that way without asking. `None` when it does not.
fn automatic_origin(
    assignment: &droppedneedle_kernel::runtime_config::sections::LibraryManagementRootAssignment,
    files: &[AlbumFile],
) -> Option<Origin> {
    let origin = if files.iter().any(|file| file.ingest_source == "download") {
        Origin::Acquisition
    } else if files
        .iter()
        .any(|file| file.ingest_source.starts_with("drop"))
    {
        Origin::DropImport
    } else {
        Origin::ScanDiscovered
    };
    let triggered = match origin {
        Origin::Acquisition => assignment.automatic_acquisitions,
        Origin::DropImport => assignment.automatic_drop_imports,
        _ => assignment.automatic_scan_discovered,
    };
    (assignment.enabled && triggered).then_some(origin)
}

impl Management {
    /// Whether the album sits in one folder that manages it without
    /// asking: the test automatic management makes before it looks.
    pub(super) fn managed_automatically(&self, album_id: &str) -> Result<bool, ManagementError> {
        let files = self
            .setup
            .db
            .read_blocking("management.managed_automatically", |conn| {
                Ok::<_, ManagementError>(album_files(conn, album_id)?)
            })?;
        let Some(first) = files.first() else {
            return Ok(false);
        };
        if files.iter().any(|file| file.root_id != first.root_id) {
            return Ok(false);
        }
        let assignment = profile::root_assignment(&self.settings()?, &first.root_id);
        Ok(automatic_origin(&assignment, &files).is_some())
    }

    /// Whether the app writes the album's files by itself (the first
    /// checks of a look would let it through, a stamped hand-back counting
    /// as done), and whether such a write is still to come: an automatic
    /// run for the album is open, or its queue row is neither parked nor
    /// waiting on a blocked preview. Blocking.
    pub fn writes_by_itself(&self, album_id: &str) -> Result<AutomaticWrites, ManagementError> {
        let open = match self.automatic_gate(album_id, true)? {
            Gate::Closed(_) | Gate::Follow | Gate::Judge { writes: false } => false,
            // A file with no track to map to stays so once the album's files
            // are placed and matched (a file the chosen edition has no track
            // for), and the look parks: only a placing or a matching still
            // to come can map it.
            Gate::NotYet(Verdict::Wait("TRACK_NOT_MAPPED")) => {
                self.setup
                    .db
                    .read_blocking("management.placing_attempts", |conn| {
                        Ok::<_, ManagementError>(
                            status::placing_attempts(conn, album_id)?.is_some()
                                || status::identifying(conn, album_id)?,
                        )
                    })?
            }
            Gate::NotYet(_) | Gate::Judge { writes: true } | Gate::Open(_) => true,
        };
        if !open {
            return Ok(AutomaticWrites::default());
        }
        let pending = self
            .setup
            .db
            .read_blocking("management.pending_writes", |conn| {
                Ok::<_, ManagementError>(conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM library_management_automatic_queue \
                 WHERE local_album_id = ?1 AND not_before < ?2 \
                 AND last_reason_code IS NOT 'PREVIEW_BLOCKED') \
                 OR EXISTS(SELECT 1 FROM library_operation_jobs j \
                 JOIN library_management_job_snapshots s ON s.job_id = j.id \
                 WHERE j.kind = ?3 AND j.state IN ('queued', 'running', 'paused') \
                 AND j.idempotency_key LIKE 'automatic:%' \
                 AND EXISTS(SELECT 1 FROM json_each(s.selection_json, '$.ids') \
                 WHERE value = ?1))",
                    params![album_id, PARKED, store::KIND],
                    |row| row.get(0),
                )?)
            })?;
        Ok(AutomaticWrites {
            by_itself: true,
            pending,
        })
    }

    /// Whether an indexed file of the album sits in a folder that keeps
    /// its own tags, by the live folder settings. Blocking.
    pub fn keeps_own_tags(&self, album_id: &str) -> Result<bool, ManagementError> {
        let files = self
            .setup
            .db
            .read_blocking("management.album_files", |conn| {
                Ok::<_, ManagementError>(album_files(conn, album_id)?)
            })?;
        let registry = self.setup.core.live_registry();
        Ok(files
            .iter()
            .any(|file| planner::keeps_own_tags(&registry, &file.root_id, &file.relative_path)))
    }

    /// Look at the next waiting album that has settled, while no scan
    /// runs. True when one was looked at or handed back. Blocking.
    pub fn automatic_step(&self, settle: &mut Settle) -> Result<bool, ManagementError> {
        self.automatic_step_at(settle, self.setup.clock.now_secs())
    }

    /// [`Self::automatic_step`] at `now` (unix seconds). Blocking.
    pub fn automatic_step_at(
        &self,
        settle: &mut Settle,
        now: f64,
    ) -> Result<bool, ManagementError> {
        // Albums a person handed back all at once come first, one a step.
        // That writes no music file, so it waits for neither a scan nor a
        // settled album.
        if let Some(album_id) = self
            .setup
            .db
            .read_blocking("management.next_stamped", |conn| {
                Ok::<_, ManagementError>(takeover::next_stamped(conn, now)?)
            })?
        {
            return match takeover::hand_back(&self.setup, &album_id, None) {
                Ok(_) => Ok(true),
                // Not a try that failed: the loop waits and it goes again.
                Err(error @ ManagementError::Busy(_)) => Err(error),
                Err(error) => {
                    // It waits before its next try; the loop pauses too.
                    tracing::warn!(%error, album_id, "album not handed back; tried again later");
                    self.setup.db.write_blocking(
                        Lane::Background,
                        "management.hand_back_failed",
                        |tx| {
                            Ok::<_, ManagementError>(takeover::hand_back_failed(
                                tx, &album_id, now,
                            )?)
                        },
                    )?;
                    Ok(false)
                }
            };
        }
        if !self.setup.coordinator.current().is_empty() {
            return Ok(false);
        }
        // The first looks after a start wait the settle window too, so a
        // boot-time scan or import can begin first.
        let started_at = *settle.started_at.get_or_insert(now);
        if now - started_at < SETTLE_SECONDS {
            return Ok(false);
        }
        if planner::manage_unconfirmed(&self.setup) {
            self.setup
                .db
                .write_blocking(Lane::Background, "management.wake", |conn| {
                    Ok::<_, ManagementError>(wake(conn, Some("UNCONFIRMED_MATCH"), now)?)
                })?;
        }
        self.look_at_due(now, Some(now - SETTLE_SECONDS))
    }

    /// Look at the next waiting album that is due, settled or not.
    /// Blocking.
    pub fn automatic_next(&self) -> Result<bool, ManagementError> {
        self.look_at_due(self.setup.clock.now_secs(), None)
    }

    /// Look at the next album due at `now`. With `settled_before`, only
    /// one queued by then whose identification is not live and has not
    /// landed since. A busy read
    /// is an error, never an empty queue. Blocking.
    fn look_at_due(&self, now: f64, settled_before: Option<f64>) -> Result<bool, ManagementError> {
        let due: Option<(String, i64)> =
            self.setup
                .db
                .read_blocking("management.automatic_due", |conn| {
                    Ok::<_, ManagementError>(
                        conn.query_row(
                            super::sql::AUTOMATIC_DUE.text(),
                            params![now, settled_before],
                            |row| Ok((row.get(0)?, row.get(1)?)),
                        )
                        .optional()?,
                    )
                })?;
        let Some((album_id, attempts)) = due else {
            return Ok(false);
        };
        let verdict = match self.automatic_verdict(&album_id) {
            Ok(verdict) => verdict,
            // A busy database says nothing about the album: it stays due,
            // the try is not counted, and the loop waits before the next.
            Err(error @ ManagementError::Busy(_)) => return Err(error),
            Err(error) => {
                tracing::warn!(%error, album_id, "automatic management could not look at an album");
                Verdict::Wait("MANAGEMENT_FAILED")
            }
        };
        // (next look, attempts added, reason shown); `None` drops the row.
        let next: Option<(f64, i64, &str)> = match verdict {
            Verdict::Started(job_id) => {
                tracing::info!(album_id, job_id, "automatic management started");
                None
            }
            Verdict::Done(code) => {
                tracing::debug!(album_id, code, "automatic management has nothing to do");
                None
            }
            Verdict::Park(code) => {
                tracing::info!(album_id, code, "automatic management skipped an album");
                Some((PARKED, 0, code))
            }
            // MusicBrainz or AcoustID coming back is something the worker
            // sees by itself, so those waits never park.
            Verdict::Wait(code)
                if attempts + 1 >= MAX_ATTEMPTS
                    && !matches!(code, "METADATA_UNAVAILABLE" | "ACOUSTID_UNAVAILABLE") =>
            {
                tracing::info!(
                    album_id,
                    code,
                    "automatic management stopped waiting for an album"
                );
                Some((PARKED, 1, code))
            }
            Verdict::Wait(code) => {
                let delay = (FIRST_RETRY_SECONDS * 2f64.powi(attempts as i32)).min(6.0 * 3600.0);
                Some((now + delay, 1, code))
            }
            Verdict::RetryAt(code, at) => Some((at.max(now + FIRST_RETRY_SECONDS), 0, code)),
        };
        self.setup
            .db
            .write_blocking(Lane::Background, "management.automatic_settle", |conn| {
                match next {
                    None => conn.execute(
                        "DELETE FROM library_management_automatic_queue WHERE local_album_id = ?1",
                        params![album_id],
                    )?,
                    Some((at, added, code)) => conn.execute(
                        "UPDATE library_management_automatic_queue SET attempts = attempts + ?2, \
                     not_before = ?3, last_reason_code = ?4 WHERE local_album_id = ?1",
                        params![album_id, added, at, code],
                    )?,
                };
                Ok::<_, ManagementError>(())
            })?;
        Ok(true)
    }

    /// Albums automatic management skipped, newest first. Blocking.
    pub fn automatic_skipped(&self, limit: i64) -> Result<Vec<SkippedAlbum>, ManagementError> {
        self.setup.db.read_blocking("management.automatic_skipped", |conn| {
            let mut statement = conn.prepare(
                "SELECT q.local_album_id, a.title, a.album_artist_name, q.last_reason_code, q.attempts, \
                 q.queued_at FROM library_management_automatic_queue q \
                 JOIN local_albums a ON a.id = q.local_album_id \
                 WHERE q.last_reason_code IS NOT NULL ORDER BY q.queued_at DESC LIMIT ?1",
            )?;
            let rows = statement.query_map(params![limit.clamp(1, 200)], |row| {
                Ok(SkippedAlbum {
                    album_id: row.get(0)?,
                    album_title: row.get(1)?,
                    artist_name: row.get(2)?,
                    reason: skip_reason(&row.get::<_, String>(3)?),
                    attempts: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            })?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })
    }

    /// The first checks of a look, which write nothing. `passing_hand_back`
    /// lets a taken-over album whose hand-back is stamped through, as the
    /// worker lifts its mark before it looks again.
    fn automatic_gate(
        &self,
        album_id: &str,
        passing_hand_back: bool,
    ) -> Result<Gate, ManagementError> {
        let files = self
            .setup
            .db
            .read_blocking("management.album_files", |conn| {
                Ok::<_, ManagementError>(album_files(conn, album_id)?)
            })?;
        let Some(first) = files.first() else {
            return Ok(Gate::Closed(Verdict::Done("ALBUM_GONE")));
        };
        if files.iter().any(|file| file.root_id != first.root_id) {
            return Ok(Gate::Closed(Verdict::Park("SPANS_FOLDERS")));
        }
        let stored = self.settings()?;
        let assignment = profile::root_assignment(&stored, &first.root_id);
        let Some(origin) = automatic_origin(&assignment, &files) else {
            // A followed retag keeps its stamp while the folder can still
            // write the album by itself some other way (an acquisition into
            // it, say): it is marked rather than dropped.
            let writes_some = assignment.enabled
                && (assignment.automatic_acquisitions
                    || assignment.automatic_drop_imports
                    || assignment.automatic_scan_discovered);
            if writes_some
                && self
                    .setup
                    .db
                    .read_blocking("management.retag_followed", |conn| {
                        Ok::<_, ManagementError>(retag_followed(conn, album_id)?)
                    })?
            {
                return Ok(Gate::Follow);
            }
            // A held album let go before it was judged is judged here too,
            // once its match would be written, so a preview of many albums
            // cannot write it over a retag.
            if self.judged_by_hand(album_id)? {
                return Ok(Gate::Judge { writes: false });
            }
            return Ok(Gate::Closed(Verdict::Done("NOT_AUTOMATIC")));
        };
        let registry = self.setup.core.live_registry();
        let policy = registry.policy_revision().to_owned();
        let pinned_policy = self.activation_policy_revision()?;
        if !activation_is_current(&stored, &assignment, &pinned_policy)
            && !migration_carry_applies(&stored, &assignment, &pinned_policy)
        {
            return Ok(Gate::Closed(Verdict::Park("ACTIVATION_STALE")));
        }
        let activated_at = assignment.activation_confirmed_at.unwrap_or(0.0);
        let mut states = Vec::new();
        for file in &files {
            states.push(
                self.setup
                    .db
                    .read_blocking("management.track_state", |conn| {
                        Ok::<_, ManagementError>(store::track_state(conn, &file.id)?)
                    })?,
            );
        }
        let restored = states.iter().flatten().any(|state| {
            state.outcome.as_deref() == Some("restored")
                && state.managed_at.is_none_or(|at| at >= activated_at)
        });
        if restored {
            return Ok(Gate::Closed(Verdict::Park("RESTORED")));
        }
        let excluded = self.setup.db.read_blocking("management.excluded", |conn| {
            Ok::<_, ManagementError>(
                conn.query_row(
                    "SELECT 1 FROM library_management_exclusions WHERE local_album_id = ?1",
                    params![album_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some(),
            )
        })?;
        if excluded {
            return Ok(Gate::Closed(Verdict::Park("EXCLUDED")));
        }
        if files
            .iter()
            .any(|file| planner::keeps_own_tags(&registry, &file.root_id, &file.relative_path))
        {
            return Ok(Gate::Closed(Verdict::Park(reasons::KEEPS_OWN_TAGS.code)));
        }
        // The only place an automatic file write starts: an album the app
        // stepped back from waits here until a person hands it back.
        let held = self
            .setup
            .db
            .read_blocking("management.is_marked", |conn| {
                Ok::<_, ManagementError>(
                    takeover::is_marked(conn, album_id)?
                        && !(passing_hand_back && hand_back_stamped(conn, album_id)?),
                )
            })?;
        if held {
            return Ok(Gate::Closed(Verdict::Park(takeover::TAKEN_OVER)));
        }
        // A scan followed the album across a retag made outside the app,
        // over an edition someone chose. A record the app wrote since does
        // not cancel it: that write took the retagged bytes as they were.
        if self
            .setup
            .db
            .read_blocking("management.automatic_gate_retag_followed", |conn| {
                Ok::<_, ManagementError>(retag_followed(conn, album_id)?)
            })?
        {
            return Ok(Gate::Follow);
        }
        let Some(identity) = self.setup.identify_store.album_identity(album_id)? else {
            return Ok(Gate::NotYet(Verdict::Wait("IDENTITY_NOT_ACCEPTED")));
        };
        // A custom edition is managed automatically only where the folder
        // says so (v2's automatic_custom_editions).
        let custom = identity.release_mbid.is_none()
            && identity.release_group_mbid.is_some()
            && self
                .setup
                .db
                .read_blocking("management.custom_edition", |conn| {
                    Ok::<_, ManagementError>(super::custom::active(conn, album_id)?)
                })?
                .is_some();
        if custom && !assignment.automatic_custom_editions {
            return Ok(Gate::Closed(Verdict::Park("CUSTOM_EDITION")));
        }
        let identities = &self.setup.identify_store;
        if !custom && (identity.release_mbid.is_none() || identity.release_group_mbid.is_none()) {
            return Ok(Gate::Closed(Verdict::Park("RELEASE_NOT_SELECTED")));
        }
        let mut mapping = Vec::new();
        for file in &files {
            match identities.track_identity(&file.id)? {
                Some(track)
                    if track.recording_mbid.is_some() && track.release_track_mbid.is_some() =>
                {
                    mapping.push((file.id.clone(), track.row_revision));
                }
                // A custom edition's file may have no recording at all.
                track if custom => {
                    mapping.push((file.id.clone(), track.map_or(0, |track| track.row_revision)));
                }
                _ => return Ok(Gate::NotYet(Verdict::Wait("TRACK_NOT_MAPPED"))),
            }
        }
        let manage_unconfirmed = planner::manage_unconfirmed(&self.setup);
        let gate = self
            .setup
            .db
            .read_blocking("management.match_gate", |conn| {
                Ok::<_, ManagementError>(planner::match_gate(conn, album_id, manage_unconfirmed)?)
            })?;
        if gate.is_some() {
            // Confirming the match, or turning on "Also manage unconfirmed
            // matches", looks at the album again.
            return Ok(Gate::Closed(Verdict::Park("UNCONFIRMED_MATCH")));
        }
        // Last, so it stops only a look that would write: new files in a
        // folder the scan holds back may be a person's retag the takeover
        // checks cannot see yet. The album goes ahead once a scan settles
        // the folder.
        if self
            .setup
            .db
            .read_blocking("management.holds_new_files", |conn| {
                Ok::<_, ManagementError>(holds_new_files(conn, album_id)?)
            })?
        {
            return Ok(Gate::NotYet(Verdict::Park(SCAN_HELD_BACK)));
        }
        // A held album let go before it could be judged as a retag is
        // judged on the edition about to be written, before the first write.
        if self
            .setup
            .db
            .read_blocking("management.remembered", |conn| {
                Ok::<_, ManagementError>(remembered(conn, album_id)?)
            })?
        {
            return Ok(Gate::Judge { writes: true });
        }
        Ok(Gate::Open(Box::new(Opened {
            files,
            stored,
            assignment,
            origin,
            policy,
            states,
            identity,
            mapping,
        })))
    }

    /// Whether an album in a folder that does not write it by itself is
    /// judged as a retag at its look: a lifted hold let some of its files
    /// go unjudged, and its match names a release the match gate lets
    /// through.
    fn judged_by_hand(&self, album_id: &str) -> Result<bool, ManagementError> {
        if !self
            .setup
            .db
            .read_blocking("management.judged_by_hand_remembered", |conn| {
                Ok::<_, ManagementError>(remembered(conn, album_id)?)
            })?
        {
            return Ok(false);
        }
        let names_release = self
            .setup
            .identify_store
            .album_identity(album_id)?
            .is_some_and(|identity| identity.release_mbid.is_some());
        if !names_release {
            return Ok(false);
        }
        let manage_unconfirmed = planner::manage_unconfirmed(&self.setup);
        Ok(self
            .setup
            .db
            .read_blocking("management.judged_by_hand_match_gate", |conn| {
                Ok::<_, ManagementError>(planner::match_gate(conn, album_id, manage_unconfirmed)?)
            })?
            .is_none())
    }

    fn automatic_verdict(&self, album_id: &str) -> Result<Verdict, ManagementError> {
        let mut gate = self.automatic_gate(album_id, false)?;
        if let Gate::Judge { .. } = gate {
            if self.setup.db.write_blocking(
                Lane::Background,
                "management.judge_remembered",
                |tx| {
                    Ok::<_, ManagementError>(judge_remembered(
                        tx,
                        album_id,
                        self.setup.clock.now_secs(),
                    )?)
                },
            )? {
                return Ok(Verdict::Park(takeover::TAKEN_OVER));
            }
            // The album's remembered rows are gone, so the gate does not
            // ask again.
            gate = self.automatic_gate(album_id, false)?;
        }
        let opened = match gate {
            Gate::Closed(verdict) | Gate::NotYet(verdict) => return Ok(verdict),
            Gate::Follow => {
                self.setup.db.write_blocking(
                    Lane::Background,
                    "management.mark_followed",
                    |tx| {
                        Ok::<_, ManagementError>(takeover::mark_followed(
                            tx,
                            album_id,
                            self.setup.clock.now_secs(),
                        )?)
                    },
                )?;
                return Ok(Verdict::Park(takeover::TAKEN_OVER));
            }
            // Only a scan's end in between remembers rows again: looked at
            // again shortly.
            Gate::Judge { .. } => return Ok(Verdict::RetryAt(SCAN_HELD_BACK, 0.0)),
            Gate::Open(opened) => *opened,
        };
        let Opened {
            files,
            stored,
            assignment,
            origin,
            policy,
            states,
            identity,
            mapping,
        } = opened;
        let pinned = profile::pin_assignment(&stored, &assignment)?;
        let revisions = profile::revisions(&stored, &pinned, &policy);
        let sandbox = self.setup.publishing.publish_sandbox().ok();
        let as_revision = |value: u64| i64::try_from(value).ok();
        let managed =
            files
                .iter()
                .zip(&states)
                .zip(&mapping)
                .all(|((file, state), (_, track_revision))| {
                    state.as_ref().is_some_and(|state| {
                        state.outcome.as_deref() == Some("succeeded")
                    && state.profile_id.as_deref() == Some(pinned.profile.id.as_str())
                    && state.profile_revision.as_deref() == Some(revisions.profile.as_str())
                    && state.naming_revision.as_deref() == Some(revisions.naming.as_str())
                    && state.root_id.as_deref() == Some(file.root_id.as_str())
                    // The edition and the file's match it was written from.
                    && state.album_identity_revision == as_revision(identity.row_revision)
                    && state.track_identity_revision == as_revision(*track_revision)
                    // The file is still where, and as, the run left it.
                    && state.path_revision.is_some()
                    && state.path_revision
                        == sandbox.as_ref().and_then(|sandbox| {
                            path_revision(sandbox, &file.root_id, &file.relative_path)
                        })
                    && state.override_revision.as_deref()
                        == Some(self.override_revision(album_id, &file.id).to_string().as_str())
                    })
                });
        if managed {
            return Ok(Verdict::Done("ALREADY_MANAGED"));
        }
        // Credits and artist names are fetched before the run starts: an
        // album whose credits or names cannot be read now waits and is
        // looked at again by itself, rather than being planned without
        // them.
        if let Some(release_mbid) = identity.release_mbid.as_deref()
            && let Some(mut release) = self.setup.identify_store.release(release_mbid, None)?
            && super::enrich::ensure_release_extras(
                self.setup.enrichment.as_deref(),
                &self.setup.identify_store,
                &pinned.profile,
                &mut release,
                true,
            )
            .unavailable()
        {
            return Ok(Verdict::Wait("METADATA_UNAVAILABLE"));
        }
        // AcoustID ids the same way: an album whose ids cannot be looked
        // up now waits, rather than being written without them and never
        // planned again. No key, or a key AcoustID refuses, goes ahead.
        if let Some(sandbox) = sandbox.as_ref() {
            let saved = files
                .iter()
                .map(|file| {
                    self.setup.db.read_blocking("management.overrides", |conn| {
                        Ok(planner::overrides(conn, album_id, &file.id)?)
                    })
                })
                .collect::<Result<Vec<_>, ManagementError>>()?;
            let subjects: Vec<planner::AcoustIdSubject<'_>> = files
                .iter()
                .zip(&saved)
                .map(|(file, overrides)| planner::AcoustIdSubject {
                    track_id: &file.id,
                    root_id: &file.root_id,
                    relative_path: &file.relative_path,
                    overrides,
                })
                .collect();
            let lookup = planner::acoustid_values(
                &self.setup,
                sandbox,
                &pinned.profile,
                &subjects,
                std::time::Instant::now() + planner::RUN_ANALYSIS_BUDGET,
                true,
                &mut super::enrich::AlbumExtras::default(),
            );
            if lookup == planner::AcoustIdLookup::Unavailable {
                return Ok(Verdict::Wait("ACOUSTID_UNAVAILABLE"));
            }
        }
        let key_material = format!(
            "{album_id}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
            identity.row_revision,
            mapping
                .iter()
                .map(|(id, revision)| format!("{id}:{revision}"))
                .collect::<Vec<_>>()
                .join(","),
            files
                .iter()
                .map(|file| file.stat_revision.as_str())
                .collect::<Vec<_>>()
                .join(","),
            revisions.profile,
            revisions.settings,
            revisions.policy,
        );
        // A hand-back asks for a fresh look: no run made before it, done,
        // stepped back or waiting, answers for the album.
        let key_material = match self
            .setup
            .db
            .read_blocking("management.hand_backs", |conn| {
                Ok::<_, ManagementError>(takeover::hand_backs(conn, album_id)?)
            })? {
            0 => key_material,
            count => format!("{key_material}\u{0}handed-back:{count}"),
        };
        let mut key = format!("automatic:{}", sha256_hex(key_material.as_bytes()));
        if let Some(existing) = self
            .setup
            .db
            .read_blocking("management.job_for_key", |conn| {
                Ok::<_, ManagementError>(store::job_for_key(conn, &key)?)
            })?
        {
            let snapshot = self.snapshot(&existing)?;
            let now = self.setup.clock.now_secs();
            let expires = snapshot.preview_expires_at.unwrap_or(0.0);
            match (snapshot.job.state.as_str(), snapshot.phase.as_str()) {
                // A run for exactly this is waiting or working.
                ("queued" | "running" | "paused", _) => return Ok(Verdict::Done("ALREADY_QUEUED")),
                // A preview nothing blocks is still here only when the
                // database was too busy to start it: it starts now. A
                // blocked one waits for review until it expires.
                ("ready", "ready") if expires > now => {
                    if self.apply_automatic(&existing, &snapshot.summary())? {
                        return Ok(Verdict::Done("ALREADY_QUEUED"));
                    }
                    return Ok(Verdict::RetryAt("PREVIEW_BLOCKED", expires));
                }
                ("succeeded", _) => return Ok(Verdict::Done("ALREADY_QUEUED")),
                // Expired, failed, stopped or discarded: try once more
                // under a new key.
                _ => key = format!("{key}:{}", now as i64),
            }
        }
        let selection = Selection {
            kind: SelectionKind::Albums,
            ids: vec![album_id.to_owned()],
            catalog_filter: None,
        };
        let handle = self.start_job(JobSpec {
            mode: JobMode::Preview,
            origin,
            actor: None,
            idempotency_key: Some(&key),
            revisions,
            selection_json: serde_json::to_string(&selection)?,
            pinned: &pinned,
            target_root_id: None,
            linked_job_id: None,
            intent: JobIntent::default(),
            proposed_settings_revision: None,
            retention_hours: stored.preview_retention_hours,
        })?;
        if handle.existing {
            return Ok(Verdict::Done("ALREADY_QUEUED"));
        }
        Ok(Verdict::Started(handle.job_id))
    }
}
