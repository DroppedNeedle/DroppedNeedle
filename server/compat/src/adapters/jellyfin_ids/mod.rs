//! Stored Jellyfin ids: the `compat_jellyfin_ids` table and the loop that
//! fills it.
//!
//! Album and artist ids are a MAC under the data key, so nobody without an
//! account can work one out; every other id is `sha256("kind:internal")`.
//! Every id handed out has a row, written by the fill loop
//! ([`JOB_NAME`]) after the catalog or the playlists change, never by a
//! request. So an id an app cached before a restart resolves with one
//! read by primary key, and nothing on a request path lists the catalog.
//!
//! A stored id is the id. The fill only adds rows for items that have
//! none, so a change of the data key (`reset-key`, or a backup restored
//! under another key) changes no stored album or artist id: only items
//! added afterwards get ids under the new key. The first pass of each
//! process loads the stored ids that differ from the computed ones into a
//! small map, and until it has committed, [`JellyfinIds::ready`] is false
//! and the Jellyfin routes answer busy.
//!
//! Aliases (`current = 0`) keep resolving for signed-in callers: the old
//! unkeyed id of every album and artist, and both ids of every album and
//! artist a merge retired. An alias row names the alias itself; the caller
//! follows it to the item that holds it now.

pub mod sql;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, Transaction, params, params_from_iter};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::jellyfin::builders::LIBRARY_INTERNAL_ID;
use crate::subsonic::views::genre_slug;
use droppedneedle_kernel::db::statement::{ListLen, Statement};
use droppedneedle_kernel::db::{CancelFlag, ChunkedError, Db, DbError};
use droppedneedle_kernel::jobs::{
    AlreadyRunning, Next, Schedule, Supervisor, Tick, TickCtx, TickError, TickResult,
};
use droppedneedle_kernel::runtime_config::Crypto;

/// The fill loop on the supervisor.
pub const JOB_NAME: &str = "compat-jellyfin-ids";

/// Longest a signed-in lookup that missed waits for the pass covering the
/// last commit.
pub const RESOLVE_WAIT: Duration = Duration::from_secs(3);

/// Shortest gap between two passes over the catalog: a scan commits many
/// times a second.
const CATALOG_PACE: Duration = Duration::from_secs(1);

/// The catalog is looked over at least this often, whatever its revision
/// says, in case a writer missed its bump. Also the loop's idle ceiling.
const CATALOG_CEILING: Duration = Duration::from_secs(600);

/// Shortest gap between two passes.
const MIN_GAP: Duration = Duration::from_millis(250);

/// Items per chunked write; an album or artist is two rows.
const CHUNK_ITEMS: usize = 500;

/// Ids per lookup statement.
const LOOKUP_BATCH: usize = 500;

/// The data key's purpose for album and artist ids: nothing else the key
/// signs is a valid id, and an id signs nothing else.
const ITEM_ID_PURPOSE: &str = "jellyfin-item-id";

/// Row code of a pass the database was too busy for.
const BUSY: &str = "JELLYFIN_IDS_BUSY";
/// Row code of a pass that failed.
const FAILED: &str = "JELLYFIN_IDS_FAILED";

/// What a Jellyfin id names; the table's `kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdKind {
    Album,
    Artist,
    Track,
    Genre,
    Playlist,
    Library,
}

impl IdKind {
    /// The name stored and used by the routes.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Album => "album",
            Self::Artist => "artist",
            Self::Track => "track",
            Self::Genre => "genre",
            Self::Playlist => "playlist",
            Self::Library => "library",
        }
    }

    /// The kind called `name`.
    pub fn named(name: &str) -> Option<Self> {
        [
            Self::Album,
            Self::Artist,
            Self::Track,
            Self::Genre,
            Self::Playlist,
            Self::Library,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == name)
    }

    /// Whether its ids are made with the data key.
    fn keyed(self) -> bool {
        matches!(self, Self::Album | Self::Artist)
    }
}

/// A read or write of the id table that did not complete.
#[derive(Debug)]
pub enum IdsError {
    /// The database was too busy; try again later.
    Busy,
    /// Anything else, with its cause for the log.
    Failed(String),
}

impl fmt::Display for IdsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy => formatter.write_str("database busy"),
            Self::Failed(cause) => formatter.write_str(cause),
        }
    }
}

impl From<DbError> for IdsError {
    fn from(error: DbError) -> Self {
        match error {
            DbError::Busy { .. } => Self::Busy,
            other => Self::Failed(other.to_string()),
        }
    }
}

