//! Rows of the held list: entries and their files, read and written on
//! a connection the caller holds, so a file's resolution and its entry's
//! commit in one transaction.

use std::collections::HashSet;

use droppedneedle_kernel::db::statement::ListLen;
use rusqlite::{Connection, OptionalExtension as _, Row, params};
use serde::Serialize;
use utoipa::ToSchema;

use super::sql;
use crate::reason::Reason;

/// How long an entry is kept after it was made, or after its newest file
/// arrived: 90 days.
pub const KEEP_MS: i64 = 90 * 86_400_000;

/// The gaps between automatic retries of an entry whose files our side
/// could not take: 5 and 15 minutes, half an hour, then 1, 2, 4, 8 and 24
/// hours, then daily until the entry expires.
const RETRY_GAPS_MS: [i64; 8] = [
    5 * 60_000,
    15 * 60_000,
    30 * 60_000,
    3_600_000,
    2 * 3_600_000,
    4 * 3_600_000,
    8 * 3_600_000,
    24 * 3_600_000,
];

/// The gap before the next retry, after `tries` retries in a row.
pub fn retry_gap(tries: i64) -> i64 {
    let step = usize::try_from(tries).unwrap_or(0);
    RETRY_GAPS_MS[step.min(RETRY_GAPS_MS.len() - 1)]
}

/// Where an entry's files came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntryOrigin {
    Download,
    Upload,
}

impl EntryOrigin {
    fn code(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Upload => "upload",
        }
    }

    fn parse(text: &str) -> Self {
        if text == "upload" {
            Self::Upload
        } else {
            Self::Download
        }
    }
}

/// Where an entry is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EntryState {
    /// Waiting for a person.
    Held,
    /// A person's action runs on it now.
    Placing,
    /// Every file was decided.
    Resolved,
    /// Kept past its expiry; its held copies were removed.
    Expired,
}

impl EntryState {
    /// Waiting for a person, or being acted on: the one meaning of open
    /// (the SQL side is [`sql::OPEN`]).
    pub fn is_open(self) -> bool {
        matches!(self, Self::Held | Self::Placing)
    }

    fn parse(text: &str) -> Self {
        match text {
            "placing" => Self::Placing,
            "resolved" => Self::Resolved,
            "expired" => Self::Expired,
            _ => Self::Held,
        }
    }
}

/// Which app-owned folder a held copy sits in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeldRoot {
    /// The held folder, where a download's file is copied.
    Held,
    /// The drop staging folder, where an upload waits.
    Drop,
}

impl HeldRoot {
    fn code(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Drop => "drop",
        }
    }

    fn parse(text: &str) -> Self {
        if text == "drop" {
            Self::Drop
        } else {
            Self::Held
        }
    }
}

/// How a held file was decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    Imported,
    AlreadyOwned,
    Discarded,
}

impl FileState {
    fn code(self) -> &'static str {
        match self {
            Self::Imported => "imported",
            Self::AlreadyOwned => "already_owned",
            Self::Discarded => "discarded",
        }
    }
}

/// What an entry is about. Written once, by [`GroupKey::text`], when the
/// entry is made: one open entry per user and key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupKey {
    /// An upload's drop item.
    Upload(i64),
    /// A track download, by recording.
    Track(String),
    /// An album download, by release group.
    Album(String),
    /// A download held before any release was matched, by task.
    Task(String),
}

impl GroupKey {
    /// A download's key: its recording for a track download, else its
    /// release group, else the task itself.
    pub fn for_download(
        download_type: &str,
        recording_mbid: Option<&str>,
        release_group_mbid: Option<&str>,
        task_id: &str,
    ) -> Self {
        fn given(value: Option<&str>) -> Option<&str> {
            value.filter(|value| !value.trim().is_empty())
        }
        match (
            download_type,
            given(recording_mbid),
            given(release_group_mbid),
        ) {
            ("track", Some(recording), _) => Self::Track(recording.to_ascii_lowercase()),
            (_, _, Some(group)) => Self::Album(group.to_ascii_lowercase()),
            _ => Self::Task(task_id.to_owned()),
        }
    }

    /// The stored key.
    pub fn text(&self) -> String {
        match self {
            Self::Upload(item) => format!("upload:{item}"),
            Self::Track(recording) => format!("track:{}", recording.to_ascii_lowercase()),
            Self::Album(group) => format!("album:{}", group.to_ascii_lowercase()),
            Self::Task(task) => format!("task:{task}"),
        }
    }
}

