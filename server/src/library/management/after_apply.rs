//! What happens after an album is written: the files' management state
//! (so automatic management knows they are done), the overrides a tag
//! edit asked to keep, and the media servers told to rescan when the job
//! ends.

use rusqlite::OptionalExtension as _;

use super::models::{JobMode, ManagementError};
use super::service::Management;
use super::store::{self, ManagedMark, Snapshot};
use super::takeover;
use crate::library::clock::now_unix;
use crate::runtime_config::secret_sections::JellyfinConnection;

impl Management {
    /// Bookkeeping for one album the job just wrote. The files are
    /// already written; a failure here is logged and leaves them as they
    /// are (the next run sees them as not managed yet and plans them
    /// again, which writes nothing new).
    /// Returns what to add to the album's result: for a tag edit, each
    /// file's override revision before and after, so the edit's own
    /// override never blocks its undo.
    pub(super) fn after_album(
        &self,
        snapshot: &Snapshot,
        ordinal: i64,
    ) -> Option<serde_json::Value> {
        match self.record_album(snapshot, ordinal) {
            Ok(extra) => extra,
            Err(error) => {
                tracing::error!(
                    %error,
                    job_id = snapshot.job.id,
                    ordinal,
                    "management state not recorded after a written album"
                );
                None
            }
        }
    }

    fn record_album(
        &self,
        snapshot: &Snapshot,
        ordinal: i64,
    ) -> Result<Option<serde_json::Value>, ManagementError> {
        let job_id = snapshot.job.id.as_str();
        let items = self.read(|conn| Ok(store::bundle_items(conn, job_id, ordinal)?))?;
        let intent = snapshot.intent();
        let mode = snapshot.job_mode();
        let now = now_unix();
        let mut extra = None;
        if let Some(edit) = &intent.tag_edit {
            self.write(|tx| {
                Ok(store::save_tag_edit(
                    tx,
                    edit,
                    job_id,
                    snapshot.job.requested_by_user_id.as_deref(),
                    now,
                )?)
            })?;
            let revisions: serde_json::Map<String, serde_json::Value> = items
                .iter()
                .filter_map(|item| {
                    let plan = item.desired.plan.as_ref()?;
                    let after = self.override_revision(&item.album_id, &plan.track_id);
                    Some((
                        plan.track_id.clone(),
                        serde_json::json!([after, plan.override_revision]),
                    ))
                })
                .collect();
            extra = Some(serde_json::json!({ "override_revisions": revisions }));
        }
        if mode == Some(JobMode::Undo)
            && let Some(source) = snapshot.linked_job_id.as_deref()
            && let Some(source) = self.read(|conn| Ok(store::snapshot(conn, source)?))?
        {
            if let Some(edit) = source.intent().tag_edit {
                // Undoing a tag edit also puts its kept values back.
                self.write(|tx| Ok(store::revert_tag_edit(tx, &edit, job_id, now)?))?;
            } else if let Some(album_id) = items.first().map(|item| item.album_id.as_str()) {
                self.run_undone(&source, album_id, Some(job_id), now)?;
            }
        }
        let written = items.iter().filter(|item| item.eligibility.writes());
        match mode {
            Some(JobMode::BaselineRestore) => self.write(|tx| {
                for item in written {
                    if let Some(track_id) = &item.track_id {
                        store::mark_restored(tx, track_id, job_id, now)?;
                    }
                }
                Ok(())
            }),
            // A profile run: remember what it applied. Tag edits and
            // duplicate resolutions apply no whole profile, and an undo
            // puts the old state back.
            Some(JobMode::Apply | JobMode::AutomaticApply) if intent.tag_edit.is_none() => {
                let (profile_id, _) = snapshot.profile_label();
                let sandbox = self.setup.publish_sandbox().ok();
                let identities = &self.setup.identify_store;
                struct Mark {
                    track_id: String,
                    root: String,
                    override_revision: u64,
                    album_identity: Option<u64>,
                    track_identity: Option<u64>,
                    path: Option<String>,
                    found: Option<String>,
                }
                let marks: Vec<Mark> = written
                    .filter_map(|item| {
                        let track_id = item.track_id.clone()?;
                        let root = item.dest_root.clone().unwrap_or(item.source_root.clone());
                        let rel = item.dest_rel.clone().unwrap_or(item.source_rel.clone());
                        Some(Mark {
                            override_revision: self.override_revision(&item.album_id, &track_id),
                            album_identity: identities
                                .album_identity(&item.album_id)
                                .map(|identity| identity.row_revision),
                            track_identity: identities
                                .track_identity(&track_id)
                                .map(|identity| identity.row_revision),
                            path: sandbox.as_ref().and_then(|sandbox| {
                                super::automatic::path_revision(sandbox, &root, &rel)
                            }),
                            // A file with no plan of its own was left as
                            // the plan read it.
                            found: (item.desired.plan.is_none() && !item.fingerprint.is_empty())
                                .then(|| item.fingerprint.clone()),
                            track_id,
                            root,
                        })
                    })
                    .collect();
                self.write(|tx| {
                    for mark in &marks {
                        store::mark_managed(
                            tx,
                            &ManagedMark {
                                track_id: &mark.track_id,
                                root_id: &mark.root,
                                profile_id: &profile_id,
                                profile_revision: &snapshot.profile_revision,
                                naming_revision: &snapshot.naming_revision,
                                override_revision: mark.override_revision,
                                job_id,
                                album_identity_revision: mark.album_identity,
                                track_identity_revision: mark.track_identity,
                                path_revision: mark.path.clone(),
                                found_sha256: mark.found.as_deref(),
                            },
                            now,
                        )?;
                    }
                    Ok(())
                })
            }
            _ => Ok(()),
        }?;
        Ok(extra)
    }