impl From<rusqlite::Error> for IdsError {
    fn from(error: rusqlite::Error) -> Self {
        DbError::from(error).into()
    }
}

/// One row to store.
#[derive(Debug, Clone)]
struct Row {
    jellyfin_id: String,
    kind: IdKind,
    internal: String,
    current: bool,
}

/// The rows of one item, written in one chunk so a current id never lands
/// without its alias.
type Item = Vec<Row>;

/// Current ids stored under another data key than today's, by item.
type Keyed = HashMap<(IdKind, String), String>;

/// What the fill has done in this process.
#[derive(Debug, Default)]
struct Progress {
    /// The commit counter read at the start of the last pass that finished:
    /// everything committed up to it has its ids stored.
    covered: Option<u64>,
    /// Loaded by the first pass; `None` until it has committed.
    keyed: Option<Arc<Keyed>>,
}

/// The stored ids (see module docs). Cloneable; clones share the progress
/// of the one fill loop.
#[derive(Clone)]
pub struct JellyfinIds {
    db: Db,
    crypto: Arc<Crypto>,
    wait: Duration,
    progress: Arc<watch::Sender<Progress>>,
}

impl JellyfinIds {
    /// Ids over compat's database, keyed with the data key. A signed-in
    /// lookup that missed waits up to `wait` for the fill; production
    /// passes [`RESOLVE_WAIT`].
    pub fn new(db: Db, crypto: Arc<Crypto>, wait: Duration) -> Self {
        Self {
            db,
            crypto,
            wait,
            progress: Arc::new(watch::channel(Progress::default()).0),
        }
    }

    /// The unkeyed id: the first 32 hex characters of
    /// `sha256("kind:internal")`. Tracks, playlists, genres and the library
    /// keep it; for an album or artist it is the old id.
    pub fn derive(kind: &str, internal: &str) -> String {
        use sha2::{Digest, Sha256};
        format!(
            "{:x}",
            Sha256::digest(format!("{kind}:{internal}").as_bytes())
        )[..32]
            .to_owned()
    }

