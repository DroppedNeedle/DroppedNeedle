//! Deezer, through the user's Deemix, behind the download source seam.
//!
//! A pick finds the Deezer album that is the task's edition: the plan's
//! barcode through Deezer's own API first, else Deemix's album search, each
//! candidate's tracklist judged by [`pick::fit`]. It is queued at the best
//! bitrate the account and the quality settings allow, as the whole album,
//! or as one track for a song or a one-track album. One Deezer item is
//! fetched by one task at a time: while another task's attempt holds it,
//! nothing adds, removes or lands it. An item already in Deemix's queue is
//! taken over (unfinished: attached, and left there when given up;
//! finished: removed and added fresh, which "Overwrite files: Yes" makes
//! safe).
//!
//! Polls read one `getQueue` answer per pass, shared by every Deezer
//! attempt. A finished item lands its files, mapped from Deemix's folder
//! onto the downloads folder DroppedNeedle sees and stored on the attempt,
//! so a later import reads the same files. Some endings are worked out
//! inside the attempt and spend no fail-over: account errors re-add the
//! item once after a fresh sign-in, an album Deezer has no file for at the
//! bitrate asked is added again one step lower, and an item Deemix left
//! waiting with nothing downloading is added again once the session is
//! signed in.
//!
//! Sign-ins only ever go through [`sign_in_if_needed`], whose limits hold
//! across every Deezer client. No text from a Deemix answer reaches an
//! error a person reads.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use futures_util::future::BoxFuture;

use super::client::{Bitrate, DeemixClient, DeemixError, ItemKind, QueueTarget, QueueUuid, Route};
use super::local_path;
use super::models::{
    AddOutcome, Connect, CurrentItem, DeezerAvailable, ErrorCause, ErrorKind, ItemStatus,
    Overwrite, Queue, QueueItem, Tracklist,
};
use super::pick::{self, AccountFlags, AddFor, Misfit, Mode, Score};
use super::session::{NotNow, SignIn, sign_in_if_needed};
use crate::dispatch::DispatchOrigin;
use crate::downloads::journal::Journal;
use crate::downloads::source_tag::SourceTag;
use crate::downloads::sources::{DownloadSource, SourceError, SourceHandle, TransferProgress};
use crate::downloads::state::AttemptState;
use crate::downloads::store::{StoreError, TaskRow};
use crate::folder_overlap::folder_clear;
use crate::landing::{Landed, Reported};
use crate::reason::Reason;
use crate::search_jobs::candidates::{Candidate, MAX_PER_SOURCE, Pin};
use crate::search_jobs::pinned;
use crate::sources::{automatic_blocklist, blocklist, resolve_target, tried_releases};
use crate::target::{SearchTarget, Targets};
use crate::usenet::policy::UsenetPolicy;
use droppedneedle_kernel::beneath::{RootLinks, open_file_beneath};
use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::runtime_config::Live;
use droppedneedle_kernel::runtime_config::sections::DownloadPolicy;
use droppedneedle_library::matching::strings::{isrc_key, string_dist};
use droppedneedle_providers::RequestPriority;
use droppedneedle_providers::preview::PreviewClient;

/// How long one `/connect` or `getQueue` answer serves every caller.
const MEMO_MS: i64 = 1_000;

/// Album hits asked of Deemix's search.
const SEARCH_HITS: u32 = 10;

/// Search hits whose tracklists are read, the closest by name.
const CLOSEST_HITS: usize = 4;

/// An item waiting with nothing downloading is judged stranded once two
/// polls this far apart saw it so.
const STRANDED_MS: i64 = 60_000;

/// Adds of one item that queued nothing and are tried again; the next one
/// gives the item up for the task.
const EMPTY_ADDS_WAITED: u32 = 2;

const LOST: &str = "Deemix lost its Deezer sign-in. Sign in on Deemix's page.";
const NOT_SIGNED_IN: &str = "Deemix is not signed in to Deezer.";
const HELD: &str = "Another download is fetching this Deezer item.";
const MOUNT_UNUSABLE: &str = "Deemix's downloads folder is missing or not writable.";
const DEEMIX_FAULT: &str = "Deemix could not finish downloads: check its disk and network.";

