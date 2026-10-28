//! SABnzbd wire models: stringly-typed queue
//! numbers, real-number history bytes, opaque `nzo_id`. Every reply the
//! client reads decodes into one of these; a job row without its
//! `nzo_id` or name, or a category without its name, fails decoding
//! instead of matching as an empty id.
//!
//! Shapes were confirmed against the owner's SABnzbd 5.0.4 under v2 (the
//! `version`, `get_cats` and `get_config` answers are written down; the
//! run's date is not). Each model was checked again on 2026-10-21 against
//! the in-repo `SabnzbdMock` in the testkit crate, which serves those 5.0.4
//! answers.

use serde::Deserialize;

/// `mode=addfile` / `addurl` response. Empty `nzo_ids` means rejection,
/// so a missing list reads as one rather than as a made-up id.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct AddResponse {
    /// Whether SABnzbd accepted the add.
    #[serde(default)]
    pub status: bool,
    /// Job ids created, usually exactly one.
    #[serde(default)]
    pub nzo_ids: Vec<String>,
}

/// Accept SABnzbd's stringly numbers (`"100.0"`) and real numbers alike.
fn de_stringly<'de, D>(value: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::{self, Visitor};

    struct Stringly;

    impl Visitor<'_> for Stringly {
        type Value = String;

        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("a string or number")
        }

        fn visit_str<E: de::Error>(self, value: &str) -> Result<String, E> {
            Ok(value.to_owned())
        }

        fn visit_string<E: de::Error>(self, value: String) -> Result<String, E> {
            Ok(value)
        }

        fn visit_i64<E: de::Error>(self, value: i64) -> Result<String, E> {
            Ok(value.to_string())
        }

        fn visit_u64<E: de::Error>(self, value: u64) -> Result<String, E> {
            Ok(value.to_string())
        }

        fn visit_f64<E: de::Error>(self, value: f64) -> Result<String, E> {
            Ok(value.to_string())
        }

        fn visit_bool<E: de::Error>(self, value: bool) -> Result<String, E> {
            Ok(value.to_string())
        }
    }

    value.deserialize_any(Stringly)
}

/// One in-progress job. `filename` is the job name; `mb`/`mbleft` are
/// decimal megabytes as strings; `priority` is a name on read.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct QueueSlot {
    /// Opaque job id (a plain UUID on 5.0.4). Required.
    pub nzo_id: String,
    /// `Downloading`, `Queued`, `Paused`, `Fetching`, and post states.
    #[serde(default)]
    pub status: String,
    /// Job name (`droppedneedle-{task_id}`). Required.
    pub filename: String,
    /// Category name.
    #[serde(default)]
    pub cat: String,
    /// Total megabytes, stringly-typed.
    #[serde(default, deserialize_with = "de_stringly")]
    pub mb: String,
    /// Remaining megabytes, stringly-typed.
    #[serde(default, deserialize_with = "de_stringly")]
    pub mbleft: String,
    /// Percent complete, int-as-string.
    #[serde(default, deserialize_with = "de_stringly")]
    pub percentage: String,
    /// Human time-left estimate.
    #[serde(default)]
    pub timeleft: String,
    /// Priority name (`Normal`/`High`/…).
    #[serde(default)]
    pub priority: String,
}

/// `mode=queue` payload.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Queue {
    /// Overall state.
    #[serde(default)]
    pub status: String,
    /// Whether the queue is paused.
    #[serde(default)]
    pub paused: bool,
    /// In-progress jobs.
    #[serde(default)]
    pub slots: Vec<QueueSlot>,
}

/// One finished / post-processing / failed job. `storage` is the final
/// folder in SABnzbd's namespace; `bytes` is already bytes. Debug is
/// hand-written: the NZB password must never appear in debug output.
#[derive(Clone, Default, Deserialize)]
pub struct HistorySlot {
    /// Opaque job id. Required.
    pub nzo_id: String,
    /// Job name. Required.
    pub name: String,
    /// Source NZB filename.
    #[serde(default)]
    pub nzb_name: String,
    /// `Completed`, `Failed`, `Verifying`, `Extracting`, ….
    #[serde(default)]
    pub status: String,
    /// Category name.
    #[serde(default)]
    pub category: String,
    /// Final folder (SABnzbd namespace).
    #[serde(default)]
    pub storage: String,
    /// Total bytes (a real number).
    #[serde(default)]
    pub bytes: u64,
    /// Failure detail on `Failed`.
    #[serde(default)]
    pub fail_message: String,
    /// Password used, when any.
    #[serde(default)]
    pub password: Option<String>,
    /// Download duration in seconds.
    #[serde(default)]
    pub download_time: u64,
    /// Completion timestamp.
    #[serde(default)]
    pub completed: u64,
}

impl std::fmt::Debug for HistorySlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HistorySlot")
            .field("nzo_id", &self.nzo_id)
            .field("name", &self.name)
            .field("nzb_name", &self.nzb_name)
            .field("status", &self.status)
            .field("category", &self.category)
            .field("storage", &self.storage)
            .field("bytes", &self.bytes)
            .field("fail_message", &self.fail_message)
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("download_time", &self.download_time)
            .field("completed", &self.completed)
            .finish()
    }
}

/// `mode=history` payload.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct History {
    /// Finished jobs.
    #[serde(default)]
    pub slots: Vec<HistorySlot>,
}

/// One SABnzbd category row (`get_config`'s `categories`).
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Category {
    /// Category name: what a job is filed under. Required.
    pub name: String,
    /// Category folder.
    #[serde(default)]
    pub dir: String,
    /// Post-processing setting.
    #[serde(default)]
    pub pp: String,
}

/// `config.misc`: the mount-remap prefix lives here.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Misc {
    /// Completed-downloads dir (the remap prefix).
    #[serde(default)]
    pub complete_dir: String,
}

/// `mode=get_config` payload.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Config {
    /// Misc settings incl. `complete_dir`.
    #[serde(default)]
    pub misc: Misc,
    /// Category rows.
    #[serde(default)]
    pub categories: Vec<Category>,
}

/// `mode=version` reply.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct VersionReply {
    /// Server version string.
    pub version: String,
}

/// One category name; SABnzbd may send a number.
#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub(crate) struct CategoryName(#[serde(deserialize_with = "de_stringly")] pub String);

/// `mode=get_cats` reply.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct CategoriesReply {
    /// Category names.
    #[serde(default)]
    pub categories: Vec<CategoryName>,
}

/// `mode=get_config` reply.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct ConfigReply {
    /// The config block.
    pub config: Config,
}

/// `mode=queue` reply.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct QueueReply {
    /// The queue block.
    pub queue: Queue,
}

/// `mode=history` reply.
#[derive(Debug, Clone, Deserialize)]
pub(crate) struct HistoryReply {
    /// The history block.
    pub history: History,
}

/// Reply to a delete: `status` is `true`, `false`, or `"False"`.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct StatusReply {
    /// The status flag as text; absent reads as success.
    #[serde(default, deserialize_with = "de_stringly")]
    pub status: String,
}

impl StatusReply {
    /// True unless SABnzbd answered false.
    pub fn ok(&self) -> bool {
        !self.status.trim().eq_ignore_ascii_case("false")
    }
}