    /// The id an item gets under today's data key. Album and artist ids
    /// are the first 32 hex characters of a MAC under the data key over
    /// `kind\0internal`.
    pub fn computed(&self, kind: &str, internal: &str) -> String {
        if kind != "album" && kind != "artist" {
            return Self::derive(kind, internal);
        }
        let message = [kind.as_bytes(), b"\0", internal.as_bytes()].concat();
        self.crypto.mac(ITEM_ID_PURPOSE, &message)[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Whether this process's first fill pass has committed.
    pub fn ready(&self) -> bool {
        self.progress.borrow().keyed.is_some()
    }

    /// The id handed out for an item: the stored one where it was made
    /// under another key, else the computed one. Reads nothing.
    pub fn handed_out(&self, kind: &str, internal: &str) -> String {
        let stored = IdKind::named(kind).and_then(|named| {
            let progress = self.progress.borrow();
            progress
                .keyed
                .as_ref()?
                .get(&(named, internal.to_owned()))
                .cloned()
        });
        stored.unwrap_or_else(|| self.computed(kind, internal))
    }

    /// The commit counter now.
    fn commits(&self) -> u64 {
        *self.db.commits().borrow()
    }

    /// Wait, up to the lookup wait, until a pass that started after every
    /// commit made so far has finished. True once one has.
    pub async fn caught_up(&self) -> bool {
        let seen = self.commits();
        let mut progress = self.progress.subscribe();
        let caught = |progress: &Progress| progress.covered.is_some_and(|start| start >= seen);
        if caught(&progress.borrow()) {
            return true;
        }
        matches!(
            tokio::time::timeout(self.wait, progress.wait_for(caught)).await,
            Ok(Ok(_))
        )
    }

    /// What each of `jf_ids` names, in order: `None` for an id not stored
    /// or not well formed. Dashes and case are ignored. A signed-in caller
    /// (`signed_in`) is answered aliases too, and when an id is missing,
    /// waits once for the fill to catch up with the last commit, then
    /// looks again. A caller who has not signed in gets current ids only
    /// and never waits.
    pub async fn resolve(
        &self,
        jf_ids: &[String],
        signed_in: bool,
    ) -> Result<Vec<Option<(IdKind, String)>>, IdsError> {
        let wanted: Vec<Option<String>> = jf_ids.iter().map(|id| normal(id)).collect();
        let mut asked: Vec<String> = wanted.iter().flatten().cloned().collect();
        asked.sort();
        asked.dedup();
        let mut found = self.lookup(&asked, !signed_in).await?;
        let missing: Vec<String> = asked
            .into_iter()
            .filter(|id| !found.contains_key(id))
            .collect();
        if signed_in && !missing.is_empty() {
            self.caught_up().await;
            found.extend(self.lookup(&missing, false).await?);
        }
        Ok(wanted
            .into_iter()
            .map(|id| id.and_then(|id| found.get(&id).cloned()))
            .collect())
    }

    /// The stored rows of `ids`, current ones only when `current_only`.
    async fn lookup(
        &self,
        ids: &[String],
        current_only: bool,
    ) -> Result<HashMap<String, (IdKind, String)>, IdsError> {
        let mut found = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(LOOKUP_BATCH) {
            let chunk = chunk.to_vec();
            let rows: Vec<(String, String, String, bool)> = self
                .db
                .read("compat.jellyfin_ids.lookup", move |conn| {
                    let mut statement =
                        conn.prepare_cached(&sql::lookup(ListLen::new(chunk.len())))?;
                    let rows = statement
                        .query_map(params_from_iter(chunk.iter()), |row| {
                            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
                        })?
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok::<_, IdsError>(rows)
                })
                .await?;
            for (jellyfin_id, kind, internal, current) in rows {
                if current_only && !current {
                    continue;
                }
                match IdKind::named(&kind) {
                    Some(kind) => {
                        found.insert(jellyfin_id, (kind, internal));
                    }
                    None => tracing::warn!(kind, "stored jellyfin id has an unknown kind"),
                }
            }
        }
        Ok(found)
    }

    /// Start the fill loop on `supervisor`. It runs at once, then after
    /// every commit (paced) and at least every ten minutes.
    pub fn start(&self, supervisor: &Supervisor) -> Result<(), AlreadyRunning> {
        let mut wake = self.db.commits();
        wake.mark_changed();
        supervisor.on_wakeup(
            JOB_NAME,
            wake,
            Schedule::new(CATALOG_CEILING)
                .with_min_delay(MIN_GAP)
                .with_jitter(Duration::from_secs(1))
                .with_backoff(Duration::from_secs(1), Duration::from_secs(60)),
            FillTick { fill: self.fill() },
        )
    }

    /// A fill of its own, for the loop or a test that runs passes itself.
    pub fn fill(&self) -> Fill {
        Fill {
            ids: self.clone(),
            done: None,
            catalog_at: None,
        }
    }
}

/// `jf_id` without dashes, trimmed and lower-cased, when it is 32 hex
/// characters; nothing else was ever handed out.
fn normal(jf_id: &str) -> Option<String> {
    let id = jf_id.replace('-', "").trim().to_lowercase();
    (id.len() == 32 && id.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(id)
}

/// The fill: one pass stores the ids of everything that has none (see
/// module docs).
pub struct Fill {
    ids: JellyfinIds,
    /// The catalog revision the last finished catalog pass read.
    done: Option<i64>,
    /// When the last catalog pass started.
    catalog_at: Option<Instant>,
}

/// What one pass stored.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PassReport {
    /// Whether it looked over the catalog, not only the playlists.
    pub catalog: bool,
    /// Rows written, current ones and aliases.
    pub rows: usize,
}

impl Fill {
    /// Whether the catalog is due a look at `revision`.
    fn catalog_due(&self, revision: i64) -> bool {
        self.done != Some(revision)
            || self
                .catalog_at
                .is_none_or(|at| at.elapsed() >= CATALOG_CEILING)
    }

    /// How long to wait before the next pass so passes over the catalog
    /// stay [`CATALOG_PACE`] apart; `None` when it can run now.
    pub async fn pace(&self) -> Result<Option<Duration>, IdsError> {
        let Some(at) = self.catalog_at else {
            return Ok(None);
        };
        if !self.catalog_due(self.revision().await?) {
            return Ok(None);
        }
        Ok(Some(CATALOG_PACE.saturating_sub(at.elapsed())).filter(|wait| !wait.is_zero()))
    }

    async fn revision(&self) -> Result<i64, IdsError> {
        self.ids
            .db
            .read("compat.jellyfin_ids.revision", |conn| {
                Ok::<_, IdsError>(conn.query_row(sql::REVISION.text(), [], |row| row.get(0))?)
            })
            .await
    }

    /// One pass: store the ids of every item that has none. The catalog
    /// part runs when the catalog revision moved since the last pass that
    /// finished it, on the first pass, and every ten minutes; playlists are
    /// looked at every pass. Marked covered only once every chunk has
    /// committed.
    pub async fn pass(&mut self) -> Result<PassReport, IdsError> {
        let start = self.ids.commits();
        let revision = self.revision().await?;
        let catalog = self.catalog_due(revision);
        let begun = Instant::now();
        let (mut pictures, mut others, mut aliases) = (Vec::new(), Vec::new(), Vec::new());
        if catalog {
            self.catalog_items(&mut pictures, &mut others, &mut aliases)
                .await?;
        }
        for id in self.list(sql::NEW_PLAYLISTS).await? {
            others.push(vec![self.current(IdKind::Playlist, id)]);
        }
        let mut rows = 0;
        for mut items in [pictures, others, aliases] {
            items.sort_by(|a, b| a[0].jellyfin_id.cmp(&b[0].jellyfin_id));
            rows += self.write(items).await?;
        }
        let loaded = self.ids.progress.borrow().keyed.clone();
        let keyed = match loaded {
            Some(keyed) => keyed,
            None => Arc::new(self.keyed().await?),
        };
        if catalog {
            self.done = Some(revision);
            self.catalog_at = Some(begun);
        }
        self.ids.progress.send_modify(|progress| {
            progress.covered = Some(progress.covered.map_or(start, |seen| seen.max(start)));
            progress.keyed = Some(keyed);
        });
        Ok(PassReport { catalog, rows })
    }

    /// The items the catalog part finds: albums, artists and the library
    /// tile first, then tracks and genres, then the retired ids.
    async fn catalog_items(
        &self,
        pictures: &mut Vec<Item>,
        others: &mut Vec<Item>,
        aliases: &mut Vec<Item>,
    ) -> Result<(), IdsError> {
        for (kind, statement) in [
            (IdKind::Album, sql::NEW_ALBUMS),
            (IdKind::Artist, sql::NEW_ARTISTS),
        ] {
            for id in self.list(statement).await? {
                let old = alias(kind, unkeyed(kind, &id), &id);
                pictures.push(vec![self.current(kind, id), old]);
            }
        }
        let library = self.current(IdKind::Library, LIBRARY_INTERNAL_ID.to_owned());
        pictures.extend(self.unstored(vec![vec![library]]).await?);
        for statement in [sql::NEW_TRACKS, sql::NEW_PLAYLIST_FILES] {
            for id in self.list(statement).await? {
                others.push(vec![self.current(IdKind::Track, id)]);
            }
        }
        let slugs: HashSet<String> = self
            .list(sql::GENRE_NAMES)
            .await?
            .iter()
            .map(|name| genre_slug(name))
            .collect();
        let genres = slugs
            .into_iter()
            .map(|slug| vec![self.current(IdKind::Genre, slug)])
            .collect();
        others.extend(self.unstored(genres).await?);
        // A track alias is a track row of its own (its key references
        // `local_tracks`), so the tracks above cover it.
        let mut retired = Vec::new();
        for (kind, statement) in [
            (IdKind::Album, sql::RETIRED_ALBUMS),
            (IdKind::Artist, sql::RETIRED_ARTISTS),
        ] {
            for id in self.list(statement).await? {
                retired.push(vec![
                    alias(kind, self.ids.computed(kind.as_str(), &id), &id),
                    alias(kind, unkeyed(kind, &id), &id),
                ]);
            }
        }
        aliases.extend(self.unstored(retired).await?);
        Ok(())
    }

    fn current(&self, kind: IdKind, internal: String) -> Row {
        Row {
            jellyfin_id: self.ids.computed(kind.as_str(), &internal),
            kind,
            internal,
            current: true,
        }
    }

    /// The one column of every row `statement` answers.
    async fn list(&self, statement: Statement) -> Result<Vec<String>, IdsError> {
        self.ids
            .db
            .read("compat.jellyfin_ids.list", move |conn| {
                let mut prepared = conn.prepare_cached(statement.text())?;
                let ids = prepared
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, IdsError>(ids)
            })
            .await
    }

    /// The items with at least one row not stored yet.
    async fn unstored(&self, items: Vec<Item>) -> Result<Vec<Item>, IdsError> {
        let ids: Vec<String> = items
            .iter()
            .flatten()
            .map(|row| row.jellyfin_id.clone())
            .collect();
        let mut stored = HashSet::with_capacity(ids.len());
        for chunk in ids.chunks(LOOKUP_BATCH) {
            let chunk = chunk.to_vec();
            let found = self
                .ids
                .db
                .read("compat.jellyfin_ids.stored", move |conn| {
                    stored_of(conn, &chunk)
                })
                .await?;
            stored.extend(found);
        }
        Ok(items
            .into_iter()
            .filter(|item| item.iter().any(|row| !stored.contains(&row.jellyfin_id)))
            .collect())
    }

    /// Write `items` in chunks; the rows written.
    async fn write(&self, items: Vec<Item>) -> Result<usize, IdsError> {
        if items.is_empty() {
            return Ok(0);
        }
        let rows = items.iter().map(Vec::len).sum();
        let written = self
            .ids
            .db
            .write_chunked(
                "compat.jellyfin_ids.fill",
                items,
                CHUNK_ITEMS,
                &CancelFlag::never(),
                |chunk: &[Item], tx: &Transaction| store(chunk, tx),
            )
            .await;
        match written {
            Ok(_) => Ok(rows),
            Err(ChunkedError::Failed { error, .. }) => Err(error),
            Err(ChunkedError::Cancelled { .. }) => {
                Err(IdsError::Failed("the fill was cancelled".to_owned()))
            }
        }
    }

    /// Current album and artist ids stored under another key than today's.
    async fn keyed(&self) -> Result<Keyed, IdsError> {
        let rows: Vec<(String, String, String)> = self
            .ids
            .db
            .read("compat.jellyfin_ids.keyed", |conn| {
                let mut statement = conn.prepare_cached(sql::KEYED_CURRENT.text())?;
                let rows = statement
                    .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, IdsError>(rows)
            })
            .await?;
        Ok(rows
            .into_iter()
            .filter_map(|(jellyfin_id, kind, internal)| {
                let kind = IdKind::named(&kind).filter(|kind| kind.keyed())?;
                (self.ids.computed(kind.as_str(), &internal) != jellyfin_id)
                    .then_some(((kind, internal), jellyfin_id))
            })
            .collect())
    }
}

/// The unkeyed id of an item.
fn unkeyed(kind: IdKind, internal: &str) -> String {
    JellyfinIds::derive(kind.as_str(), internal)
}

/// An alias row: `jellyfin_id` still names `internal`.
fn alias(kind: IdKind, jellyfin_id: String, internal: &str) -> Row {
    Row {
        jellyfin_id,
        kind,
        internal: internal.to_owned(),
        current: false,
    }
}

/// Which of `ids` are stored.
fn stored_of(conn: &Connection, ids: &[String]) -> Result<Vec<String>, IdsError> {
    let mut statement = conn.prepare_cached(&sql::stored(ListLen::new(ids.len())))?;
    let found = statement
        .query_map(params_from_iter(ids.iter()), |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(found)
}

/// Insert each row; a current row whose id is already stored as an alias
/// of the same item becomes its current id. Nothing stored changes else.
fn store(chunk: &[Item], tx: &Transaction) -> Result<(), IdsError> {
    let mut insert = tx.prepare_cached(sql::INSERT.text())?;
    let mut promote = tx.prepare_cached(sql::PROMOTE.text())?;
    for row in chunk.iter().flatten() {
        let values = params![
            row.jellyfin_id,
            row.kind.as_str(),
            row.internal,
            i64::from(row.current)
        ];
        if insert.execute(values)? == 0 && row.current {
            promote.execute(params![row.jellyfin_id, row.kind.as_str(), row.internal])?;
        }
    }
    Ok(())
}

/// One turn of the fill loop.
struct FillTick {
    fill: Fill,
}

impl Tick for FillTick {
    async fn tick(&mut self, ctx: &TickCtx) -> TickResult {
        let outcome = async {
            if let Some(wait) = self.fill.pace().await?
                && !ctx.sleep(wait).await
            {
                return Ok(None);
            }
            self.fill.pass().await.map(Some)
        }
        .await;
        match outcome {
            Ok(Some(_)) => Ok(Next::Continue),
            Ok(None) => Ok(Next::Finish),
            Err(IdsError::Busy) => Err(TickError::new(BUSY, IdsError::Busy)),
            Err(error) => Err(TickError::new(FAILED, error)),
        }
    }
}