/// The track a held file was paired to.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Pairing {
    pub release_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
    pub recording_mbid: Option<String>,
    pub disc_number: Option<i64>,
    pub track_number: Option<i64>,
    pub track_title: Option<String>,
    pub expected_duration_seconds: Option<f64>,
}

/// One held file.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldFile {
    pub id: i64,
    pub entry_id: i64,
    pub root: HeldRoot,
    /// Below its root folder.
    pub path: String,
    pub original_name: String,
    pub pairing: Pairing,
    pub file_format: Option<String>,
    pub duration_seconds: Option<f64>,
    pub evidence_title: Option<String>,
    pub evidence_artist: Option<String>,
    pub evidence_score: Option<f64>,
    /// `soulseek`, `usenet`, `plugin:<key>` or `upload`.
    pub source: String,
    pub source_task_id: Option<String>,
    pub reason: Reason,
    pub reason_detail: Option<String>,
    pub created_at: i64,
}

impl HeldFile {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            entry_id: row.get(1)?,
            root: HeldRoot::parse(&row.get::<_, String>(2)?),
            path: row.get(3)?,
            original_name: row.get(4)?,
            pairing: Pairing {
                release_mbid: row.get(5)?,
                release_track_mbid: row.get(6)?,
                recording_mbid: row.get(7)?,
                disc_number: row.get(8)?,
                track_number: row.get(9)?,
                track_title: row.get(10)?,
                expected_duration_seconds: row.get(13)?,
            },
            file_format: row.get(11)?,
            duration_seconds: row.get(12)?,
            evidence_title: row.get(14)?,
            evidence_artist: row.get(15)?,
            evidence_score: row.get(16)?,
            source: row.get(17)?,
            source_task_id: row.get(18)?,
            reason: Reason::from_stored(&row.get::<_, String>(19)?),
            reason_detail: row.get(20)?,
            created_at: row.get(21)?,
        })
    }

    /// Paired to a release track, so it can be placed as it is.
    pub fn is_paired(&self) -> bool {
        self.pairing
            .release_track_mbid
            .as_deref()
            .is_some_and(|id| !id.is_empty())
            || self
                .pairing
                .recording_mbid
                .as_deref()
                .is_some_and(|id| !id.is_empty())
    }
}

/// One entry and its held files.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub id: i64,
    pub user_id: String,
    pub origin: EntryOrigin,
    pub group_key: String,
    pub release_group_mbid: Option<String>,
    pub recording_mbid: Option<String>,
    pub drop_item_id: Option<i64>,
    pub artist_name: Option<String>,
    pub album_title: Option<String>,
    pub state: EntryState,
    pub created_at: i64,
    pub expires_at: i64,
    /// When the held loop next retries its files our side could not take;
    /// `None` for at its next pass.
    pub retry_at: Option<i64>,
    pub files: Vec<HeldFile>,
}

impl Entry {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get(0)?,
            user_id: row.get(1)?,
            origin: EntryOrigin::parse(&row.get::<_, String>(2)?),
            group_key: row.get(3)?,
            release_group_mbid: row.get(4)?,
            recording_mbid: row.get(5)?,
            drop_item_id: row.get(6)?,
            artist_name: row.get(7)?,
            album_title: row.get(8)?,
            state: EntryState::parse(&row.get::<_, String>(9)?),
            created_at: row.get(10)?,
            expires_at: row.get(11)?,
            retry_at: row.get(12)?,
            files: Vec::new(),
        })
    }
}

/// The entry files join.
#[derive(Debug, Clone)]
pub struct NewEntry {
    pub user_id: String,
    pub origin: EntryOrigin,
    pub key: GroupKey,
    pub release_group_mbid: Option<String>,
    pub recording_mbid: Option<String>,
    pub drop_item_id: Option<i64>,
    pub artist_name: Option<String>,
    pub album_title: Option<String>,
}

/// A file to hold.
#[derive(Debug, Clone)]
pub struct NewFile {
    pub root: HeldRoot,
    /// Below its root folder.
    pub path: String,
    pub original_name: String,
    pub pairing: Pairing,
    pub file_format: Option<String>,
    pub duration_seconds: Option<f64>,
    pub evidence_title: Option<String>,
    pub evidence_artist: Option<String>,
    pub evidence_score: Option<f64>,
    pub source: String,
    pub source_task_id: Option<String>,
    pub reason: Reason,
    pub reason_detail: Option<String>,
}