    /// A person undid `source`'s run on the album. When that was a run of
    /// the profile in a folder managed without asking, the files are the
    /// app's own bytes again and automatic management would do the same
    /// run over, so the album is left to the person.
    fn run_undone(
        &self,
        source: &Snapshot,
        album_id: &str,
        job_id: Option<&str>,
        now: f64,
    ) -> Result<(), ManagementError> {
        let profile_run = matches!(
            source.job_mode(),
            Some(JobMode::Apply | JobMode::AutomaticApply)
        ) && source.intent().tag_edit.is_none();
        if profile_run && self.managed_automatically(album_id)? {
            self.write(|tx| {
                Ok(takeover::mark(
                    tx,
                    album_id,
                    takeover::Cause::RunUndone,
                    &[],
                    job_id,
                    now,
                )?)
            })?;
        }
        Ok(())
    }

    /// The same for one bundle undone on its own, outside a job (the
    /// route that undoes a single bundle). Blocking.
    pub fn bundle_undone(&self, bundle_id: &str) -> Result<(), ManagementError> {
        let found: Option<(String, i64)> = self.read(|conn| {
            Ok(conn
                .query_row(
                    "SELECT job_id, ordinal FROM library_operation_work \
                     WHERE json_extract(result_json, '$.bundle_id') = ?1",
                    [bundle_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?)
        })?;
        let Some((source_id, ordinal)) = found else {
            return Ok(());
        };
        let Some(source) = self.read(|conn| Ok(store::snapshot(conn, &source_id)?))? else {
            return Ok(());
        };
        let items = self.read(|conn| Ok(store::bundle_items(conn, &source_id, ordinal)?))?;
        match items.first() {
            Some(item) => self.run_undone(&source, &item.album_id, None, now_unix()),
            None => Ok(()),
        }
    }

    /// Ask the media servers to rescan once a job that wrote files ends,
    /// when the profile and the settings say so (as v2 did: Jellyfin is
    /// told; Plex and Navidrome have no refresh call and say so).
    pub(super) fn queue_external_refreshes(&self, snapshot: &Snapshot) {
        let wants = Self::pinned(snapshot)
            .is_ok_and(|pinned| pinned.profile.notification.refresh_external_servers);
        if !wants {
            return;
        }
        let settings = match self.settings() {
            Ok(settings) => settings.external_refresh,
            Err(error) => {
                tracing::warn!(%error, "media server refresh skipped: settings unreadable");
                return;
            }
        };
        if !settings.enabled {
            return;
        }
        let mut targets = Vec::new();
        if settings.jellyfin_enabled {
            targets.push("jellyfin");
        }
        if settings.plex_enabled {
            targets.push("plex");
        }
        if settings.navidrome_enabled {
            targets.push("navidrome");
        }
        if targets.is_empty() {
            return;
        }
        let job_id = snapshot.job.id.as_str();
        let queued = self.write(|tx| {
            Ok(store::queue_refreshes(
                tx,
                job_id,
                &targets,
                settings.retry_attempts.saturating_add(1),
                settings.retry_delay_seconds,
                now_unix(),
            )?)
        });
        if let Err(error) = queued {
            tracing::warn!(%error, job_id, "media server refresh not queued");
        }
    }

    /// Send the next media-server refresh that is due. True when one was
    /// sent (or settled). Blocking: it waits on the HTTP call.
    pub fn deliver_refresh(&self, worker: &str) -> Result<bool, ManagementError> {
        let Some(due) = self.write(|tx| Ok(store::claim_refresh(tx, worker, now_unix())?))? else {
            return Ok(false);
        };
        let outcome = match due.target.as_str() {
            "jellyfin" => self.refresh_jellyfin(),
            // v2 had no refresh call for these either.
            _ => Err("NOT_CONFIGURED"),
        };
        if let Err(code) = outcome {
            tracing::info!(
                target = due.target,
                code,
                "media server refresh did not go through"
            );
        }
        self.write(|tx| Ok(store::finish_refresh(tx, &due, outcome, now_unix())?))?;
        Ok(true)
    }

    fn refresh_jellyfin(&self) -> Result<(), &'static str> {
        let Some(http) = self.setup.http.clone() else {
            return Err("NOT_CONFIGURED");
        };
        let connection = self
            .setup
            .config
            .get_raw::<JellyfinConnection>()
            .map_err(|_| "NOT_CONFIGURED")?;
        let key = connection.api_key.expose().to_owned();
        if !connection.enabled || key.is_empty() || connection.jellyfin_url.is_empty() {
            return Err("NOT_CONFIGURED");
        }
        let base = connection.jellyfin_url.trim().trim_end_matches('/');
        let url = format!("{base}/Library/Refresh");
        let handle = tokio::runtime::Handle::try_current().map_err(|_| "UNAVAILABLE")?;
        let response = handle.block_on(async move {
            http.post(url)
                .header("X-Emby-Token", key)
                .timeout(std::time::Duration::from_secs(15))
                .send()
                .await
        });
        match response {
            Ok(response) if response.status().is_success() => Ok(()),
            Ok(response) if matches!(response.status().as_u16(), 401 | 403) => Err("AUTH_FAILED"),
            Ok(_) => Err("SERVER_ERROR"),
            Err(_) => Err("UNREACHABLE"),
        }
    }
}
