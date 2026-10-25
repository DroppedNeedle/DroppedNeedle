//! Download dispatch: the one way into the download queue.
//!
//! Requests, the flow loops, upgrades and editions all start, cancel and
//! read download tasks through [`DownloadDispatch`] over the durable
//! [`Journal`]: one task-id mint, one insert path, one origin vocabulary.
//! An album that already has a live task gets no second one: the call
//! answers [`DispatchOutcome::Active`] with that task.
//!
//! Task ids are 32 lowercase hex chars (a UUID without dashes): the
//! orphan reconciler's `job_name_parts` only recognises that shape, so
//! anything else would make debris invisible to the sweep.

use std::sync::Arc;

use super::downloads::journal::Journal;
use super::downloads::plan::{PlannedAlbum, TaskPlan, TrackPosition};
use super::downloads::state::TaskStatus;
use super::downloads::store::{NewTask, StoreError, TaskDetails, TaskRow};
use super::downloads::watchdog::RetryPolicy;
use super::edition::chosen_edition;
use droppedneedle_kernel::db::DbError;
use droppedneedle_kernel::ids::IdGenerator;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::DownloadPolicy;

/// What started a download. Stored as the task's `origin`; origins decide
/// quota exemptions and how a landing treats the files, never routing.
/// A request row stores it too, and the Requests page shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DispatchOrigin {
    /// A fresh ask from intake.
    User,
    /// An admin approval releasing a waiting ask.
    Approval,
    /// A retry of a terminal ask (quota-exempt: already admitted once).
    Retry,
    /// A wanted-watch redispatch (quota-exempt for the same reason).
    Wanted,
    /// An upgrade of owned files (size-neutral, quota-exempt).
    Upgrade,
    /// "Get missing tracks" for an album's chosen edition.
    Edition,
    /// A new release of a followed artist, auto-downloaded.
    Follow,
}

impl DispatchOrigin {
    /// Every origin, in the order `request_history.origin`'s CHECK lists them.
    pub const ALL: [Self; 7] = [
        Self::User,
        Self::Approval,
        Self::Retry,
        Self::Wanted,
        Self::Upgrade,
        Self::Edition,
        Self::Follow,
    ];

    /// The text stored in `download_tasks.origin` and `request_history.origin`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Approval => "approval",
            Self::Retry => "retry",
            Self::Wanted => "wanted",
            Self::Upgrade => "upgrade",
            Self::Edition => "edition",
            Self::Follow => "follow",
        }
    }

    /// Read back a stored origin.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "approval" => Some(Self::Approval),
            "retry" => Some(Self::Retry),
            "wanted" => Some(Self::Wanted),
            "upgrade" => Some(Self::Upgrade),
            "edition" => Some(Self::Edition),
            "follow" => Some(Self::Follow),
            _ => None,
        }
    }
}

/// What a download fetches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchTarget {
    /// A whole album (an edition fill is one too).
    Album {
        /// Release-group MBID.
        release_group_mbid: String,
    },
    /// One track; it is searched for as part of its album.
    Track {
        /// Recording MBID.
        recording_mbid: String,
        /// The person chose to take the song from its album: the album's
        /// track imports as this recording when it is the right track.
        album_version: bool,
    },
}

/// One download to start.
#[derive(Debug, Clone)]
pub struct DispatchRequest {
    /// Primary owner id (immutable request attribution, never the actor).
    pub user_id: String,
    /// What to fetch.
    pub target: DispatchTarget,
    /// Artist name.
    pub artist_name: String,
    /// Album or track title.
    pub title: String,
    /// What started this download.
    pub origin: DispatchOrigin,
    /// Edition release id, when pinned.
    pub release_mbid: Option<String>,
    /// For an album on `release_mbid`: only these (disc, position) tracks
    /// are fetched, from the best whole-album source. Empty fetches the
    /// whole album.
    pub wanted: Vec<(u32, u32)>,
    /// For an album on `release_mbid`: the library copy it is fetched
    /// for, which the files then join.
    pub planned_album: Option<PlannedAlbum>,
    /// Caller-supplied idempotency key. A repeat dispatch with the same
    /// key answers the original task instead of minting a duplicate;
    /// `None` always mints fresh.
    pub idempotency_key: Option<String>,
}

