//! `GET /api/v3/admin/support-bundle`: one JSON document an admin reads and
//! then attaches to a bug report.
//!
//! It says how the server is set up and what is wrong, never whose data it
//! holds. The types do the work: no value in the bundle is a free `String`.
//! Every text is an enum, a `&'static str` the server names (a section key,
//! a service key, a folder role), or a [`SafeToken`], whose one constructor
//! keeps only a short plain token. So a path, a host, a name, an email, a
//! secret, a message or a cause cannot reach it: none of those fit the
//! shapes, and nothing else is read into it. Recent problems come in the
//! redacted form the ring kept when they were logged.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{Json, extract::State};
use serde::Serialize;
use utoipa::ToSchema;

use super::sql;
use crate::admin::problems::Problems;
use droppedneedle_auth::Admin;
use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_kernel::disk::{DiskLevel, FolderRole};
use droppedneedle_kernel::error::ApiError;
use droppedneedle_kernel::jobs::{JobState, queue};
use droppedneedle_kernel::problems_ring::{FieldValue, ProblemLevel, is_minted_id, is_safe_token};

/// The bundle's size cap; recent problems are trimmed, newest kept, to fit.
pub const BUNDLE_MAX_BYTES: usize = 1024 * 1024;
/// How long the bundle's local reads may take.
const LOCAL_READS_BOUND: Duration = Duration::from_secs(5);

/// A short plain token: letters, digits, `_`, `.`, `:` and `-`, at most 64.
/// Its one constructor checks the shape, so no path, host, address, name,
/// sentence or secret is ever one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(transparent)]
pub struct SafeToken(String);

impl SafeToken {
    /// `value` when it is a plain token.
    pub fn new(value: &str) -> Option<Self> {
        is_safe_token(value).then(|| Self(value.to_owned()))
    }

    /// A file as the ring cut it (`<crate>/src/...`): a plain token per
    /// part, joined by `/`, and nothing absolute.
    fn file(value: &str) -> Option<Self> {
        let parts_ok = !value.starts_with('/')
            && value.len() <= 128
            && value
                .split('/')
                .all(|part| is_safe_token(part) && part != "..");
        parts_ok.then(|| Self(value.to_owned()))
    }

    /// A ref with the minted id's shape.
    fn reference(value: &str) -> Option<Self> {
        is_minted_id(value).then(|| Self(value.to_owned()))
    }

    /// A Linux kernel release cut to its numbers (`6.12.117`).
    fn kernel_release(value: &str) -> Option<Self> {
        let numbers: String = value
            .trim()
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        let parts: Vec<&str> = numbers.trim_end_matches('.').split('.').collect();
        let ok = (2..=3).contains(&parts.len())
            && parts.iter().all(|part| !part.is_empty() && part.len() <= 4);
        ok.then(|| Self(parts.join(".")))
    }

    /// A plugin version: `^[0-9A-Za-z.+-]{1,32}$`.
    fn version(value: &str) -> Option<Self> {
        let ok = (1..=32).contains(&value.len())
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'+' | b'-'));
        ok.then(|| Self(value.to_owned()))
    }
}

/// The support bundle.
#[derive(Debug, Serialize, ToSchema)]
pub struct SupportBundle {
    /// The server's version.
    pub version: &'static str,
    /// The image's tag, when it is a plain token.
    pub commit_tag: Option<SafeToken>,
    /// When the image was built, when that is a plain token.
    pub build_date: Option<SafeToken>,
    /// The CPU architecture.
    pub arch: &'static str,
    /// The operating system.
    pub os: &'static str,
    /// The Linux kernel release, numbers only.
    pub kernel_release: Option<SafeToken>,
    /// Whether the server runs in a container.
    pub container: bool,
    /// CPUs the server may use.
    pub cpus: u64,
    /// Memory the machine has, in bytes.
    pub memory_bytes: Option<u64>,
    /// Each filesystem the app writes, by the role of the folders on it.
    pub folders: Vec<BundleFolder>,
    /// Each settings section's state, never its values.
    pub settings: Vec<BundleSection>,
    /// The download clients.
    pub download_sources: Vec<BundleSource>,
    /// The background loops.
    pub loops: Vec<BundleLoop>,
    /// Queued work by job kind.
    pub queues: Vec<BundleQueue>,
    /// Outside services not answering now, by key.
    pub degraded_services: Vec<&'static str>,
    /// The database's files.
    pub database: BundleDatabase,
    /// How much the server holds, as counts.
    pub counts: BundleCounts,
    /// Plugins an admin enabled.
    pub plugins: Vec<BundlePlugin>,
    /// Recent warnings and errors, redacted, newest first.
    pub problems: Vec<BundleProblem>,
    /// Recent problems left out to keep the bundle under 1 MiB.
    pub problems_left_out: u64,
}