impl NewFile {
    /// A file at `path` below `root`, named after its last part, held for
    /// `reason`, with nothing else known about it yet.
    pub fn new(root: HeldRoot, path: &str, reason: Reason, source: &str) -> Self {
        Self {
            root,
            original_name: path.rsplit('/').next().unwrap_or(path).to_owned(),
            path: path.to_owned(),
            pairing: Pairing::default(),
            file_format: path
                .rsplit_once('.')
                .map(|(_, ext)| ext.to_ascii_lowercase()),
            duration_seconds: None,
            evidence_title: None,
            evidence_artist: None,
            evidence_score: None,
            source: source.to_owned(),
            source_task_id: None,
            reason,
            reason_detail: None,
        }
    }

    /// An uploaded file at `path` below the drop folder, which starts with
    /// its job's folder. It is named by its path below that folder, so two
    /// discs' `01.flac` stay apart.
    pub fn upload(path: &str, reason: Reason) -> Self {
        Self {
            original_name: path
                .split_once('/')
                .map_or(path, |(_, below)| below)
                .to_owned(),
            ..Self::new(HeldRoot::Drop, path, reason, "upload")
        }
    }
}

/// The held list over one connection: a transaction for writes, any
/// connection for reads.
pub struct HeldRows<'c> {
    conn: &'c Connection,
}

impl<'c> HeldRows<'c> {
    pub fn new(conn: &'c Connection) -> Self {
        Self { conn }
    }

    /// Hold `files` in the open entry `entry` names, made if there is
    /// none. A file whose track (or, unpaired, whose name) the entry
    /// already holds is not held again: its answer is `None` and the
    /// caller keeps no copy of it. Every file held pushes the entry's
    /// expiry to 90 days after `now`, never earlier than it was.
    pub fn hold(
        &self,
        entry: &NewEntry,
        files: &[NewFile],
        now: i64,
    ) -> rusqlite::Result<(i64, Vec<Option<i64>>)> {
        let key = entry.key.text();
        let found: Option<i64> = self
            .conn
            .query_row(sql::OPEN_ENTRY.text(), params![entry.user_id, key], |row| {
                row.get(0)
            })
            .optional()?;
        let group = entry
            .release_group_mbid
            .as_deref()
            .filter(|group| !group.is_empty())
            .map(str::to_ascii_lowercase);
        let entry_id = match found {
            Some(id) => id,
            None => {
                self.conn.execute(
                    sql::INSERT_ENTRY.text(),
                    params![
                        entry.user_id,
                        entry.origin.code(),
                        key,
                        group,
                        entry.recording_mbid.as_deref().map(str::to_ascii_lowercase),
                        entry.drop_item_id,
                        entry.artist_name,
                        entry.album_title,
                        now,
                        now + KEEP_MS,
                    ],
                )?;
                self.conn.last_insert_rowid()
            }
        };
        let mut held = Vec::with_capacity(files.len());
        for file in files {
            let same: Option<i64> = self
                .conn
                .query_row(
                    sql::SAME_FILE.text(),
                    params![
                        entry_id,
                        file.pairing
                            .release_track_mbid
                            .as_deref()
                            .filter(|id| !id.is_empty()),
                        file.original_name
                    ],
                    |row| row.get(0),
                )
                .optional()?;
            if same.is_some() {
                tracing::info!(
                    entry_id,
                    file = %file.original_name,
                    "the entry already holds this track or file; not held twice"
                );
                held.push(None);
                continue;
            }
            self.conn.execute(
                sql::INSERT_FILE.text(),
                params![
                    entry_id,
                    file.root.code(),
                    file.path,
                    file.original_name,
                    file.pairing.release_mbid,
                    file.pairing.release_track_mbid,
                    file.pairing
                        .recording_mbid
                        .as_deref()
                        .map(str::to_ascii_lowercase),
                    file.pairing.disc_number,
                    file.pairing.track_number,
                    file.pairing.track_title,
                    file.file_format,
                    file.duration_seconds,
                    file.pairing.expected_duration_seconds,
                    file.evidence_title,
                    file.evidence_artist,
                    file.evidence_score,
                    file.source,
                    file.source_task_id,
                    file.reason.code(),
                    file.reason_detail,
                    now,
                ],
            )?;
            held.push(Some(self.conn.last_insert_rowid()));
        }
        if held.iter().any(Option::is_some) || found.is_none() {
            self.conn.execute(
                sql::EXTEND_ENTRY.text(),
                params![
                    entry_id,
                    now + KEEP_MS,
                    group,
                    entry.artist_name,
                    entry.album_title
                ],
            )?;
        }
        Ok((entry_id, held))
    }