/// How a dispatch call resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// This call started the task (or a repeat of it answered the
    /// original).
    Dispatched {
        /// Download task id.
        task_id: String,
    },
    /// The album already has a live task; nothing new was queued. The
    /// task belongs to another ask, so the caller never cancels it.
    Active {
        /// The live task's id.
        task_id: String,
    },
}

impl DispatchOutcome {
    /// The task now fetching the target.
    pub fn task_id(&self) -> &str {
        match self {
            Self::Dispatched { task_id } | Self::Active { task_id } => task_id,
        }
    }

    /// Whether this call started the task.
    pub fn started(&self) -> bool {
        matches!(self, Self::Dispatched { .. })
    }
}

/// Dispatch failures. Busy is retryable and never marks work failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DispatchError {
    /// The database was busy; try again later.
    #[error("database is busy during {operation}")]
    Busy {
        /// Operation name for the log line.
        operation: String,
    },
    /// The journal failed (the message stays out of responses).
    #[error("{0}")]
    Failed(String),
}

impl From<StoreError> for DispatchError {
    fn from(error: StoreError) -> Self {
        match error {
            StoreError::Db(DbError::Busy { operation }) => Self::Busy { operation },
            StoreError::Db(DbError::LaneClosed) => Self::Busy {
                operation: "downloads".to_owned(),
            },
            other => Self::Failed(other.to_string()),
        }
    }
}

/// One task as callers outside the downloads module read it.
#[derive(Debug, Clone)]
pub struct TaskView {
    /// Task id.
    pub id: String,
    /// Current status.
    pub status: TaskStatus,
    /// Progress percent (0-100) the worker last reported.
    pub progress_percent: i64,
    /// Total transfer bytes, once known.
    pub total_size_bytes: Option<i64>,
    /// Bytes transferred so far.
    pub downloaded_bytes: i64,
    /// Last outcome text.
    pub error_message: Option<String>,
    /// Picked candidate quality (format label), once picked.
    pub quality: Option<String>,
    /// Fetch source (`soulseek`, `usenet`, `plugin:<key>`).
    pub protocol: String,
}

impl From<TaskRow> for TaskView {
    fn from(row: TaskRow) -> Self {
        Self {
            id: row.id,
            status: row.status,
            progress_percent: row.progress_percent,
            total_size_bytes: row.total_size_bytes,
            downloaded_bytes: row.downloaded_bytes,
            error_message: row.error_message,
            quality: row.quality_format,
            protocol: row.source,
        }
    }
}

/// One scheduled auto-retry for a failed fetch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrySchedule {
    /// Retries already spent.
    pub retry_count: u32,
    /// Retries the ladder allows.
    pub max_attempts: u32,
    /// Epoch seconds when the next retry is due.
    pub next_retry_at: u64,
}

/// The download dispatch over the shared journal.
pub struct DownloadDispatch {
    journal: Arc<Journal>,
    ids: Arc<dyn IdGenerator>,
    policy: Live<DownloadPolicy>,
}

impl DownloadDispatch {
    /// Wire dispatch over a shared journal. `policy` holds the auto-retry
    /// policy for the wanted view's retrying rows.
    pub fn new(
        journal: Arc<Journal>,
        ids: Arc<dyn IdGenerator>,
        policy: Live<DownloadPolicy>,
    ) -> Self {
        Self {
            journal,
            ids,
            policy,
        }
    }

    /// Shared journal, for the worker and the source adapters.
    pub fn journal(&self) -> &Arc<Journal> {
        &self.journal
    }

    /// Mint a 32-hex task id.
    fn mint_task_id(&self) -> String {
        self.ids.new_id().replace('-', "").to_lowercase()
    }

