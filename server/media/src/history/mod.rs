//! Each person's listening history from ListenBrainz, kept on the server.
//!
//! A person uploads an export ZIP they downloaded from ListenBrainz; the
//! upload lands in the cache folder and one [`import::KIND`] job, keyed by
//! the person, reads it a month file at a time into
//! `media_imported_listens`, then deletes it. `media_history_imports`
//! holds one row a person saying where that stands.
//!
//! An imported listen is history for taste and for "have you heard this":
//! it is never a play. Nothing here writes `library_play_history`, and no
//! play count, scrobble or statistic reads the imported table. A listen in
//! the same second as one of the person's plays here is that play coming
//! back from ListenBrainz, and is not kept.
//!
//! Every write of an import checks, inside its own transaction, that the
//! person's row still reads the same file at the same place. [`forget`]
//! moves the row to `forgotten` in the write that deletes the listens and
//! cancels the person's jobs, so an import claimed before a forget writes
//! nothing after it, and nothing is imported again until they upload.

pub mod archive;
pub mod import;
pub(crate) mod sql;
pub mod store;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

pub use store::{ForgetError, HistoryStatus, HistoryStore};

/// Where a person's import stands. Declared whole: `pending` and
/// `exporting` are first written by the export fetched from ListenBrainz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HistoryImportState {
    /// An export is asked for and not started yet. Not written yet.
    Pending,
    /// ListenBrainz is making the export. Not written yet.
    Exporting,
    /// The file is being read.
    Importing,
    /// Everything the file held is kept.
    Current,
    /// The file could not be read.
    Failed,
    /// The person forgot their history: nothing is imported until they ask.
    Forgotten,
}

droppedneedle_kernel::spelled!(HistoryImportState {
    Pending => "pending",
    Exporting => "exporting",
    Importing => "importing",
    Current => "current",
    Failed => "failed",
    Forgotten => "forgotten",
});

/// Where an import's file came from. Declared whole: `export` is first
/// written by the export fetched from ListenBrainz.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum HistoryImportSource {
    /// A file the person uploaded.
    Upload,
    /// An export fetched with the person's own link. Not written yet.
    Export,
}

droppedneedle_kernel::spelled!(HistoryImportSource {
    Upload => "upload",
    Export => "export",
});

/// Where the caller's listening history stands.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ListeningHistory {
    /// Where their import stands; null while they never imported.
    pub state: Option<HistoryImportState>,
    /// Where the last import's file came from; null while none.
    pub source: Option<HistoryImportSource>,
    /// The ListenBrainz account the history is from; null for an upload.
    pub account: Option<String>,
    /// Listens kept from ListenBrainz. Never counted as plays.
    pub imported_listens: i64,
    /// Plays here, in the web player and in apps.
    pub plays: i64,
    /// The oldest listen kept, unix seconds.
    pub oldest_listened_at: Option<i64>,
    /// The newest listen kept, unix seconds.
    pub newest_listened_at: Option<i64>,
    /// When the last import finished, unix seconds.
    pub last_synced_at: Option<i64>,
}

impl From<HistoryStatus> for ListeningHistory {
    fn from(status: HistoryStatus) -> Self {
        let secs = |at: Option<i64>| at.map(|ms| ms.div_euclid(1000));
        Self {
            state: status.state,
            source: status.source,
            account: status.account,
            imported_listens: status.listens,
            plays: status.plays,
            oldest_listened_at: secs(status.oldest_listened_at),
            newest_listened_at: secs(status.newest_listened_at),
            last_synced_at: secs(status.last_synced_at),
        }
    }
}

/// The upload form: one `file` part, a ListenBrainz export ZIP.
#[derive(Debug, ToSchema)]
pub struct ListeningHistoryUpload {
    /// The export ZIP.
    #[schema(value_type = String, format = Binary)]
    pub file: Vec<u8>,
}

/// Forget `user_id`'s imported history: their listens go, their import
/// is marked `forgotten` and their queued and running imports are
/// cancelled, in one write; then the files waiting to be read are deleted.
/// Answers how many listens went. Safe to run again.
pub async fn forget(store: &HistoryStore, user_id: &str) -> Result<usize, ForgetError> {
    store.forget(user_id).await
}
