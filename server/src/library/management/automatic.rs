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
//! at one only after the catalog has been still for a moment (so a scan
//! or an import has finished writing, and never right after a start),
//! and while no scan runs. An album it skips for a reason the person can
//! act on stays listed with that reason until it is queued again.

use rusqlite::{Connection, OptionalExtension as _, params};

use super::models::{JobIntent, JobMode, ManagementError, Origin, Selection, SelectionKind};
use super::reasons::{self, ItemReason};
use super::service::{JobSpec, Management};
use super::{planner, profile, store};
use crate::library::clock::now_unix;
use crate::library::publish::Sandbox;
use crate::library::publish::snapshots::sha256_hex;
use crate::settings::management::{activation_is_current, migration_carry_applies};

/// How long the catalog must be still before automatic management looks
/// at an album (v2's settle window).
pub const SETTLE_SECONDS: f64 = 30.0;
/// How many times an album that is not ready yet is looked at again.
const MAX_ATTEMPTS: i64 = 8;
/// First wait before looking again; it doubles each time, up to six hours.
const FIRST_RETRY_SECONDS: f64 = 60.0;
/// `not_before` of an album skipped until something changes.
const PARKED: f64 = 1.0e15;

/// The catalog revision the worker saw last, and when it last moved.
#[derive(Debug, Clone, Default)]
pub struct Settle {
    revision: Option<i64>,
    changed_at: Option<f64>,
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
        "UNCONFIRMED_MATCH" => reasons::unconfirmed_match(),
        "IDENTITY_NOT_ACCEPTED" => reasons::identity_not_accepted(),
        "TRACK_NOT_MAPPED" => reasons::track_not_mapped(),
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

impl crate::library::wiring::LibrarySetup {
    /// Queue an album for automatic management. A failure only logs:
    /// the album is then managed by hand, never left half done. Blocking.
    pub fn queue_automatic_management(&self, album_id: &str) {
        let queued = self
            .identify_store
            .with_connection(|conn| queue(conn, album_id, now_unix()));
        if let Err(error) = queued {
            tracing::warn!(%error, album_id, "album not queued for automatic management");
        }
    }
}

fn catalog_revision(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT value FROM library_catalog_revision WHERE singleton = 1",
        [],
        |row| row.get(0),
    )
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

impl Management {
    /// Look at the next waiting album once the catalog has settled and no
    /// scan runs. True when one was looked at. Blocking.
    pub fn automatic_step(&self, settle: &mut Settle) -> Result<bool, ManagementError> {
        if !self.setup.coordinator.current().is_empty() {
            return Ok(false);
        }
        let now = now_unix();
        let revision = self.read(|conn| Ok(catalog_revision(conn)?))?;
        // A start counts as a change: the first look waits the settle
        // window too, so a boot-time scan or import can begin first.
        if settle.revision != Some(revision) {
            settle.revision = Some(revision);
            settle.changed_at = Some(now);
            return Ok(false);
        }
        if settle
            .changed_at
            .is_some_and(|at| now - at < SETTLE_SECONDS)
        {
            return Ok(false);
        }
        if planner::manage_unconfirmed(&self.setup) {
            self.read(|conn| Ok(wake(conn, Some("UNCONFIRMED_MATCH"), now)?))?;
        }
        self.automatic_next()
    }