    /// Start one download. Track rows key on the recording MBID; album
    /// rows on the release-group MBID (a pinned release rides in the row
    /// and its plan for the library importer). The live-task check, the
    /// idempotency claim and the insert with its plan share one
    /// transaction: a repeat dispatch either finds the original task or
    /// nothing at all, never a key without a task, and two asks for one
    /// album never both queue.
    pub async fn dispatch(
        &self,
        request: &DispatchRequest,
    ) -> Result<DispatchOutcome, DispatchError> {
        let (is_track, key) = match &request.target {
            DispatchTarget::Album { release_group_mbid } => (false, release_group_mbid.as_str()),
            DispatchTarget::Track { recording_mbid, .. } => (true, recording_mbid.as_str()),
        };
        let release_mbid = request.release_mbid.as_deref();
        // Wanted positions and the copy fetched for belong to the caller's
        // release; they mean nothing on an edition picked here.
        let on_release = release_mbid.is_some() && !is_track;
        let wanted: Vec<TrackPosition> = if on_release {
            request
                .wanted
                .iter()
                .map(|&(disc, track)| TrackPosition { disc, track })
                .collect()
        } else {
            Vec::new()
        };
        let planned_album = request.planned_album.clone().filter(|_| on_release);
        let task_id = self.mint_task_id();
        let now = self.journal.db().now_unix_f64();
        // An album ask without a release fetches the edition the library
        // has chosen for that album (a single track resolves its album
        // when it is searched for).
        let chosen = match release_mbid {
            None if !is_track => match chosen_edition(self.journal.db().db(), key).await {
                Ok(chosen) => chosen.map(|edition| edition.release_mbid),
                Err(error) => {
                    tracing::warn!(key, %error, "chosen edition unreadable; dispatching the album");
                    None
                }
            },
            _ => None,
        };
        let release_mbid = release_mbid.or(chosen.as_deref());
        let (release_group_mbid, recording_mbid) = if is_track {
            (String::new(), key.to_owned())
        } else {
            (key.to_owned(), String::new())
        };
        let origin = request.origin.as_str();
        let task = NewTask {
            id: task_id.clone(),
            user_id: request.user_id.clone(),
            artist_name: request.artist_name.clone(),
            album_title: request.title.clone(),
            release_group_mbid,
            origin: origin.to_owned(),
            retry_count: 0,
        };
        let details = TaskDetails {
            release_mbid: release_mbid.map(str::to_owned),
            track_title: is_track.then(|| request.title.clone()),
            ..TaskDetails::default()
        };
        let plan = TaskPlan {
            release_mbid: release_mbid.map(str::to_owned),
            album_wanted: wanted,
            planned_album,
            album_version: matches!(
                request.target,
                DispatchTarget::Track {
                    album_version: true,
                    ..
                }
            ),
            ..TaskPlan::default()
        };
        let namespaced = request
            .idempotency_key
            .as_deref()
            .map(|caller_key| format!("dispatch:{caller_key}"));
        enum Inserted {
            New,
            Found(DispatchOutcome),
            KeyWithoutTask,
        }
        let inserted = self
            .journal
            .run_foreground("downloads.dispatch", move |store| {
                if !is_track
                    && let Some(live) = store.newest_active_for_album(&task.release_group_mbid)?
                {
                    return Ok(Inserted::Found(DispatchOutcome::Active {
                        task_id: live.id,
                    }));
                }
                if let Some(key) = &namespaced
                    && !store.claim_key(key, &task.id, "dispatch", now)?
                {
                    // Repeat dispatch: answer the original task, never a twin.
                    return Ok(match store.task_id_for_key(key)? {
                        Some(task_id) => Inserted::Found(DispatchOutcome::Dispatched { task_id }),
                        None => Inserted::KeyWithoutTask,
                    });
                }
                if is_track {
                    store.insert_track_task(&task, &recording_mbid, now)?;
                } else {
                    store.insert_task(&task, now)?;
                }
                store.set_task_details(&task.id, &details, now)?;
                store.set_task_plan(&task.id, &plan, now)?;
                Ok(Inserted::New)
            })
            .await?;
        match inserted {
            Inserted::Found(outcome) => Ok(outcome),
            Inserted::KeyWithoutTask => Err(DispatchError::Failed(
                "duplicate dispatch; original task unknown".to_owned(),
            )),
            Inserted::New => Ok(DispatchOutcome::Dispatched { task_id }),
        }
    }