/// The folders Deemix's downloads folder must stay clear of, read on each
/// use: what each set is ("library folder", "recycle bin" and the like) and
/// its folders. Deemix's own entry is left out of the download folders.
/// `None` when a setting the sets come from does not read.
pub type MountNeighbours = Arc<dyn Fn() -> Option<Vec<(&'static str, Vec<PathBuf>)>> + Send + Sync>;

/// What a [`DeemixSource`] is built from.
pub struct DeemixParts {
    /// The client for the configured Deemix, on the one shared session.
    pub client: DeemixClient,
    /// Deezer's public API, for the barcode lookup.
    pub previews: PreviewClient,
    pub journal: Arc<Journal>,
    pub targets: Arc<Targets>,
    /// The quality settings the bitrate is chosen against.
    pub policy: Live<DownloadPolicy>,
    /// Deemix's downloads folder as DroppedNeedle sees it.
    pub downloads_mount: PathBuf,
    pub neighbours: MountNeighbours,
    pub clock: Arc<dyn Clock>,
    /// What every source built for this Deemix remembers between calls.
    pub memory: Arc<SourceMemory>,
}

/// Deezer through Deemix.
pub struct DeemixSource {
    parts: DeemixParts,
    connect: Memo<Connect>,
    queue: Memo<Queue>,
    shape_warned: AtomicBool,
}

/// What the Deezer source remembers between calls, kept once beside the
/// clients so a rebuild after a settings save keeps it. It is lost on a
/// restart, which costs at most one more try of each.
#[derive(Default)]
pub struct SourceMemory(Mutex<Memory>);

#[derive(Default)]
struct Memory {
    /// Adds that queued nothing, by task and item.
    empty_adds: HashMap<(String, QueueTarget), u32>,
    /// Items seen waiting with nothing current, by job name.
    stranded: HashMap<String, Stranded>,
    /// The one re-add an item gets after an ending inside the attempt,
    /// by job name.
    once: HashMap<String, Once>,
    /// Re-adds whose item was removed and not added back yet, by job name.
    pending: HashMap<String, (QueueTarget, Bitrate)>,
}

#[derive(Debug, Clone, Copy)]
struct Stranded {
    since_ms: i64,
    signed_in: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Once {
    /// The session was dropped and waits for a sign-in.
    SigningIn,
    /// Signed in for the re-add, which has not been sent yet.
    SignedIn,
    /// The item was removed to be added again: no second try.
    ReAdded,
}

/// One answer shared for [`MEMO_MS`].
struct Memo<T>(Mutex<Option<(i64, Arc<T>)>>);

impl<T> Default for Memo<T> {
    fn default() -> Self {
        Memo(Mutex::new(None))
    }
}

impl<T> Memo<T> {
    fn lock(&self) -> MutexGuard<'_, Option<(i64, Arc<T>)>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn get(&self, now_ms: i64) -> Option<Arc<T>> {
        self.lock()
            .as_ref()
            .filter(|(at, _)| (0..MEMO_MS).contains(&(now_ms - at)))
            .map(|(_, value)| Arc::clone(value))
    }

    fn put(&self, now_ms: i64, value: T) -> Arc<T> {
        let value = Arc::new(value);
        *self.lock() = Some((now_ms, Arc::clone(&value)));
        value
    }

    fn clear(&self) {
        *self.lock() = None;
    }
}

/// What a sign-in found.
enum Signed {
    /// Signed in, as this `/connect` answer shows.
    In(Arc<Connect>),
    Lost,
    NotNow(NotNow),
}

/// How a finished item with failed tracks ends.
#[derive(Debug, PartialEq, Eq)]
enum Ending {
    /// Every failed track was refused for the account.
    AccountErrors,
    /// Every failed track had no file at the bitrate asked.
    WrongBitrate,
    /// Every failed track failed in Deemix itself (its network or disk):
    /// such an error names no cause.
    DeemixFault,
    Other,
}

/// How an item ends, by the causes of its failed tracks. A failure Deemix
/// names no cause for is a class of its own, never left out.
fn ending(item: &QueueItem) -> Ending {
    let causes: Vec<Option<ErrorCause>> = item
        .errors
        .iter()
        .flatten()
        .filter(|error| error.kind == ErrorKind::Track)
        .map(|error| error.errid.map(|id| id.cause))
        .collect();
    let all = |test: fn(Option<ErrorCause>) -> bool| {
        !causes.is_empty() && causes.iter().all(|cause| test(*cause))
    };
    if all(|cause| {
        matches!(
            cause,
            Some(ErrorCause::NotLoggedIn | ErrorCause::WrongLicense)
        )
    }) {
        Ending::AccountErrors
    } else if all(|cause| cause == Some(ErrorCause::WrongBitrate)) {
        Ending::WrongBitrate
    } else if all(|cause| cause.is_none()) {
        Ending::DeemixFault
    } else {
        Ending::Other
    }
}

/// A Deemix failure as the worker sees it: Deemix is unavailable, with a
/// fixed sentence.
fn said(error: &DeemixError) -> &'static str {
    match error {
        DeemixError::BadUrl => "The Deemix URL is not a usable address.",
        DeemixError::InvalidRequest(_) => "Deemix was not asked: the request was not valid.",
        DeemixError::Unavailable(_) | DeemixError::Status { .. } => "Deemix is not answering.",
        DeemixError::NotDeemix { .. } => "This address isn't Deemix.",
        DeemixError::Shape { .. } => "Deemix answered in a shape this version doesn't read.",
        DeemixError::TooLarge { .. } => "Clear finished downloads in Deemix.",
        DeemixError::NotSignedIn => NOT_SIGNED_IN,
        DeemixError::CantStream => "Your Deezer account can't download at that quality.",
        DeemixError::Refused { .. } => "Deemix refused the request.",
    }
}

fn unavailable(text: &str) -> SourceError {
    SourceError::Unavailable(text.to_owned())
}

fn local(error: StoreError) -> SourceError {
    SourceError::LocalFault(error.to_string())
}

fn not_now(why: NotNow) -> &'static str {
    match why {
        NotNow::NoArlOffered => "Not signed in to Deezer. Sign in on Deemix's page.",
        NotNow::DeezerUnavailable => "Deemix can't reach Deezer: restart it.",
        NotNow::Waiting => "Waiting a few minutes to sign in to Deezer again.",
        NotNow::DidNotTake => "Deemix did not take the Deezer sign-in.",
    }
}

fn flags(connect: &Connect) -> AccountFlags {
    AccountFlags {
        can_stream_lossless: connect.can_stream_lossless(),
        can_stream_hq: connect.can_stream_hq(),
    }
}

/// The tier name the account's settings card line uses.
fn tier_name(bitrate: Bitrate) -> &'static str {
    match bitrate {
        Bitrate::Flac => "FLAC 16-bit",
        Bitrate::Mp3At320 => "MP3 320",
        Bitrate::Mp3At128 => "MP3 128",
    }
}

/// The job name an attempt journals: Deemix's uuid, the task and the
/// source's attempt index.
fn job_name(uuid: &QueueUuid, task_id: &str, index: &str) -> String {
    format!("{}:{task_id}:{index}", uuid.as_str())
}

/// A job name's uuid, task and index.
fn parse_job(job: &str) -> Option<(QueueUuid, &str, &str)> {
    let (uuid, rest) = job.split_once(':')?;
    let (task_id, index) = rest.rsplit_once(':')?;
    Some((QueueUuid::parse(uuid)?, task_id, index))
}

fn bad_job() -> SourceError {
    SourceError::rejected(Reason::ClientDropped, "not a Deemix job name")
}

/// The item and bitrate a uuid names.
fn item_of(uuid: &QueueUuid) -> Option<(QueueTarget, Bitrate)> {
    let mut parts = uuid.as_str().split('_');
    let kind = parts.next()?;
    let id: u64 = parts.next()?.parse().ok()?;
    let bitrate = Bitrate::try_from(parts.next()?.parse::<u8>().ok()?).ok()?;
    let target = match kind {
        "album" => QueueTarget::album(id),
        _ => QueueTarget::track(id),
    };
    Some((target.ok()?, bitrate))
}

fn kind_name(target: QueueTarget) -> &'static str {
    match target.kind() {
        ItemKind::Album => "album",
        ItemKind::Track => "track",
    }
}

/// What a failover blocklists and the tried set holds.
fn identity(target: QueueTarget) -> String {
    format!("deezer:{}:{}", kind_name(target), target.id())
}

/// The job names of every bitrate of one item start with this.
fn holder_prefix(target: QueueTarget) -> String {
    format!("{}_{}_", kind_name(target), target.id())
}

