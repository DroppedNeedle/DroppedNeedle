//! Automatic management: once an imported or newly identified album is
//! ready, run its library folder's profile on it without asking, as v2
//! did after a scan or an import. It only runs for folders whose
//! automatic switch is on and whose dry run is still current, never for
//! an album whose match is unconfirmed (unless "Also manage unconfirmed
//! matches" is on), and always from the album's chosen edition. A run
//! with a blocked file waits on the operations page with its reasons.
//!
//! Albums wait in `library_management_automatic_queue`. The worker looks
//! at one only after the catalog has been still for a moment (so a scan
//! or an import has finished writing), and while no scan runs.

use rusqlite::{Connection, OptionalExtension as _, params};

use super::models::{JobIntent, JobMode, ManagementError, Origin, Selection, SelectionKind};
use super::planner;
use super::profile;
use super::service::{JobSpec, Management};
use super::store;
use crate::library::clock::now_unix;
use crate::library::identify::stores::IdentityStore as _;
use crate::library::publish::snapshots::sha256_hex;
use crate::settings::management::{activation_is_current, migration_carry_applies};

/// How long the catalog must be still before automatic management looks
/// at an album (v2's settle window).
pub const SETTLE_SECONDS: f64 = 30.0;
/// How many times an album that is not ready yet is looked at again.
const MAX_ATTEMPTS: i64 = 8;
/// First wait before looking again; it doubles each time, up to six hours.
const FIRST_RETRY_SECONDS: f64 = 60.0;

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
    /// Nothing to do now or later; the code says why (for the log).
    Done(&'static str),
    /// Not ready yet; look again later.
    Wait(&'static str),
}

/// Queue one album for automatic management (an import landed, or its
/// identification settled). A queued album is looked at again from the
/// start.
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

/// One indexed file of an album: id, folder, how it arrived, revision.
struct AlbumFile {
    id: String,
    root_id: String,
    ingest_source: String,
    stat_revision: String,
}

fn album_files(conn: &Connection, album_id: &str) -> rusqlite::Result<Vec<AlbumFile>> {
    let mut statement = conn.prepare_cached(
        "SELECT id, root_id, ingest_source, stat_revision FROM local_tracks \
         WHERE local_album_id = ?1 AND availability = 'indexed' ORDER BY id",
    )?;
    let rows = statement.query_map(params![album_id], |row| {
        Ok(AlbumFile {
            id: row.get(0)?,
            root_id: row.get(1)?,
            ingest_source: row.get(2)?,
            stat_revision: row.get(3)?,
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
        if settle.revision != Some(revision) {
            if settle.revision.is_some() {
                settle.changed_at = Some(now);
            }
            settle.revision = Some(revision);
            if settle.changed_at.is_some() {
                return Ok(false);
            }
        }
        if settle
            .changed_at
            .is_some_and(|at| now - at < SETTLE_SECONDS)
        {
            return Ok(false);
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
        let waiting = match verdict {
            Verdict::Started(job_id) => {
                tracing::info!(album_id, job_id, "automatic management started");
                None
            }
            Verdict::Done(code) => {
                tracing::debug!(album_id, code, "automatic management has nothing to do");
                None
            }
            Verdict::Wait(code) if attempts + 1 >= MAX_ATTEMPTS => {
                tracing::info!(
                    album_id,
                    code,
                    "automatic management gave up waiting for an album"
                );
                None
            }
            Verdict::Wait(code) => Some(code),
        };
        self.read(|conn| {
            match waiting {
                None => conn.execute(
                    "DELETE FROM library_management_automatic_queue WHERE local_album_id = ?1",
                    params![album_id],
                )?,
                Some(code) => {
                    let delay =
                        (FIRST_RETRY_SECONDS * 2f64.powi(attempts as i32)).min(6.0 * 3600.0);
                    conn.execute(
                        "UPDATE library_management_automatic_queue SET attempts = attempts + 1, \
                         not_before = ?2, last_reason_code = ?3 WHERE local_album_id = ?1",
                        params![album_id, now + delay, code],
                    )?
                }
            };
            Ok(())
        })?;
        Ok(true)
    }

    fn automatic_verdict(&self, album_id: &str) -> Result<Verdict, ManagementError> {
        let files = self.read(|conn| Ok(album_files(conn, album_id)?))?;
        let Some(first) = files.first() else {
            return Ok(Verdict::Done("ALBUM_GONE"));
        };
        if files.iter().any(|file| file.root_id != first.root_id) {
            return Ok(Verdict::Done("SPANS_FOLDERS"));
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
        if !activation_is_current(&stored, &assignment, &policy)
            && !migration_carry_applies(&stored, &assignment, &policy)
        {
            tracing::info!(
                album_id,
                root = first.root_id,
                "automatic management waits: the folder's dry run is out of date"
            );
            return Ok(Verdict::Done("ACTIVATION_STALE"));
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
            return Ok(Verdict::Done("RESTORED"));
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
            return Ok(Verdict::Done("EXCLUDED"));
        }
        let identities = &self.setup.identify_store;
        let Some(identity) = identities.album_identity(album_id) else {
            return Ok(Verdict::Wait("IDENTITY_NOT_ACCEPTED"));
        };
        if identity.release_mbid.is_none() || identity.release_group_mbid.is_none() {
            return Ok(Verdict::Done("RELEASE_NOT_SELECTED"));
        }
        let mut mapping = Vec::new();
        for file in &files {
            match identities.track_identity(&file.id) {
                Some(track)
                    if track.recording_mbid.is_some() && track.release_track_mbid.is_some() =>
                {
                    mapping.push(format!("{}:{}", file.id, track.row_revision));
                }
                _ => return Ok(Verdict::Wait("TRACK_NOT_MAPPED")),
            }
        }
        let manage_unconfirmed = planner::manage_unconfirmed(&self.setup);
        let gate =
            self.read(|conn| Ok(planner::match_gate(conn, album_id, manage_unconfirmed)?))?;
        if gate.is_some() {
            // Confirming the match (or turning on "Also manage unconfirmed
            // matches") queues the album again.
            return Ok(Verdict::Done("UNCONFIRMED_MATCH"));
        }
        let pinned = profile::pin_assignment(&stored, &assignment)?;
        let revisions = profile::revisions(&stored, &pinned, &policy);
        let managed = files.iter().zip(&states).all(|(file, state)| {
            state.as_ref().is_some_and(|state| {
                state.outcome.as_deref() == Some("succeeded")
                    && state.profile_id.as_deref() == Some(pinned.profile.id.as_str())
                    && state.profile_revision.as_deref() == Some(revisions.profile.as_str())
                    && state.naming_revision.as_deref() == Some(revisions.naming.as_str())
                    && state.root_id.as_deref() == Some(file.root_id.as_str())
                    && state.override_revision.as_deref()
                        == Some(
                            self.override_revision(album_id, &file.id)
                                .to_string()
                                .as_str(),
                        )
            })
        });
        if managed {
            return Ok(Verdict::Done("ALREADY_MANAGED"));
        }
        let key_material = format!(
            "{album_id}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}\u{0}{}",
            identity.row_revision,
            mapping.join(","),
            files
                .iter()
                .map(|file| file.stat_revision.as_str())
                .collect::<Vec<_>>()
                .join(","),
            revisions.profile,
            revisions.settings,
            revisions.policy,
        );
        let key = format!("automatic:{}", sha256_hex(key_material.as_bytes()));
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