    /// Look at the next waiting album that is due. Blocking.
    pub fn automatic_next(&self) -> Result<bool, ManagementError> {
        let now = now_unix();
        let due: Option<(String, i64)> = self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT local_album_id, attempts FROM library_management_automatic_queue \
                     WHERE not_before <= ?1 ORDER BY not_before, local_album_id LIMIT 1",
                    params![now],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?)
        })?;
        let Some((album_id, attempts)) = due else {
            return Ok(false);
        };
        let verdict = self.automatic_verdict(&album_id).unwrap_or_else(|error| {
            tracing::warn!(%error, album_id, "automatic management could not look at an album");
            Verdict::Wait("MANAGEMENT_FAILED")
        });
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
            Verdict::Wait(code) if attempts + 1 >= MAX_ATTEMPTS => {
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
        self.read(|conn| {
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
            Ok(())
        })?;
        Ok(true)
    }

    /// Albums automatic management skipped, newest first. Blocking.
    pub fn automatic_skipped(&self, limit: i64) -> Result<Vec<SkippedAlbum>, ManagementError> {
        self.read(|conn| {
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

    fn automatic_verdict(&self, album_id: &str) -> Result<Verdict, ManagementError> {
        let files = self.read(|conn| Ok(album_files(conn, album_id)?))?;
        let Some(first) = files.first() else {
            return Ok(Verdict::Done("ALBUM_GONE"));
        };
        if files.iter().any(|file| file.root_id != first.root_id) {
            return Ok(Verdict::Park("SPANS_FOLDERS"));
        }
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
        let stored = self.settings()?;
        let assignment = profile::root_assignment(&stored, &first.root_id);
        let triggered = match origin {
            Origin::Acquisition => assignment.automatic_acquisitions,
            Origin::DropImport => assignment.automatic_drop_imports,
            _ => assignment.automatic_scan_discovered,
        };
        if !assignment.enabled || !triggered {
            return Ok(Verdict::Done("NOT_AUTOMATIC"));
        }
        let registry = self.setup.live_registry();
        let policy = registry.policy_revision().to_owned();
        let pinned_policy = self.activation_policy_revision()?;
        if !activation_is_current(&stored, &assignment, &pinned_policy)
            && !migration_carry_applies(&stored, &assignment, &pinned_policy)
        {
            return Ok(Verdict::Park("ACTIVATION_STALE"));
        }
        let activated_at = assignment.activation_confirmed_at.unwrap_or(0.0);
        let mut states = Vec::new();
        for file in &files {
            states.push(self.read(|conn| Ok(store::track_state(conn, &file.id)?))?);
        }
        let restored = states.iter().flatten().any(|state| {
            state.outcome.as_deref() == Some("restored")
                && state.managed_at.is_none_or(|at| at >= activated_at)
        });
        if restored {
            return Ok(Verdict::Park("RESTORED"));
        }
        let excluded = self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT 1 FROM library_management_exclusions WHERE local_album_id = ?1",
                    params![album_id],
                    |_| Ok(()),
                )
                .optional()?
                .is_some())
        })?;
        if excluded {
            return Ok(Verdict::Park("EXCLUDED"));
        }
        let identities = &self.setup.identify_store;
        let Some(identity) = identities.album_identity(album_id) else {
            return Ok(Verdict::Wait("IDENTITY_NOT_ACCEPTED"));
        };
        // A custom edition is managed automatically only where the folder
        // says so (v2's automatic_custom_editions).
        let custom = identity.release_mbid.is_none()
            && identity.release_group_mbid.is_some()
            && self
                .read(|conn| Ok(super::custom::active(conn, album_id)?))?
                .is_some();
        if custom && !assignment.automatic_custom_editions {
            return Ok(Verdict::Park("CUSTOM_EDITION"));
        }
        if !custom && (identity.release_mbid.is_none() || identity.release_group_mbid.is_none()) {
            return Ok(Verdict::Park("RELEASE_NOT_SELECTED"));
        }
        let mut mapping = Vec::new();
        for file in &files {
            match identities.track_identity(&file.id) {
                Some(track)
                    if track.recording_mbid.is_some() && track.release_track_mbid.is_some() =>
                {
                    mapping.push((file.id.clone(), track.row_revision));
                }
                // A custom edition's file may have no recording at all.
                track if custom => {
                    mapping.push((file.id.clone(), track.map_or(0, |track| track.row_revision)));
                }
                _ => return Ok(Verdict::Wait("TRACK_NOT_MAPPED")),
            }
        }
        let manage_unconfirmed = planner::manage_unconfirmed(&self.setup);
        let gate =
            self.read(|conn| Ok(planner::match_gate(conn, album_id, manage_unconfirmed)?))?;
        if gate.is_some() {
            // Confirming the match, or turning on "Also manage unconfirmed
            // matches", looks at the album again.
            return Ok(Verdict::Park("UNCONFIRMED_MATCH"));
        }
        let pinned = profile::pin_assignment(&stored, &assignment)?;
        let revisions = profile::revisions(&stored, &pinned, &policy);
        let sandbox = self.setup.publish_sandbox().ok();
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
        let mut key = format!("automatic:{}", sha256_hex(key_material.as_bytes()));
        if let Some(existing) = self.read(|conn| Ok(store::job_for_key(conn, &key)?))? {
            let snapshot = self.snapshot(&existing)?;
            let now = now_unix();
            let expires = snapshot.preview_expires_at.unwrap_or(0.0);
            match (snapshot.job.state.as_str(), snapshot.phase.as_str()) {
                // A run for exactly this is waiting or working.
                ("queued" | "running" | "paused", _) => return Ok(Verdict::Done("ALREADY_QUEUED")),
                // A blocked preview waits for review until it expires.
                ("ready", "ready") if expires > now => {
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