/// The album a Deezer candidate's pin names.
fn album_of(identity: &str) -> Option<u64> {
    identity.strip_prefix("deezer:album:")?.parse().ok()
}

fn unfinished(status: ItemStatus) -> bool {
    matches!(status, ItemStatus::InQueue | ItemStatus::Downloading)
}

/// What an add of `album` queues for `target`: the wanted track for a
/// song, the one track of a one-track album, else the album.
fn queue_target(target: &SearchTarget, album: &Tracklist) -> Option<QueueTarget> {
    if Mode::of(target) == Mode::Song {
        let wanted = pick::paired_track(target, album, *target.wanted.first()?)?;
        return QueueTarget::track(wanted.id).ok();
    }
    match album.tracks.as_slice() {
        [only] => QueueTarget::track(only.id).ok(),
        _ => QueueTarget::album(album.id).ok(),
    }
}

/// Target tracks that have a Deezer track at their position or one sharing
/// an ISRC: what a misfit holds, for the search page.
fn tracks_held(target: &SearchTarget, album: &Tracklist) -> usize {
    target
        .tracklist
        .iter()
        .filter(|expected| {
            let keys: Vec<String> = expected
                .isrcs
                .iter()
                .filter_map(|isrc| isrc_key(isrc))
                .collect();
            album.tracks.iter().any(|track| {
                (i64::from(track.disk_number) == expected.disc_number
                    && i64::from(track.track_position) == expected.track_number)
                    || track
                        .isrc
                        .as_deref()
                        .and_then(isrc_key)
                        .is_some_and(|key| keys.contains(&key))
            })
        })
        .count()
}

/// The downloads folder's real path, when it is there, writable and clear
/// of every other folder. Blocking.
fn check_mount(mount: &Path, neighbours: &MountNeighbours) -> Result<PathBuf, String> {
    let real = std::fs::canonicalize(mount)
        .ok()
        .filter(|real| real.is_dir() && crate::library_ready::writable(real))
        .ok_or_else(|| MOUNT_UNUSABLE.to_owned())?;
    let sets = neighbours().ok_or_else(|| {
        "The folder settings can't be read, so Deemix's folder isn't checked.".to_owned()
    })?;
    folder_clear(&real, sets)
        .map_err(|(kind, _)| format!("Deemix's downloads folder overlaps a {kind}."))?;
    Ok(real)
}

/// Bytes on disk of `paths` below `mount`; a file not there yet counts 0.
/// Blocking.
fn bytes_on_disk(mount: &Path, paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .filter_map(|path| open_file_beneath(mount, path, RootLinks::Follow).ok())
        .filter_map(|file| file.metadata().ok())
        .map(|meta| meta.len())
        .sum()
}

fn waiting(item: &QueueItem) -> TransferProgress {
    TransferProgress {
        queued_at_client: true,
        files_total: item.size,
        ..TransferProgress::default()
    }
}

fn finished(item: &QueueItem, succeeded: bool) -> TransferProgress {
    let files = item.files.as_deref().unwrap_or_default();
    TransferProgress {
        all_terminal: true,
        all_succeeded: succeeded,
        succeeded_filenames: files.iter().map(|file| file.filename.clone()).collect(),
        files_total: item.size,
        files_completed: u32::try_from(files.len()).unwrap_or(u32::MAX),
        files_failed: item.failed.unwrap_or_default(),
        ..TransferProgress::default()
    }
}

impl DeemixSource {
    #[must_use]
    pub fn new(parts: DeemixParts) -> Self {
        DeemixSource {
            parts,
            connect: Memo::default(),
            queue: Memo::default(),
            shape_warned: AtomicBool::new(false),
        }
    }

    fn now(&self) -> i64 {
        self.parts.clock.now_ms()
    }

    fn memory(&self) -> MutexGuard<'_, Memory> {
        self.parts
            .memory
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// A Deemix failure as `Unavailable`. A shape this version does not
    /// read is logged once: polls would repeat it every pass.
    fn failed(&self, error: &DeemixError) -> SourceError {
        if matches!(error, DeemixError::Shape { .. })
            && !self.shape_warned.swap(true, Ordering::Relaxed)
        {
            tracing::warn!(%error, "Deemix answered in a shape this version does not read");
        }
        unavailable(said(error))
    }

    /// An item state this version does not read.
    fn unknown_state(&self) -> SourceError {
        self.failed(&DeemixError::Shape {
            route: Route::GetQueue,
            detail: "an item state this version does not read".to_owned(),
        })
    }

    /// Forget the session, so the next call opens one and reads fresh
    /// account flags.
    fn forget_session(&self) {
        self.parts.client.session().drop_cookie();
        self.connect.clear();
    }

    /// `/connect`, shared for [`MEMO_MS`]. The offered sign-in is never
    /// kept.
    async fn connect_now(&self) -> Result<Arc<Connect>, DeemixError> {
        if let Some(connect) = self.connect.get(self.now()) {
            return Ok(connect);
        }
        let mut connect = self.parts.client.connect().await?;
        drop(connect.take_arl());
        Ok(self.connect.put(self.now(), connect))
    }

    /// `getQueue`, shared for [`MEMO_MS`].
    async fn queue_now(&self) -> Result<Arc<Queue>, DeemixError> {
        if let Some(queue) = self.queue.get(self.now()) {
            return Ok(queue);
        }
        let queue = self.parts.client.queue().await?;
        Ok(self.queue.put(self.now(), queue))
    }

    /// Sign the session in when it needs it, under the session's limits.
    /// A signed-in answer is kept as the `/connect` answer.
    async fn try_sign_in(&self) -> Result<Signed, SourceError> {
        let signed = sign_in_if_needed(&self.parts.client, self.parts.clock.as_ref())
            .await
            .map_err(|error| self.failed(&error))?;
        Ok(match signed {
            SignIn::SignedIn(connect) => Signed::In(self.connect.put(self.now(), *connect)),
            SignIn::Lost => Signed::Lost,
            SignIn::NotNow(why) => Signed::NotNow(why),
        })
    }

    /// As [`Self::try_sign_in`], a session that stays signed out being
    /// `Unavailable`.
    async fn sign_in(&self) -> Result<AccountFlags, SourceError> {
        match self.try_sign_in().await? {
            Signed::In(connect) => Ok(flags(&connect)),
            Signed::Lost => Err(unavailable(LOST)),
            Signed::NotNow(why) => Err(unavailable(not_now(why))),
        }
    }