/// One folder the app writes.
#[derive(Debug, Serialize, ToSchema)]
pub struct BundleFolder {
    /// What it is to the app.
    pub role: &'static str,
    /// For a library folder, which one: 1 for the first.
    pub library_number: Option<u32>,
    /// Its filesystem's kind (`ext4`, `nfs`, ...).
    pub filesystem: &'static str,
    /// Whether that filesystem is local, not a network share.
    pub local: bool,
    /// Free bytes, when the last check read them.
    pub free_bytes: Option<u64>,
    /// The filesystem's size, when read.
    pub total_bytes: Option<u64>,
    /// How that compares with what the app needs there.
    pub level: BundleDiskLevel,
}

/// A filesystem's room.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BundleDiskLevel {
    /// Enough room.
    Ok,
    /// Running low.
    Low,
    /// Nearly full.
    Critical,
    /// The last check could not read it.
    Unknown,
}

/// One settings section.
#[derive(Debug, Serialize, ToSchema)]
pub struct BundleSection {
    /// The section's key.
    pub key: &'static str,
    /// Its state.
    pub state: BundleSectionState,
}

/// What is stored for a section.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BundleSectionState {
    /// Never saved: its defaults apply.
    Default,
    /// Saved, and it reads.
    Saved,
    /// Saved, and it does not read.
    Unreadable,
}

/// One download source: a built-in client or a plugin's.
#[derive(Debug, Serialize, ToSchema)]
pub struct BundleSource {
    /// Which: the client, or the plugin's id.
    pub name: SafeToken,
    /// A plugin's download source.
    pub plugin: bool,
    /// Turned on.
    pub enabled: bool,
    /// Set up.
    pub configured: bool,
    /// Its last check answered.
    pub reachable: bool,
}

/// One background loop.
#[derive(Debug, Serialize, ToSchema)]
pub struct BundleLoop {
    /// Its name.
    pub name: SafeToken,
    /// What it is doing.
    pub state: BundleLoopState,
    /// Seconds since its current run of failures began, while it fails.
    pub failing_for_secs: Option<u64>,
    /// Its last failure's fixed code, when it has one.
    pub last_error: Option<SafeToken>,
}

/// A loop's state.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BundleLoopState {
    /// Running.
    Running,
    /// Stopped cleanly.
    Stopped,
    /// Ended failed.
    Failed,
}

/// Queued work of one job kind.
#[derive(Debug, Serialize, ToSchema)]
pub struct BundleQueue {
    /// The job kind.
    pub kind: SafeToken,
    /// Waiting.
    pub queued: u64,
    /// Running.
    pub running: u64,
    /// Failed for good in the last 24 hours.
    pub failed_last_day: u64,
}

/// The database's files.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct BundleDatabase {
    /// The database file's size.
    pub db_bytes: Option<u64>,
    /// Its write-ahead log's size.
    pub wal_bytes: Option<u64>,
    /// The provider cache's size.
    pub cache_bytes: Option<u64>,
    /// The database's schema version.
    pub schema_version: i64,
    /// Backups in the backups folder.
    pub backup_count: Option<u64>,
    /// Seconds since the newest nightly backup.
    pub newest_nightly_backup_age_secs: Option<u64>,
}