    /// Cancel one task: it goes terminal and its live attempts move to
    /// cleanup with a discard disposition in the same transaction, so the
    /// worker's cleanup pass aborts the transfer and discards the client
    /// record. Unknown or finished ids are a no-op success.
    pub async fn cancel(&self, task_id: &str) -> Result<(), DispatchError> {
        let task_id = task_id.to_owned();
        let clock = self.journal.db().clock().clone();
        self.journal
            .run_foreground("downloads.cancel", move |store| {
                store
                    .cancel_task(&task_id, crate::db::unix_secs_f64(clock.as_ref()))
                    .map(|_| ())
            })
            .await
            .map_err(DispatchError::from)
    }

    /// One task, or `None` for an unknown id. Read on the read pool.
    pub async fn task(&self, task_id: &str) -> Result<Option<TaskView>, DispatchError> {
        Ok(self.journal.read_task(task_id).await?.map(TaskView::from))
    }

    /// The newest live task for an album, across users.
    pub async fn active_task_for_album(
        &self,
        release_group_mbid: &str,
    ) -> Result<Option<TaskView>, DispatchError> {
        Ok(self
            .journal
            .read_active_for_album(release_group_mbid)
            .await?
            .map(TaskView::from))
    }

    /// Whether one task can be reimported (failed or short-landed with
    /// its candidate still linked).
    pub async fn reimportable(&self, task_id: &str) -> Result<bool, DispatchError> {
        Ok(self.journal.read_reimportable(task_id).await?)
    }

    /// Whether the task's newest landing held a single because only its
    /// album's track was found.
    pub async fn album_version_only(&self, task_id: &str) -> Result<bool, DispatchError> {
        let code = self.journal.read_hold_code(task_id).await?;
        Ok(code == Some(crate::reason::Reason::AlbumVersionOnly))
    }

    /// The pending auto-retry for one failed task, or `None` when no retry
    /// is scheduled (auto-retry off, ladder exhausted, or not failed).
    pub async fn retry_schedule(
        &self,
        task_id: &str,
    ) -> Result<Option<RetrySchedule>, DispatchError> {
        let Some(policy) = self.policy.read() else {
            return Ok(None);
        };
        let policy = RetryPolicy::from_policy(&policy);
        if policy.auto_retry_max() == 0 {
            return Ok(None);
        }
        // The newest task for its target is the one the retry sweep acts
        // on; once a successor exists this task is no longer waiting on a
        // retry.
        let Some(row) = self.journal.read_retry_anchor(task_id).await? else {
            return Ok(None);
        };
        let retry_count = u32::try_from(row.retry_count).unwrap_or(0);
        let anchor = row.completed_at.unwrap_or(row.updated_at);
        Ok(policy
            .next_retry_at(retry_count, anchor, &row.status)
            .map(|at| RetrySchedule {
                retry_count,
                max_attempts: policy.max_attempts,
                next_retry_at: at.max(0.0) as u64,
            }))
    }

    /// The newest task one owner started for an album or recording
    /// (`kind` is `album` or `track`) at or after `since` (epoch seconds).
    /// Startup recovery uses it to relink a request whose task was created
    /// but never linked.
    pub async fn find_task_since(
        &self,
        owner: &str,
        kind: &str,
        key: &str,
        since: u64,
    ) -> Result<Option<String>, DispatchError> {
        Ok(self
            .journal
            .read_task_since(owner, kind, key, since as f64)
            .await?)
    }
}