    /// Whether the open entry `entry` names already holds this file's
    /// track (or, unpaired, a file of its name).
    pub fn already_held(&self, entry: &NewEntry, file: &NewFile) -> rusqlite::Result<bool> {
        let found: Option<i64> = self
            .conn
            .query_row(
                sql::OPEN_ENTRY.text(),
                params![entry.user_id, entry.key.text()],
                |row| row.get(0),
            )
            .optional()?;
        let Some(entry_id) = found else {
            return Ok(false);
        };
        Ok(self
            .conn
            .query_row(
                sql::SAME_FILE.text(),
                params![
                    entry_id,
                    file.pairing
                        .release_track_mbid
                        .as_deref()
                        .filter(|id| !id.is_empty()),
                    file.original_name
                ],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .is_some())
    }

    /// Open entries with their held files, newest first: one owner's (or
    /// everyone's), one album's, one download's.
    pub fn list(
        &self,
        owner: Option<&str>,
        release_group_mbid: Option<&str>,
        task_id: Option<&str>,
    ) -> rusqlite::Result<Vec<Entry>> {
        let mut statement = self.conn.prepare_cached(sql::LIST_ENTRIES.text())?;
        let entries = statement
            .query_map(params![owner, release_group_mbid, task_id], Entry::from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        entries
            .into_iter()
            .map(|entry| self.with_files(entry))
            .collect()
    }

    /// One entry with its held files, whatever its state.
    pub fn entry(&self, id: i64) -> rusqlite::Result<Option<Entry>> {
        let entry = self
            .conn
            .query_row(sql::ENTRY_BY_ID.text(), params![id], Entry::from_row)
            .optional()?;
        entry.map(|entry| self.with_files(entry)).transpose()
    }

    /// The open entry of a drop item.
    pub fn entry_of_item(&self, item_id: i64) -> rusqlite::Result<Option<Entry>> {
        let entry = self
            .conn
            .query_row(sql::ENTRY_OF_ITEM.text(), params![item_id], Entry::from_row)
            .optional()?;
        entry.map(|entry| self.with_files(entry)).transpose()
    }

    fn with_files(&self, mut entry: Entry) -> rusqlite::Result<Entry> {
        let mut statement = self.conn.prepare_cached(sql::HELD_FILES_OF.text())?;
        entry.files = statement
            .query_map(params![entry.id], HeldFile::from_row)?
            .collect::<Result<_, _>>()?;
        Ok(entry)
    }

    /// One held file.
    pub fn file(&self, id: i64) -> rusqlite::Result<Option<HeldFile>> {
        self.conn
            .query_row(sql::FILE_BY_ID.text(), params![id], HeldFile::from_row)
            .optional()
    }

    /// Claim an entry waiting for a person for one action. False when it
    /// is already being acted on or decided.
    pub fn claim(&self, entry_id: i64) -> rusqlite::Result<bool> {
        Ok(self.conn.execute(sql::CLAIM.text(), params![entry_id])? == 1)
    }

    /// Give a claimed entry back: waiting again, or resolved when none of
    /// its files is held any more.
    pub fn release(&self, entry_id: i64, now: i64) -> rusqlite::Result<()> {
        self.conn.execute(sql::RELEASE.text(), params![entry_id])?;
        self.settle(entry_id, now)
    }

    /// Decide held files of one entry, and resolve the entry when none is
    /// left held. Answers how many files moved.
    pub fn resolve(
        &self,
        entry_id: i64,
        file_ids: &[i64],
        state: FileState,
        now: i64,
    ) -> rusqlite::Result<usize> {
        let mut moved = 0;
        for id in file_ids {
            moved += self
                .conn
                .execute(sql::RESOLVE_FILE.text(), params![id, state.code(), now])?;
        }
        self.settle(entry_id, now)?;
        Ok(moved)
    }

    /// Resolve an open entry none of whose files is held any more; its
    /// drop item, if any, is settled with it.
    fn settle(&self, entry_id: i64, now: i64) -> rusqlite::Result<()> {
        if self
            .conn
            .execute(sql::SETTLE_ENTRY.text(), params![entry_id, now])?
            == 0
        {
            return Ok(());
        }
        let item: Option<i64> = self
            .conn
            .query_row(sql::ENTRY_BY_ID.text(), params![entry_id], |row| row.get(6))
            .optional()?
            .flatten();
        if let Some(item) = item {
            self.settle_item(item, "Discarded.", now)?;
        }
        Ok(())
    }

    /// A drop item whose entry is decided takes its final status.
    fn settle_item(&self, item_id: i64, discarded: &str, now: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            sql::SETTLE_ITEM.text(),
            params![item_id, discarded, now, Reason::AlreadyOwned.message()],
        )?;
        Ok(())
    }

    /// Count files a drop item now has in the library.
    pub fn add_item_imports(&self, item_id: i64, imported: usize) -> rusqlite::Result<()> {
        if imported > 0 {
            self.conn.execute(
                sql::ITEM_IMPORTED.text(),
                params![item_id, i64::try_from(imported).unwrap_or(i64::MAX)],
            )?;
        }
        Ok(())
    }

    /// Keep a held file held for another reason, in place.
    pub fn rehold(
        &self,
        file_id: i64,
        reason: Reason,
        detail: Option<&str>,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            sql::REHOLD.text(),
            params![
                file_id,
                reason.code(),
                detail.filter(|detail| !detail.is_empty())
            ],
        )?;
        Ok(())
    }