/// How much the server holds.
#[derive(Debug, Default, Serialize, ToSchema)]
pub struct BundleCounts {
    /// Accounts by role.
    pub users_by_role: BTreeMap<&'static str, u64>,
    /// Artists in the library.
    pub artists: u64,
    /// Albums in the library.
    pub albums: u64,
    /// Tracks in the library.
    pub tracks: u64,
    /// Playlists.
    pub playlists: u64,
    /// Requests by state.
    pub requests_by_state: BTreeMap<SafeToken, u64>,
    /// Files kept for a person's decision.
    pub held_entries: u64,
}

/// One enabled plugin.
#[derive(Debug, Serialize, ToSchema)]
pub struct BundlePlugin {
    /// Its id.
    pub id: SafeToken,
    /// Its version; null when the manifest's is not a plain version.
    pub version: Option<SafeToken>,
}

/// One recent problem, redacted.
#[derive(Debug, Serialize, ToSchema)]
pub struct BundleProblem {
    /// Its level.
    pub level: BundleProblemLevel,
    /// The module that logged it.
    pub target: SafeToken,
    /// Its file, cut to its crate.
    pub file: Option<SafeToken>,
    /// Its line.
    pub line: Option<u32>,
    /// How many times it was seen.
    pub count: u64,
    /// Seconds since it was first seen.
    pub first_secs_ago: u64,
    /// Seconds since it was last seen.
    pub last_secs_ago: u64,
    /// Refs of requests it was seen in, the minted ones only.
    pub refs: Vec<SafeToken>,
    /// Refs of another shape, counted.
    pub other_refs: u32,
    /// The allowlisted fields.
    pub fields: BTreeMap<&'static str, BundleValue>,
}

/// A recent problem's level.
#[derive(Debug, Clone, Copy, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BundleProblemLevel {
    /// A warning.
    Warn,
    /// An error.
    Error,
}

/// An allowlisted field's value.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(untagged)]
pub enum BundleValue {
    /// A plain token.
    Token(SafeToken),
    /// A number.
    Number(f64),
    /// A switch.
    Switch(bool),
}

/// What the bundle reads off the disk, on a blocking thread.
#[derive(Default)]
struct LocalReads {
    kernel_release: Option<SafeToken>,
    container: bool,
    memory_bytes: Option<u64>,
    db_bytes: Option<u64>,
    wal_bytes: Option<u64>,
    cache_bytes: Option<u64>,
    backup_count: Option<u64>,
    plugins: Vec<BundlePlugin>,
    plugin_sources: Vec<BundleSource>,
}

fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|meta| meta.len())
}

fn memory_total() -> Option<u64> {
    let text = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = text.lines().find(|line| line.starts_with("MemTotal:"))?;
    let kib: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    kib.checked_mul(1024)
}

fn local_reads(
    db_path: PathBuf,
    cache_path: PathBuf,
    host: Arc<droppedneedle_plugins::host::PluginHost>,
    backups: Arc<droppedneedle_kernel::db::BackupService>,
) -> LocalReads {
    let mut wal = db_path.clone().into_os_string();
    wal.push("-wal");
    let loaded: Vec<_> = host
        .list_plugins()
        .into_iter()
        .filter(|plugin| plugin.enabled)
        .filter(|plugin| droppedneedle_plugins::manifest::valid_plugin_name(&plugin.manifest.name))
        .collect();
    let plugin_sources = loaded
        .iter()
        .filter(|plugin| {
            plugin
                .active_capabilities
                .contains(&droppedneedle_plugins::manifest::Capability::DownloadClient)
        })
        .filter_map(|plugin| {
            Some(BundleSource {
                name: SafeToken::new(&plugin.manifest.name)?,
                plugin: true,
                enabled: true,
                configured: true,
                reachable: plugin.runtime.is_some(),
            })
        })
        .collect();
    let backup_count = crate::admin::backups::list_backups(backups.backup_dir(), false)
        .ok()
        .and_then(|listed| u64::try_from(listed.backups.len()).ok());
    let plugins = loaded
        .into_iter()
        .filter_map(|plugin| {
            Some(BundlePlugin {
                id: SafeToken::new(&plugin.manifest.name)?,
                version: SafeToken::version(&plugin.manifest.version),
            })
        })
        .collect();
    LocalReads {
        kernel_release: std::fs::read_to_string("/proc/sys/kernel/osrelease")
            .ok()
            .and_then(|text| SafeToken::kernel_release(&text)),
        container: Path::new("/.dockerenv").exists() || Path::new("/run/.containerenv").exists(),
        memory_bytes: memory_total(),
        db_bytes: file_size(&db_path),
        wal_bytes: file_size(Path::new(&wal)),
        cache_bytes: file_size(&cache_path),
        backup_count,
        plugins,
        plugin_sources,
    }
}