    /// The account's flags for an add: Deemix reaches Deezer, overwrites
    /// files, and its session is signed in (signing it in when needed).
    async fn account(&self) -> Result<AccountFlags, SourceError> {
        let connect = self.connect_now().await.map_err(|e| self.failed(&e))?;
        if connect.deezer_available != DeezerAvailable::Yes {
            return Err(unavailable(not_now(NotNow::DeezerUnavailable)));
        }
        if connect.settings().overwrite_file != Overwrite::Yes {
            return Err(unavailable("Set Overwrite files to Yes in Deemix."));
        }
        if connect.signed_in() {
            return Ok(flags(&connect));
        }
        self.sign_in().await
    }

    fn quality(&self) -> Result<UsenetPolicy, SourceError> {
        self.parts
            .policy
            .read()
            .map(|policy| UsenetPolicy::from_saved(&policy))
            .ok_or_else(|| unavailable("The download settings don't read."))
    }

    async fn mount(&self) -> Result<PathBuf, String> {
        let mount = self.parts.downloads_mount.clone();
        let neighbours = Arc::clone(&self.parts.neighbours);
        tokio::task::spawn_blocking(move || check_mount(&mount, &neighbours))
            .await
            .unwrap_or_else(|_| Err(MOUNT_UNUSABLE.to_owned()))
    }

    async fn task(&self, task_id: &str) -> Result<TaskRow, SourceError> {
        self.parts
            .journal
            .read_task(task_id)
            .await
            .map_err(local)?
            .ok_or_else(|| {
                SourceError::rejected(Reason::NoSource, format!("unknown download task {task_id}"))
            })
    }

    /// What an add for `task` is for. A waiver covers only the pick a
    /// person confirmed (`pinned`).
    async fn add_for(&self, task: &TaskRow, pinned: bool) -> Result<AddFor, SourceError> {
        let task_id = task.id.clone();
        let plan = self
            .parts
            .journal
            .read("deemix.plan", move |store| store.task_plan(&task_id))
            .await
            .map_err(local)?;
        Ok(AddFor {
            waived: pinned && plan.quality_waived,
            upgrade: task.origin == DispatchOrigin::Upgrade,
        })
    }

    /// Whether another task's attempt holds `item`.
    async fn held(&self, task_id: &str, item: QueueTarget) -> Result<bool, SourceError> {
        let holders = self
            .parts
            .journal
            .read_item_holders(&SourceTag::Deezer, &holder_prefix(item), task_id)
            .await
            .map_err(local)?;
        Ok(!holders.is_empty())
    }

    async fn tracklist(&self, id: u64) -> Result<Option<Tracklist>, SourceError> {
        self.parts
            .client
            .album_tracklist(id)
            .await
            .map_err(|error| self.failed(&error))
    }