    /// Pair a held file with the release track a person's album gave it.
    pub fn pair(&self, file_id: i64, pairing: &Pairing) -> rusqlite::Result<()> {
        self.conn.execute(
            sql::PAIR_FILE.text(),
            params![
                file_id,
                pairing.release_mbid,
                pairing.release_track_mbid,
                pairing
                    .recording_mbid
                    .as_deref()
                    .map(str::to_ascii_lowercase),
                pairing.disc_number,
                pairing.track_number,
                pairing.track_title,
                pairing.expected_duration_seconds,
            ],
        )?;
        Ok(())
    }

    /// Whether a download task still has a file held in an open entry: its
    /// automatic retry waits while it does.
    pub fn task_holds(&self, task_id: &str) -> rusqlite::Result<bool> {
        self.conn
            .query_row(sql::TASK_HOLDS.text(), params![task_id], |row| row.get(0))
    }

    /// The tasks among `task_ids` that still have a file held in an open
    /// entry.
    pub fn tasks_holding(&self, task_ids: &[String]) -> rusqlite::Result<HashSet<String>> {
        if task_ids.is_empty() {
            return Ok(HashSet::new());
        }
        let mut statement = self
            .conn
            .prepare_cached(&sql::tasks_holding(ListLen::new(task_ids.len())))?;
        let rows = statement
            .query_map(rusqlite::params_from_iter(task_ids), |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Open entries of one owner, or of everyone.
    pub fn count_open(&self, owner: Option<&str>) -> rusqlite::Result<i64> {
        self.conn
            .query_row(sql::COUNT_OPEN.text(), params![owner], |row| row.get(0))
    }

    /// The download copies of an album held at or before `before`, in
    /// entries waiting for a person (one being acted on is left alone):
    /// `(file, entry, path below the held folder)`.
    pub fn removed_album_files(
        &self,
        release_group_mbid: &str,
        before: i64,
    ) -> rusqlite::Result<Vec<(i64, i64, String)>> {
        let mut statement = self.conn.prepare_cached(sql::REMOVED_ALBUM_FILES.text())?;
        let rows = statement
            .query_map(params![release_group_mbid, before], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Owner `user_id`'s files held as only the album version by their
    /// other track downloads (not `task_id`) of one recording.
    pub fn album_version_files(
        &self,
        user_id: &str,
        recording_mbid: &str,
        task_id: &str,
    ) -> rusqlite::Result<Vec<HeldFile>> {
        let mut statement = self.conn.prepare_cached(sql::ALBUM_VERSION_FILES.text())?;
        let rows = statement
            .query_map(
                params![user_id, recording_mbid, task_id],
                HeldFile::from_row,
            )?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Entries past their expiry at `now`, oldest first.
    pub fn due(&self, now: i64, limit: i64) -> rusqlite::Result<Vec<i64>> {
        let mut statement = self.conn.prepare_cached(sql::DUE_ENTRIES.text())?;
        let rows = statement
            .query_map(params![now, limit], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// Claim an entry past its expiry for the prune: `held` to `expired`,
    /// so one being acted on is skipped. Answers the copies to remove,
    /// `None` when the claim was lost. A drop item it held settles too.
    pub fn expire(
        &self,
        entry_id: i64,
        now: i64,
    ) -> rusqlite::Result<Option<Vec<(HeldRoot, String)>>> {
        if self
            .conn
            .execute(sql::EXPIRE_ENTRY.text(), params![entry_id, now])?
            == 0
        {
            return Ok(None);
        }
        let mut statement = self.conn.prepare_cached(sql::DISCARD_ALL_FILES.text())?;
        let copies = statement
            .query_map(params![entry_id, now], |row| {
                Ok((HeldRoot::parse(&row.get::<_, String>(0)?), row.get(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let item: Option<i64> = self
            .conn
            .query_row(sql::ENTRY_BY_ID.text(), params![entry_id], |row| row.get(6))
            .optional()?
            .flatten();
        if let Some(item) = item {
            self.settle_item(item, "Removed after 90 days.", now)?;
        }
        Ok(Some(copies))
    }

    /// Entries waiting for a person whose automatic retry is due at `now`,
    /// longest waiting first: id and owner.
    pub fn due_retries(&self, now: i64, limit: i64) -> rusqlite::Result<Vec<(i64, String)>> {
        let mut statement = self.conn.prepare_cached(sql::DUE_RETRIES.text())?;
        let rows = statement
            .query_map(params![now, limit], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// After an automatic retry: an entry that still has a file it
    /// retries by itself counts one more try and waits for the next gap;
    /// one with none left starts again from the first try. Answers when
    /// the next try is.
    pub fn schedule_retry(&self, entry_id: i64, now: i64) -> rusqlite::Result<Option<i64>> {
        let state: Option<(bool, i64)> = self
            .conn
            .query_row(sql::RETRY_STATE.text(), params![entry_id], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .optional()?;
        let (tries, at) = match state {
            None => return Ok(None),
            Some((true, tries)) => (tries + 1, Some(now + retry_gap(tries))),
            Some((false, _)) => (0, None),
        };
        self.conn
            .execute(sql::SET_RETRY.text(), params![entry_id, tries, at])?;
        Ok(at)
    }

    /// Entries an action had claimed.
    pub fn placing(&self) -> rusqlite::Result<Vec<Entry>> {
        let mut statement = self.conn.prepare_cached(sql::PLACING_ENTRIES.text())?;
        let entries = statement
            .query_map([], Entry::from_row)?
            .collect::<Result<Vec<_>, _>>()?;
        entries
            .into_iter()
            .map(|entry| self.with_files(entry))
            .collect()
    }

    /// Every copy in the held folder an open entry still names.
    pub fn held_folder_paths(&self) -> rusqlite::Result<HashSet<String>> {
        let mut statement = self.conn.prepare_cached(sql::HELD_FOLDER_PATHS.text())?;
        let rows = statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_written_by_one_rule() {
        let key = |kind, recording, group| GroupKey::for_download(kind, recording, group, "t1");
        assert_eq!(key("track", Some("REC"), Some("rg")).text(), "track:rec");
        assert_eq!(key("album", Some("rec"), Some("RG")).text(), "album:rg");
        assert_eq!(key("album", None, Some(" ")).text(), "task:t1");
        assert_eq!(GroupKey::Upload(7).text(), "upload:7");
    }

    #[test]
    fn retries_wait_longer_each_time_up_to_a_day() {
        let minutes: Vec<i64> = (0..10).map(|tries| retry_gap(tries) / 60_000).collect();
        assert_eq!(minutes, [5, 15, 30, 60, 120, 240, 480, 1440, 1440, 1440]);
    }

    #[test]
    fn the_library_reasons_in_sql_are_the_library_kind() {
        let listed: Vec<&str> = sql::LIBRARY_REASONS
            .trim_matches(|c| c == '(' || c == ')')
            .split(',')
            .map(|code| code.trim().trim_matches('\''))
            .collect();
        let library: Vec<&str> = Reason::ALL
            .iter()
            .filter(|reason| reason.kind() == crate::reason::ReasonKind::Library)
            .map(|reason| reason.code())
            .collect();
        assert_eq!(listed, library);
    }

    #[test]
    fn the_reason_check_lists_every_spelling() {
        let migration = include_str!("../../../migrations/0090_acquire_queue_gave_way.sql");
        let check = migration
            .split("reason TEXT NOT NULL CHECK (reason IN (")
            .nth(1)
            .and_then(|rest| rest.split("))").next())
            .unwrap_or_default();
        let listed: Vec<&str> = check
            .split(',')
            .map(|code| code.trim().trim_matches('\''))
            .collect();
        let known: Vec<&str> = Reason::ALL.iter().map(|reason| reason.code()).collect();
        assert_eq!(listed, known);
    }
}