fn redacted_value(value: &FieldValue) -> Option<BundleValue> {
    match value {
        FieldValue::Text(text) => SafeToken::new(text).map(BundleValue::Token),
        FieldValue::Int(number) => Some(BundleValue::Number(*number as f64)),
        FieldValue::Float(number) => number.is_finite().then_some(BundleValue::Number(*number)),
        FieldValue::Bool(on) => Some(BundleValue::Switch(*on)),
    }
}

fn level_of(level: DiskLevel) -> BundleDiskLevel {
    match level {
        DiskLevel::Ok => BundleDiskLevel::Ok,
        DiskLevel::Low => BundleDiskLevel::Low,
        DiskLevel::Critical => BundleDiskLevel::Critical,
        DiskLevel::Unknown => BundleDiskLevel::Unknown,
    }
}

fn role_of(role: FolderRole) -> &'static str {
    match role {
        FolderRole::Data => "data",
        FolderRole::Config => "config",
        FolderRole::Temp => "temp",
        FolderRole::Imports => "imports",
        FolderRole::Library => "library",
        FolderRole::RecycleBin => "recycle_bin",
        FolderRole::Downloads => "downloads",
        FolderRole::ImportFolder => "import_folder",
    }
}

impl Problems {
    /// The support bundle as it stands now.
    pub async fn support_bundle(&self) -> SupportBundle {
        let now_ms = self.supervisor.clock().now_ms();
        let host = Arc::clone(&self.plugins);
        let (db_path, cache_path) = (self.db_path.clone(), self.cache_path.clone());
        let backups = Arc::clone(&self.backups);
        let local = tokio::time::timeout(
            LOCAL_READS_BOUND,
            tokio::task::spawn_blocking(move || local_reads(db_path, cache_path, host, backups)),
        )
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();

        let folders = self
            .disk
            .latest()
            .map(|sample| {
                let mut library = 0u32;
                sample
                    .filesystems
                    .iter()
                    .flat_map(|fs| fs.folders.iter().map(move |folder| (fs, folder)))
                    .map(|(fs, folder)| {
                        let library_number = (folder.role == FolderRole::Library).then(|| {
                            library += 1;
                            library
                        });
                        BundleFolder {
                            role: role_of(folder.role),
                            library_number,
                            filesystem: fs.kind,
                            local: fs.local,
                            free_bytes: fs.free,
                            total_bytes: fs.total,
                            level: level_of(fs.level),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        let settings = self
            .settings
            .section_states()
            .into_iter()
            .map(|(key, saved, unreadable)| BundleSection {
                key,
                state: if unreadable {
                    BundleSectionState::Unreadable
                } else if saved {
                    BundleSectionState::Saved
                } else {
                    BundleSectionState::Default
                },
            })
            .collect();

        let probes = self.probes.snapshot();
        let source = |name: &str, probe: &droppedneedle_acquire::imports::health::ClientProbe| {
            BundleSource {
                name: SafeToken(name.to_owned()),
                plugin: false,
                enabled: probe.enabled,
                configured: probe.configured,
                reachable: probe.reachable,
            }
        };
        let download_sources = vec![
            source("slskd", &probes.slskd),
            source("sabnzbd", &probes.sabnzbd.status),
            source("nzbget", &probes.nzbget.status),
            BundleSource {
                name: SafeToken("deemix".to_owned()),
                plugin: false,
                enabled: probes.deemix.enabled,
                configured: probes.deemix.configured,
                reachable: probes.deemix.reachable,
            },
        ]
        .into_iter()
        .chain(local.plugin_sources)
        .collect::<Vec<_>>();

        let loops = self
            .supervisor
            .loops()
            .into_iter()
            .filter_map(|record| {
                Some(BundleLoop {
                    name: SafeToken::new(&record.name)?,
                    state: match record.state {
                        JobState::Running => BundleLoopState::Running,
                        JobState::Stopped => BundleLoopState::Stopped,
                        JobState::Failed => BundleLoopState::Failed,
                    },
                    failing_for_secs: record.failing_since.map(|since| {
                        u64::try_from(now_ms.saturating_sub(since) / 1000).unwrap_or(0)
                    }),
                    last_error: record.last_error.as_deref().and_then(SafeToken::new),
                })
            })
            .collect();

        let failed: BTreeMap<String, u64> =
            queue::failed_since(&self.db, now_ms.saturating_sub(24 * 60 * 60 * 1000))
                .await
                .unwrap_or_default()
                .into_iter()
                .collect();
        let queues = queue::counts(&self.db)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter_map(|counts| {
                Some(BundleQueue {
                    failed_last_day: failed.get(&counts.kind).copied().unwrap_or(0),
                    kind: SafeToken::new(&counts.kind)?,
                    queued: counts.queued,
                    running: counts.running,
                })
            })
            .collect();

        let degraded_services = self
            .health
            .current(Instant::now())
            .into_iter()
            .map(|degraded| degraded.service)
            .collect();

        let newest_nightly_backup_age_secs = self
            .newest_nightly_at()
            .await
            .map(|at| u64::try_from(now_ms.saturating_sub(at) / 1000).unwrap_or(0));

        let mut bundle = SupportBundle {
            version: env!("CARGO_PKG_VERSION"),
            commit_tag: SafeToken::new(&self.commit_tag),
            build_date: self.build_date.as_deref().and_then(SafeToken::new),
            arch: std::env::consts::ARCH,
            os: std::env::consts::OS,
            kernel_release: local.kernel_release,
            container: local.container,
            cpus: std::thread::available_parallelism()
                .map_or(1, |n| u64::try_from(n.get()).unwrap_or(1)),
            memory_bytes: local.memory_bytes,
            folders,
            settings,
            download_sources,
            loops,
            queues,
            degraded_services,
            database: BundleDatabase {
                db_bytes: local.db_bytes,
                wal_bytes: local.wal_bytes,
                cache_bytes: local.cache_bytes,
                schema_version: droppedneedle_kernel::schema::latest_version(),
                backup_count: local.backup_count,
                newest_nightly_backup_age_secs,
            },
            counts: bundle_counts(&self.db).await,
            plugins: local.plugins,
            problems: Vec::new(),
            problems_left_out: 0,
        };
        bundle.problems = self.bundle_problems();
        trim_to_cap(&mut bundle);
        bundle
    }

    /// The recent problems in the form the ring kept for the bundle.
    fn bundle_problems(&self) -> Vec<BundleProblem> {
        let now = Instant::now();
        let secs = |at: Instant| now.saturating_duration_since(at).as_secs();
        self.recent
            .entries(None)
            .into_iter()
            .filter_map(|entry| {
                let redacted = entry.redacted;
                Some(BundleProblem {
                    level: match entry.level {
                        ProblemLevel::Warn => BundleProblemLevel::Warn,
                        ProblemLevel::Error => BundleProblemLevel::Error,
                    },
                    target: SafeToken::new(entry.target)?,
                    file: redacted.file.as_deref().and_then(SafeToken::file),
                    line: entry.line,
                    count: entry.count,
                    first_secs_ago: secs(entry.first),
                    last_secs_ago: secs(entry.last),
                    refs: redacted
                        .refs
                        .iter()
                        .filter_map(|r| SafeToken::reference(r))
                        .collect(),
                    other_refs: redacted.other_refs,
                    fields: redacted
                        .fields
                        .iter()
                        .filter_map(|(name, value)| Some((*name, redacted_value(value)?)))
                        .collect(),
                })
            })
            .collect()
    }
}

/// The bundle's counts, each from one read. A count that does not read is
/// left at zero and logged.
async fn bundle_counts(db: &Db) -> BundleCounts {
    let mut counts = BundleCounts::default();
    match db
        .read("support.library-counts", |conn| {
            conn.query_row(sql::BUNDLE_COUNTS.text(), [], sql::BundleCountsRow::read)
                .map_err(DbError::from)
        })
        .await
    {
        Ok(row) => {
            let count = |value: i64| u64::try_from(value).unwrap_or(0);
            counts.artists = count(row.artists);
            counts.albums = count(row.albums);
            counts.tracks = count(row.tracks);
            counts.playlists = count(row.playlists);
            counts.held_entries = count(row.held_entries);
        }
        Err(error) => tracing::warn!(%error, "support bundle: library counts not read"),
    }
    match grouped(
        db,
        "support.users-by-role",
        sql::BUNDLE_USERS_BY_ROLE.text(),
    )
    .await
    {
        Ok(rows) => {
            for (role, total) in rows {
                let role = match role.as_str() {
                    "admin" => "admin",
                    "trusted" => "trusted",
                    "user" => "user",
                    _ => continue,
                };
                counts.users_by_role.insert(role, total);
            }
        }
        Err(error) => tracing::warn!(%error, "support bundle: accounts not counted"),
    }
    match grouped(
        db,
        "support.requests-by-state",
        sql::BUNDLE_REQUESTS_BY_STATE.text(),
    )
    .await
    {
        Ok(rows) => {
            for (state, total) in rows {
                if let Some(state) = SafeToken::new(&state) {
                    counts.requests_by_state.insert(state, total);
                }
            }
        }
        Err(error) => tracing::warn!(%error, "support bundle: requests not counted"),
    }
    counts
}

/// A `name, count` statement's rows.
async fn grouped(
    db: &Db,
    name: &'static str,
    text: &'static str,
) -> Result<Vec<(String, u64)>, DbError> {
    db.read(name, move |conn| {
        let mut statement = conn.prepare_cached(text)?;
        let rows = statement.query_map([], sql::GroupCountRow::read)?;
        let mut out = Vec::new();
        for row in rows {
            let row = row?;
            out.push((row.name, u64::try_from(row.total).unwrap_or(0)));
        }
        Ok(out)
    })
    .await
}

/// Drop the oldest recent problems until the bundle fits [`BUNDLE_MAX_BYTES`].
fn trim_to_cap(bundle: &mut SupportBundle) {
    while serde_json::to_vec(bundle).map_or(0, |bytes| bytes.len()) > BUNDLE_MAX_BYTES {
        if bundle.problems.pop().is_none() {
            break;
        }
        bundle.problems_left_out += 1;
    }
}

/// The support bundle: how this server is set up and what is wrong, with
/// no settings values, paths, hosts, names, library contents or message
/// text. An admin reads it before it is downloaded.
#[utoipa::path(
    get,
    tag = "admin",
    path = "/api/v3/admin/support-bundle",
    responses(
        (status = 200, description = "The support bundle", body = SupportBundle),
        (status = 401, description = "Not authenticated"),
        (status = 403, description = "Admin role required"),
    )
)]
pub async fn support_bundle(
    State(problems): State<Problems>,
    _caller: Admin,
) -> Result<Json<SupportBundle>, ApiError> {
    Ok(Json(problems.support_bundle().await))
}