    /// The albums that may be `target`'s edition, barcode hit first: at
    /// most five tracklists. A barcode hit `take_at_once` accepts is the
    /// only one read.
    async fn candidates(
        &self,
        target: &SearchTarget,
        priority: RequestPriority,
        take_at_once: impl Fn(&Tracklist) -> bool,
    ) -> Result<Vec<Tracklist>, SourceError> {
        let mut albums = Vec::new();
        if let Some(barcode) = target.barcode.as_deref() {
            match self
                .parts
                .previews
                .deezer_album_by_upc(barcode, priority)
                .await
            {
                Ok(Some(id)) => {
                    if let Some(album) = self.tracklist(id).await? {
                        if take_at_once(&album) {
                            return Ok(vec![album]);
                        }
                        albums.push(album);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(%error, "Deezer barcode lookup failed; searching Deemix by name");
                }
            }
        }
        let term = format!("{} {}", target.artist, target.album_title);
        let hits = match self.parts.client.search_albums(&term, SEARCH_HITS).await {
            Ok(hits) => hits,
            Err(error) if !albums.is_empty() => {
                tracing::warn!(%error, "Deemix search failed; judging the barcode hit alone");
                return Ok(albums);
            }
            Err(error) => return Err(self.failed(&error)),
        };
        let mut ranked: Vec<(f64, u64)> = hits
            .iter()
            .filter(|hit| !albums.iter().any(|album| album.id == hit.id))
            .map(|hit| {
                let distance = string_dist(&hit.title, &target.album_title)
                    + string_dist(&hit.artist.name, &target.artist);
                (distance, hit.id)
            })
            .collect();
        ranked.sort_by(|left, right| left.0.total_cmp(&right.0));
        for (_, id) in ranked.into_iter().take(CLOSEST_HITS) {
            if let Some(album) = self.tracklist(id).await? {
                albums.push(album);
            }
        }
        Ok(albums)
    }

    /// The album to queue for `task` and whether it is the pinned pick.
    async fn choose(
        &self,
        task: &TaskRow,
        target: &SearchTarget,
        pin: Option<Pin>,
        person: bool,
    ) -> Result<(Tracklist, bool), SourceError> {
        let automatic = automatic_blocklist(&self.parts.journal, task, &SourceTag::Deezer).await?;
        let tried = tried_releases(&self.parts.journal, &task.id, &SourceTag::Deezer).await?;
        let usable = |album: &Tracklist| {
            queue_target(target, album).is_some_and(|item| {
                let identity = identity(item);
                let given_up = self
                    .memory()
                    .empty_adds
                    .get(&(task.id.clone(), item))
                    .is_some_and(|count| *count > EMPTY_ADDS_WAITED);
                !automatic.blocks(&identity) && !tried.contains(&identity) && !given_up
            })
        };
        if let Some(Pin::Release { identity }) = &pin
            && let Some(id) = album_of(identity)
        {
            match self.tracklist(id).await? {
                // A person's pick is taken as they saw it.
                Some(album) if person => return Ok((album, true)),
                // The search review's pick still has to fit, and to be one
                // an automatic pick may take.
                Some(album) if pick::fit(target, &album).is_ok() && usable(&album) => {
                    return Ok((album, true));
                }
                None if person => {
                    return Err(SourceError::rejected(
                        Reason::NoSource,
                        "The Deezer album you picked is no longer there.",
                    ));
                }
                _ => {}
            }
        }
        let at_once = |album: &Tracklist| {
            usable(album) && pick::fit(target, album).is_ok_and(|score| score.every_pair_by_isrc())
        };
        let albums: Vec<Tracklist> = self
            .candidates(target, RequestPriority::BackgroundSync, at_once)
            .await?
            .into_iter()
            .filter(usable)
            .collect();
        pick::best_fit(target, &albums)
            .map(|(album, _)| (album.clone(), false))
            .ok_or_else(|| {
                SourceError::rejected(Reason::NoSource, "No Deezer album fits this edition.")
            })
    }

    /// The bitrate to add at, or why there is none.
    fn bitrate(
        account: AccountFlags,
        policy: &UsenetPolicy,
        add: AddFor,
    ) -> Result<Bitrate, SourceError> {
        if let Some(bitrate) = pick::choose_bitrate(account, policy, add) {
            return Ok(bitrate);
        }
        let plain = AddFor {
            upgrade: false,
            ..add
        };
        if add.upgrade && pick::choose_bitrate(account, policy, plain).is_some() {
            return Err(SourceError::rejected(
                Reason::NoBetterCopy,
                "Deezer has nothing above your quality cutoff for this account.",
            ));
        }
        let best = pick::choose_bitrate(
            account,
            policy,
            AddFor {
                waived: true,
                upgrade: false,
            },
        )
        .unwrap_or(Bitrate::Mp3At128);
        Err(unavailable(&format!(
            "Your Deezer account gives {}; your quality settings don't allow it.",
            tier_name(best)
        )))
    }

    /// Whether an attempt of `task_id` names `uuid`.
    async fn named_by_task(&self, task_id: &str, uuid: &QueueUuid) -> Result<bool, SourceError> {
        let attempts = self
            .parts
            .journal
            .read_attempts(task_id)
            .await
            .map_err(local)?;
        let prefix = format!("{}:", uuid.as_str());
        Ok(attempts
            .iter()
            .any(|attempt| attempt.job_name.starts_with(&prefix)))
    }

    /// Put `item` at `bitrate` in Deemix's queue for `task_id`: attach to
    /// it while it is there unfinished (adopted when no attempt of the task
    /// names it), else remove a finished one and add it fresh. The uuid is
    /// the one Deemix answers with.
    async fn place(
        &self,
        task_id: &str,
        item: QueueTarget,
        bitrate: Bitrate,
        queue: &Queue,
    ) -> Result<(QueueUuid, bool), SourceError> {
        if let Some(there) = queue.item(&item.uuid(bitrate)) {
            if there.status == ItemStatus::Unknown {
                return Err(self.unknown_state());
            }
            if unfinished(there.status) {
                let adopted = !self.named_by_task(task_id, &there.uuid).await?;
                return Ok((there.uuid.clone(), adopted));
            }
            self.remove(&there.uuid).await?;
        }
        Ok((self.add(task_id, item, bitrate).await?, false))
    }

    async fn remove(&self, uuid: &QueueUuid) -> Result<(), SourceError> {
        let removed = self.parts.client.remove_from_queue(uuid.as_str()).await;
        self.queue.clear();
        removed.map_err(|error| self.failed(&error))
    }

    /// One add. A session Deemix says is signed out is signed in and the
    /// add sent once more. An add that queues nothing is tried again on
    /// later passes, twice, then the item is given up for the task.
    async fn add(
        &self,
        task_id: &str,
        item: QueueTarget,
        bitrate: Bitrate,
    ) -> Result<QueueUuid, SourceError> {
        let client = &self.parts.client;
        let mut outcome = client.add_to_queue(item, bitrate).await;
        if outcome == Err(DeemixError::NotSignedIn) {
            self.forget_session();
            self.sign_in()
                .await
                .map_err(|_| unavailable(NOT_SIGNED_IN))?;
            outcome = client.add_to_queue(item, bitrate).await;
        }
        self.queue.clear();
        let key = (task_id.to_owned(), item);
        match outcome {
            Ok(AddOutcome::Queued(uuid)) => {
                self.memory().empty_adds.remove(&key);
                Ok(uuid)
            }
            Ok(AddOutcome::NothingAdded) => {
                let mut memory = self.memory();
                let count = memory.empty_adds.entry(key).or_default();
                *count += 1;
                tracing::warn!(task_id, item = %identity(item), empty_adds = *count, "Deemix queued nothing for an add");
                if *count > EMPTY_ADDS_WAITED {
                    Err(SourceError::rejected(
                        Reason::ClientDropped,
                        "Deemix queued nothing for this item three times.",
                    ))
                } else {
                    Err(unavailable("Deemix queued nothing; trying again later."))
                }
            }
            Err(DeemixError::CantStream) => {
                self.forget_session();
                Err(unavailable(said(&DeemixError::CantStream)))
            }
            Err(error) => Err(self.failed(&error)),
        }
    }

    async fn enqueue_pick(
        &self,
        task_id: &str,
        candidate_index: i64,
    ) -> Result<SourceHandle, SourceError> {
        let task = self.task(task_id).await?;
        let target = resolve_target(&self.parts.targets, &task).await?;
        if target.tracklist.is_empty() || !target.has_album() {
            return Err(SourceError::rejected(
                Reason::NoSource,
                "Deezer is searched only for an edition with a known tracklist.",
            ));
        }
        self.mount().await.map_err(SourceError::Unavailable)?;
        let account = self.account().await?;
        let policy = self.quality()?;
        let pin = pinned(
            &self.parts.journal,
            &task,
            &SourceTag::Deezer,
            candidate_index,
        )
        .await;
        let person = task.picked_by_person && matches!(pin, Some(Pin::Release { .. }));
        let add = self.add_for(&task, person).await?;
        let bitrate = Self::bitrate(account, &policy, add)?;
        let (album, pinned_pick) = self.choose(&task, &target, pin, person).await?;
        let item = queue_target(&target, &album).ok_or_else(|| {
            SourceError::rejected(Reason::NoSource, "The Deezer album lacks the wanted track.")
        })?;
        if self.held(task_id, item).await? {
            return Err(unavailable(HELD));
        }
        let queue = self.queue_now().await.map_err(|e| self.failed(&e))?;
        let (uuid, adopted) = self.place(task_id, item, bitrate, &queue).await?;
        Ok(SourceHandle {
            job_name: job_name(&uuid, task_id, &candidate_index.to_string()),
            release_identity: identity(item),
            pinned: pinned_pick,
            adopted,
            ..SourceHandle::new(SourceTag::Deezer)
        })
    }

    async fn progress(&self, handle: &SourceHandle) -> Result<TransferProgress, SourceError> {
        let (uuid, task_id, _) = parse_job(&handle.job_name).ok_or_else(bad_job)?;
        let queue = match self.queue_now().await {
            Ok(queue) => queue,
            // Too large to read: it waits, and the watchdog judges it
            // without blame.
            Err(DeemixError::TooLarge { .. }) => {
                return Ok(TransferProgress {
                    queued_at_client: true,
                    ..TransferProgress::default()
                });
            }
            Err(error) => return Err(self.failed(&error)),
        };
        let job = handle.job_name.as_str();
        let Some(item) = queue.item(&uuid) else {
            // A re-add that removed the item and could not add it back
            // yet sends the add again.
            let pending = self.memory().pending.get(job).copied();
            if let Some((target, bitrate)) = pending {
                self.add_back(handle, task_id, &uuid, target, bitrate)
                    .await?;
                return Ok(TransferProgress {
                    queued_at_client: true,
                    ..TransferProgress::default()
                });
            }
            self.forget_job(job);
            return Err(SourceError::rejected(
                Reason::ClientDropped,
                "Deemix no longer holds this download.",
            ));
        };
        match item.status {
            ItemStatus::Unknown => Err(self.unknown_state()),
            ItemStatus::InQueue | ItemStatus::Downloading => match queue.current() {
                Some(current) if current.uuid == uuid.as_str() => {
                    self.memory().stranded.remove(job);
                    Ok(self.active(item, current).await)
                }
                Some(_) => {
                    self.memory().stranded.remove(job);
                    Ok(waiting(item))
                }
                None => self.stranded(handle, task_id, &uuid, item).await,
            },
            ItemStatus::Completed if item.files.as_ref().is_some_and(|f| !f.is_empty()) => {
                Ok(finished(item, true))
            }
            ItemStatus::Completed => Err(SourceError::rejected(
                Reason::DownloadFailed,
                "Deemix finished this download with no files.",
            )),
            ItemStatus::WithErrors | ItemStatus::Failed => match ending(item) {
                Ending::AccountErrors => self.account_retry(handle, task_id, &uuid, item).await,
                Ending::WrongBitrate if item.status == ItemStatus::Failed => {
                    self.step_down(handle, task_id, &uuid, item, &queue).await
                }
                Ending::DeemixFault => self.deemix_fault(handle, task_id, &uuid, item).await,
                // What arrived lands; a refill fetches the rest.
                _ if item.status == ItemStatus::WithErrors => Ok(finished(item, true)),
                _ => Ok(finished(item, false)),
            },
        }
    }

    fn forget_job(&self, job: &str) {
        let mut memory = self.memory();
        memory.stranded.remove(job);
        memory.once.remove(job);
        memory.pending.remove(job);
    }

    /// The current item's progress: the bytes of its finished tracks on
    /// disk.
    async fn active(&self, item: &QueueItem, current: &CurrentItem) -> TransferProgress {
        let files = current
            .progress
            .as_ref()
            .map(|progress| progress.files.as_slice())
            .unwrap_or_default();
        let bytes = match self.connect_now().await {
            Ok(connect) => {
                let mount = self.parts.downloads_mount.clone();
                let location = &connect.settings().download_location;
                let paths: Vec<PathBuf> = files
                    .iter()
                    .filter_map(|file| local_path(&file.path, location, &mount))
                    .collect();
                tokio::task::spawn_blocking(move || bytes_on_disk(&mount, &paths))
                    .await
                    .unwrap_or_default()
            }
            Err(error) => {
                tracing::debug!(%error, "Deemix's folder unknown; progress in bytes skipped");
                0
            }
        };
        let progress = current.progress.as_ref();
        TransferProgress {
            has_active_transfer: true,
            downloaded_bytes: bytes,
            files_total: item.size,
            files_completed: progress.map_or(0, |progress| progress.downloaded),
            files_failed: progress.map_or(0, |progress| progress.failed),
            ..TransferProgress::default()
        }
    }

    /// Ours waits and nothing is downloading. Two polls a minute apart
    /// see it so: the session is signed in, and a poll after that which
    /// still sees it so removes and adds it again. Nothing is removed
    /// before a sign-in works.
    async fn stranded(
        &self,
        handle: &SourceHandle,
        task_id: &str,
        uuid: &QueueUuid,
        item: &QueueItem,
    ) -> Result<TransferProgress, SourceError> {
        let job = handle.job_name.as_str();
        let now = self.now();
        let seen = *self
            .memory()
            .stranded
            .entry(job.to_owned())
            .or_insert(Stranded {
                since_ms: now,
                signed_in: false,
            });
        if now - seen.since_ms < STRANDED_MS {
            return Ok(waiting(item));
        }
        if !seen.signed_in {
            self.sign_in().await?;
            if let Some(stranded) = self.memory().stranded.get_mut(job) {
                stranded.signed_in = true;
            }
            return Ok(waiting(item));
        }
        self.memory().stranded.remove(job);
        tracing::info!(
            task_id,
            uuid = uuid.as_str(),
            "Deemix left the download waiting; adding it again"
        );
        self.readd(handle, task_id, uuid, item, false).await
    }

    /// Remove the item and add it again at the same bitrate. The re-add is
    /// remembered as pending before the remove, so an add that queues
    /// nothing is sent again on later polls; with `once`, the item gets no
    /// second re-add for the same ending.
    async fn readd(
        &self,
        handle: &SourceHandle,
        task_id: &str,
        uuid: &QueueUuid,
        item: &QueueItem,
        once: bool,
    ) -> Result<TransferProgress, SourceError> {
        let (target, bitrate) = item_of(uuid).ok_or_else(bad_job)?;
        let job = handle.job_name.clone();
        self.memory().pending.insert(job.clone(), (target, bitrate));
        if let Err(error) = self.remove(uuid).await {
            self.memory().pending.remove(&job);
            return Err(error);
        }
        if once {
            self.memory().once.insert(job, Once::ReAdded);
        }
        self.add_back(handle, task_id, uuid, target, bitrate)
            .await?;
        Ok(waiting(item))
    }

    /// Add a removed item back, pointing the attempt at the uuid Deemix
    /// answers. An add that queues nothing leaves the re-add pending until
    /// the empty-add count gives the item up.
    async fn add_back(
        &self,
        handle: &SourceHandle,
        task_id: &str,
        uuid: &QueueUuid,
        target: QueueTarget,
        bitrate: Bitrate,
    ) -> Result<(), SourceError> {
        let job = handle.job_name.as_str();
        let added = match self.add(task_id, target, bitrate).await {
            Ok(added) => added,
            Err(error) => {
                if matches!(error, SourceError::Rejected { .. }) {
                    self.memory().pending.remove(job);
                }
                return Err(error);
            }
        };
        self.memory().pending.remove(job);
        if added != *uuid {
            let renamed = self
                .rewrite(handle, task_id, &added, handle.adopted)
                .await?;
            let mut memory = self.memory();
            if let Some(once) = memory.once.remove(job) {
                memory.once.insert(renamed, once);
            }
        }
        Ok(())
    }

    /// Every failed track was refused for the account: a fresh session,
    /// a sign-in, and the item added again, once. While the sign-in waits
    /// on its limits the item waits too.
    async fn account_retry(
        &self,
        handle: &SourceHandle,
        task_id: &str,
        uuid: &QueueUuid,
        item: &QueueItem,
    ) -> Result<TransferProgress, SourceError> {
        let job = handle.job_name.clone();
        let refused = || {
            SourceError::rejected(
                Reason::AccountRefused,
                "Deezer refused this download for the account.",
            )
        };
        let state = self.memory().once.get(&job).copied();
        match state {
            Some(Once::ReAdded) => {
                self.memory().once.remove(&job);
                return Err(refused());
            }
            Some(Once::SignedIn) => {}
            Some(Once::SigningIn) | None => {
                if state.is_none() {
                    self.forget_session();
                    self.memory().once.insert(job.clone(), Once::SigningIn);
                }
                match self.try_sign_in().await? {
                    Signed::In(_) => {
                        self.memory().once.insert(job.clone(), Once::SignedIn);
                    }
                    Signed::NotNow(NotNow::Waiting) => return Ok(waiting(item)),
                    Signed::Lost | Signed::NotNow(_) => {
                        self.memory().once.remove(&job);
                        return Err(refused());
                    }
                }
            }
        }
        self.readd(handle, task_id, uuid, item, true).await
    }

    /// Every failed track failed in Deemix itself: the item is added again
    /// once, as overwrite rewrites its files. Failing the same way again, it
    /// fails over as a local fault, blaming nothing, even with some tracks
    /// arrived: landing them would count the missing ones against the
    /// album.
    async fn deemix_fault(
        &self,
        handle: &SourceHandle,
        task_id: &str,
        uuid: &QueueUuid,
        item: &QueueItem,
    ) -> Result<TransferProgress, SourceError> {
        if self.memory().once.get(&handle.job_name) == Some(&Once::ReAdded) {
            return Err(SourceError::rejected(Reason::LocalFault, DEEMIX_FAULT));
        }
        tracing::warn!(
            task_id,
            uuid = uuid.as_str(),
            "Deemix could not finish the download; adding it again"
        );
        self.readd(handle, task_id, uuid, item, true).await
    }

    /// Deezer has no file of the album at the bitrate asked: add it at the
    /// next bitrate the settings allow, point the attempt at it, then
    /// remove the old item. No fail-over is spent.
    async fn step_down(
        &self,
        handle: &SourceHandle,
        task_id: &str,
        uuid: &QueueUuid,
        item: &QueueItem,
        queue: &Queue,
    ) -> Result<TransferProgress, SourceError> {
        let (target, current) = item_of(uuid).ok_or_else(bad_job)?;
        let task = self.task(task_id).await?;
        let policy = self.quality()?;
        let add = self.add_for(&task, handle.pinned).await?;
        let account = self
            .connect_now()
            .await
            .map(|connect| flags(&connect))
            .unwrap_or_default();
        let Some(lower) = pick::step_down(current, account, &policy, add) else {
            return Err(SourceError::rejected(
                Reason::NothingAllowed,
                "Deezer has no file of this album at a quality your settings allow.",
            ));
        };
        let (added, adopted) = self.place(task_id, target, lower, queue).await?;
        if let Err(error) = self.rewrite(handle, task_id, &added, adopted).await {
            if !adopted && let Err(error) = self.remove(&added).await {
                tracing::warn!(task_id, %error, "the stepped-down item stays in Deemix");
            }
            return Err(error);
        }
        if let Err(error) = self.remove(uuid).await {
            tracing::warn!(task_id, %error, "the item Deezer had no file for stays in Deemix");
        }
        tracing::info!(
            task_id,
            from = tier_name(current),
            to = tier_name(lower),
            "Deezer had no file at that quality; stepped down"
        );
        Ok(waiting(item))
    }

    /// Point the live attempt journaling `handle` at `uuid`, behind its
    /// revision. Answers the new job name.
    async fn rewrite(
        &self,
        handle: &SourceHandle,
        task_id: &str,
        uuid: &QueueUuid,
        adopted: bool,
    ) -> Result<String, SourceError> {
        let (_, _, index) = parse_job(&handle.job_name).ok_or_else(bad_job)?;
        let job = job_name(uuid, task_id, index);
        let attempts = self
            .parts
            .journal
            .read_attempts(task_id)
            .await
            .map_err(local)?;
        let Some(attempt) = attempts.into_iter().find(|attempt| {
            attempt.job_name == handle.job_name
                && matches!(attempt.state, AttemptState::Acquiring | AttemptState::InUse)
        }) else {
            return Err(unavailable("The download's attempt moved on."));
        };
        let next = SourceHandle {
            job_name: job.clone(),
            adopted,
            ..handle.clone()
        };
        let json = serde_json::to_string(&next)
            .map_err(|error| SourceError::LocalFault(error.to_string()))?;
        let now = self.parts.journal.db().now_unix_f64();
        let stored_job = job.clone();
        let rewritten = self
            .parts
            .journal
            .run("deemix.rewrite_handle", move |store| {
                store.rewrite_handle(&attempt.id, attempt.row_revision, &stored_job, &json, now)
            })
            .await
            .map_err(local)?;
        if rewritten.is_none() {
            tracing::warn!(task_id, job = %job, "the attempt moved on before Deemix's new item was journaled");
            return Err(unavailable("The download's attempt moved on."));
        }
        Ok(job)
    }

    async fn landed_files(&self, handle: &SourceHandle) -> Result<Landed, SourceError> {
        let (uuid, task_id, _) = parse_job(&handle.job_name).ok_or_else(bad_job)?;
        let (item, _) = item_of(&uuid).ok_or_else(bad_job)?;
        if self.held(task_id, item).await? {
            return Err(SourceError::rejected(Reason::AlreadyDownloading, HELD));
        }
        self.mount().await.map_err(SourceError::LocalFault)?;
        let journal = &self.parts.journal;
        let stored = journal
            .read_stored_paths(&SourceTag::Deezer, &handle.job_name)
            .await
            .map_err(local)?;
        let paths = match stored {
            Some(paths) => paths,
            None => {
                let paths = self.map_files(&uuid).await?;
                let (job, kept) = (handle.job_name.clone(), paths.clone());
                let now = journal.db().now_unix_f64();
                journal
                    .run("deemix.store_paths", move |store| {
                        store.store_paths(&SourceTag::Deezer, &job, &kept, now)
                    })
                    .await
                    .map_err(local)?;
                paths
            }
        };
        Ok(Landed {
            mounts: vec![self.parts.downloads_mount.clone().into()],
            files: paths.into_iter().map(Reported::path).collect(),
        })
    }

    /// The files Deemix lists for the item, mapped under the downloads
    /// folder.
    async fn map_files(&self, uuid: &QueueUuid) -> Result<Vec<PathBuf>, SourceError> {
        let queue = self.queue_now().await.map_err(|e| self.failed(&e))?;
        let item = queue
            .item(uuid)
            .ok_or_else(|| unavailable("Deemix no longer lists this download's files."))?;
        let connect = self.connect_now().await.map_err(|e| self.failed(&e))?;
        let location = &connect.settings().download_location;
        let mut paths = Vec::new();
        for file in item.files.iter().flatten() {
            match local_path(&file.path, location, &self.parts.downloads_mount) {
                Some(path) => paths.push(path),
                None => tracing::warn!(
                    uuid = uuid.as_str(),
                    "a file Deemix listed is outside its downloads folder; left out"
                ),
            }
        }
        if paths.is_empty() {
            return Err(unavailable(
                "Deemix listed no file inside its downloads folder.",
            ));
        }
        Ok(paths)
    }

    /// Take the item out of Deemix's queue, unless another task holds it
    /// or it was adopted.
    async fn give_up(&self, handle: &SourceHandle) -> Result<bool, SourceError> {
        if handle.adopted {
            return Ok(false);
        }
        let (uuid, task_id, _) = parse_job(&handle.job_name).ok_or_else(bad_job)?;
        let (item, _) = item_of(&uuid).ok_or_else(bad_job)?;
        if self.held(task_id, item).await? {
            return Ok(false);
        }
        self.remove(&uuid).await?;
        self.forget_job(&handle.job_name);
        Ok(true)
    }

    /// A manual search or the search review: every album read, those that
    /// fit first and best first. Nothing signs in: an account not known
    /// yet is judged as able to stream.
    async fn search_albums(&self, target: &SearchTarget) -> Result<Vec<Candidate>, SourceError> {
        if target.tracklist.is_empty() || !target.has_album() {
            return Ok(Vec::new());
        }
        let connect = self.connect_now().await.map_err(|e| self.failed(&e))?;
        let account = flags(&connect);
        let policy = self.quality()?;
        let allowed = pick::choose_bitrate(account, &policy, AddFor::default());
        let shown = allowed
            .or_else(|| {
                pick::choose_bitrate(
                    account,
                    &policy,
                    AddFor {
                        waived: true,
                        upgrade: false,
                    },
                )
            })
            .unwrap_or(Bitrate::Mp3At128);
        let blocked = blocklist(&self.parts.journal, &SourceTag::Deezer).await?;
        let albums = self
            .candidates(target, RequestPriority::UserInitiated, |_| false)
            .await?;
        let mut judged: Vec<(&Tracklist, Result<Score, Misfit>)> = albums
            .iter()
            .filter(|album| {
                // An album lacking a song's track is still listed, as a misfit.
                let identity = queue_target(target, album)
                    .map_or_else(|| format!("deezer:album:{}", album.id), identity);
                !blocked.blocks(&identity)
            })
            .map(|album| (album, pick::fit(target, album)))
            .collect();
        judged.sort_by(|(_, left), (_, right)| match (left, right) {
            (Ok(left), Ok(right)) => right.ordering(left),
            (Ok(_), Err(_)) => std::cmp::Ordering::Less,
            (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
            (Err(_), Err(_)) => std::cmp::Ordering::Equal,
        });
        let mut listing: Vec<Candidate> = judged
            .into_iter()
            .map(|(album, fit)| {
                let held = tracks_held(target, album);
                Candidate::from_deezer(album, fit, held, target, shown, allowed.is_none())
            })
            .collect();
        listing.truncate(MAX_PER_SOURCE);
        Ok(listing)
    }
}

impl DownloadSource for DeemixSource {
    fn tag(&self) -> SourceTag {
        SourceTag::Deezer
    }

    fn search<'a>(
        &'a self,
        target: &'a SearchTarget,
    ) -> BoxFuture<'a, Result<Vec<Candidate>, SourceError>> {
        Box::pin(self.search_albums(target))
    }

    fn enqueue<'a>(
        &'a self,
        task_id: &'a str,
        candidate_index: i64,
    ) -> BoxFuture<'a, Result<SourceHandle, SourceError>> {
        Box::pin(self.enqueue_pick(task_id, candidate_index))
    }

    fn poll<'a>(
        &'a self,
        handle: &'a SourceHandle,
    ) -> BoxFuture<'a, Result<TransferProgress, SourceError>> {
        Box::pin(self.progress(handle))
    }

    fn landed<'a>(
        &'a self,
        handle: &'a SourceHandle,
    ) -> BoxFuture<'a, Result<Landed, SourceError>> {
        Box::pin(self.landed_files(handle))
    }

    fn discard<'a>(&'a self, handle: &'a SourceHandle) -> BoxFuture<'a, Result<bool, SourceError>> {
        Box::pin(self.give_up(handle))
    }

    fn abort<'a>(&'a self, handle: &'a SourceHandle) -> BoxFuture<'a, Result<bool, SourceError>> {
        Box::pin(self.give_up(handle))
    }

    fn queue_is_local(&self) -> bool {
        true
    }
}
