//! Deemix's answers, decoded to the fields DroppedNeedle reads.
//!
//! Verified against deemix-webui 4.8.0 / deemix 3.14.1, image digest
//! sha256:aff6120619685b339a704b49bec13469684dbec2c7e39cd7bdf0014217dd1d0a,
//! 2026-11-21 and 2026-11-22, on a free Deezer account: `/connect` signed
//! out, offering a saved sign-in, signed in and with a queue; `loginArl`
//! signing in and already signed in; album search and tracklists; adds
//! refused, queued and queueing nothing; and the queue through a track,
//! two albums, a restart, a kill and a cancel. The answers are in
//! `tests/fixtures/deemix/`. The error ids, an item ending `withErrors` or
//! `failed`, and a FLAC item's files are read from that version's source
//! and not recorded: a free account cannot make them.
//!
//! Only named fields are decoded and unknown ones are passed over. Some
//! fields are never named on purpose, so they are never decoded or kept:
//! `/connect`'s `queue` (it can run to megabytes), `spotifySettings`,
//! `currentUser.license_token`, and everything `loginArl` echoes back
//! beside its `status`, the ARL among them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::Deserialize;
use serde::de::{Deserializer, Error as _, IgnoredAny, MapAccess, Visitor};

use super::client::{Bitrate, ItemKind, QueueUuid};

/// The longest ARL taken. Deezer's are 192 characters.
const MAX_ARL_LEN: usize = 256;

/// Deemix's own Deezer sign-in (the `arl` cookie), as `/connect` offers it
/// to a signed-out session. It is lowercase hex only, is never cloned,
/// serialized or printed, and lives for the one sign-in request it is read
/// for.
pub struct Arl(String);

impl Arl {
    /// The ARL in `text`, when it is 1 to 256 lowercase hex characters.
    #[must_use]
    pub fn parse(text: &str) -> Option<Arl> {
        let hex = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
        (!text.is_empty() && text.len() <= MAX_ARL_LEN && text.bytes().all(hex))
            .then(|| Arl(text.to_owned()))
    }

    /// The value, for the sign-in request's body only.
    pub(crate) fn secret(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Arl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Arl(<redacted>)")
    }
}

/// An offered ARL that is not one reads as no ARL offered. A refusal never
/// quotes the value.
fn offered_arl<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Arl>, D::Error> {
    let text = Option::<String>::deserialize(deserializer)?;
    Ok(text.as_deref().and_then(Arl::parse))
}

/// `GET /api/connect`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Connect {
    /// The versions Deemix runs.
    pub update: Versions,
    /// True while this session is not signed in to Deezer.
    pub autologin: bool,
    /// The signed-in account. A signed-out session answers `{}`.
    #[serde(default)]
    pub current_user: Option<CurrentUser>,
    /// Whether Deezer serves Deemix's country.
    pub deezer_available: DeezerAvailable,
    settings_data: SettingsData,
    /// Sent only in single-user mode and only to a signed-out session.
    #[serde(default)]
    single_user: Option<SingleUser>,
}

impl Connect {
    /// Whether this session is signed in to Deezer. Deemix answers
    /// `autologin` as "not signed in".
    #[must_use]
    pub fn signed_in(&self) -> bool {
        !self.autologin
    }

    /// The settings DroppedNeedle checks.
    #[must_use]
    pub fn settings(&self) -> &DeemixSettings {
        &self.settings_data.settings
    }

    /// Whether the account can download MP3 320, when Deemix says.
    #[must_use]
    pub fn can_stream_hq(&self) -> Option<bool> {
        self.current_user
            .as_ref()
            .and_then(|user| user.can_stream_hq)
    }

    /// Whether the account can download FLAC, when Deemix says.
    #[must_use]
    pub fn can_stream_lossless(&self) -> Option<bool> {
        self.current_user
            .as_ref()
            .and_then(|user| user.can_stream_lossless)
    }

    /// Whether Deemix answered as in single-user mode: it sends
    /// `singleUser` to a signed-out session, with or without a saved
    /// sign-in, and only in that mode.
    #[must_use]
    pub fn single_user_mode(&self) -> bool {
        self.single_user.is_some()
    }

    /// The saved Deezer sign-in Deemix offered, taken out of the answer.
    pub fn take_arl(&mut self) -> Option<Arl> {
        self.single_user
            .as_mut()
            .and_then(|single| single.arl.take())
    }
}

/// `/connect`'s `update`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Versions {
    pub webui_version: String,
    pub deemix_version: String,
}

/// `/connect`'s `currentUser`, the two flags only. Both are absent while
/// signed out; an absent flag means not known, never "cannot".
#[derive(Debug, Clone, Default, Deserialize)]
pub struct CurrentUser {
    #[serde(default)]
    pub can_stream_hq: Option<bool>,
    #[serde(default)]
    pub can_stream_lossless: Option<bool>,
}

/// `/connect`'s `deezerAvailable`, a string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum DeezerAvailable {
    #[serde(rename = "yes")]
    Yes,
    #[serde(rename = "no")]
    No,
    /// Deemix could not reach deezer.com when it first asked.
    #[serde(rename = "no-network")]
    NoNetwork,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct SettingsData {
    settings: DeemixSettings,
}

/// The Deemix settings DroppedNeedle checks. Deemix fills every missing
/// setting with its default before it answers, so each one is required.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeemixSettings {
    pub overwrite_file: Overwrite,
    pub fallback_bitrate: bool,
    pub fallback_search: bool,
    #[serde(rename = "fallbackISRC")]
    pub fallback_isrc: bool,
    /// Whether a command is set to run after each download. The command
    /// itself is not kept.
    #[serde(deserialize_with = "is_set")]
    pub execute_command: bool,
    /// Where Deemix writes, on its side.
    pub download_location: String,
}

fn is_set<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(!String::deserialize(deserializer)?.trim().is_empty())
}

/// Deemix's "Overwrite files" setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub enum Overwrite {
    #[serde(rename = "y")]
    Yes,
    #[serde(rename = "n")]
    No,
    #[serde(rename = "e")]
    NoExtensionCheck,
    #[serde(rename = "b")]
    KeepBoth,
    #[serde(rename = "t")]
    OnlyTags,
    #[serde(rename = "l")]
    OnlyLowerBitrates,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct SingleUser {
    #[serde(default, deserialize_with = "offered_arl")]
    arl: Option<Arl>,
}

impl fmt::Debug for SingleUser {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SingleUser")
            .field("arl_offered", &self.arl.is_some())
            .finish()
    }
}

/// `POST /api/loginArl`'s `status`, the one field read from its answer.
/// A number this version does not know reads as [`LoginStatus::Unknown`];
/// anything but a number does not decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(from = "i64")]
pub enum LoginStatus {
    /// Deezer is not available (-1); Deemix erased its saved sign-in.
    NotAvailable,
    /// The sign-in failed (0); Deemix erased its saved sign-in.
    Failed,
    /// Signed in (1).
    Success,
    /// This session was signed in already (2).
    AlreadyLogged,
    /// Any other number.
    Unknown,
}

impl From<i64> for LoginStatus {
    fn from(code: i64) -> Self {
        match code {
            -1 => LoginStatus::NotAvailable,
            0 => LoginStatus::Failed,
            1 => LoginStatus::Success,
            2 => LoginStatus::AlreadyLogged,
            _ => LoginStatus::Unknown,
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct LoginAnswer {
    pub(crate) status: LoginStatus,
}

/// `GET /api/search?type=album`: Deezer's album search, passed through.
#[derive(Debug, Deserialize)]
pub(crate) struct SearchAnswer {
    pub(crate) data: Vec<SearchAlbum>,
    /// Set when Deezer's search failed; `data` is then empty.
    #[serde(default)]
    pub(crate) error: Option<String>,
}

/// One album search hit.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SearchAlbum {
    pub id: u64,
    pub title: String,
    pub artist: Artist,
}

/// An album's artist, by name.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Artist {
    pub name: String,
}

/// `GET /api/getTracklist?type=album`. An id Deezer does not know answers
/// `{"tracks":[]}`, so the album's own fields wait for
/// [`TracklistAnswer::into_tracklist`].
#[derive(Debug, Deserialize)]
pub(crate) struct TracklistAnswer {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    artist: Option<Artist>,
    #[serde(default)]
    explicit_lyrics: Option<bool>,
    tracks: Vec<TracklistRow>,
}

/// A row of `tracks`: a track, or the `disc_separator` Deemix puts before
/// each disc of an album with more than one.
#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum TracklistRow {
    #[serde(rename = "track")]
    Track(Track),
    #[serde(other)]
    Other,
}

impl TracklistAnswer {
    /// The album, or `None` when the answer holds no track. An album with
    /// tracks but no id, title or artist is not a shape this reads.
    pub(crate) fn into_tracklist(self) -> Result<Option<Tracklist>, &'static str> {
        let tracks: Vec<Track> = self
            .tracks
            .into_iter()
            .filter_map(|row| match row {
                TracklistRow::Track(track) => Some(track),
                TracklistRow::Other => None,
            })
            .collect();
        if tracks.is_empty() {
            return Ok(None);
        }
        let (Some(id), Some(title), Some(artist)) = (self.id, self.title, self.artist) else {
            return Err("a tracklist with tracks but no album id, title or artist");
        };
        Ok(Some(Tracklist {
            id,
            title,
            artist,
            explicit_lyrics: self.explicit_lyrics,
            tracks,
        }))
    }
}

/// A Deezer album and its tracks, disc separators dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tracklist {
    pub id: u64,
    pub title: String,
    pub artist: Artist,
    pub explicit_lyrics: Option<bool>,
    pub tracks: Vec<Track>,
}

/// One track of a tracklist.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Track {
    pub id: u64,
    pub disk_number: u32,
    pub track_position: u32,
    /// Whole seconds.
    #[serde(default)]
    pub duration: Option<u32>,
    #[serde(default, deserialize_with = "non_empty")]
    pub isrc: Option<String>,
    pub title_short: String,
    /// "(Remastered 2012)", "Live" and the like; empty when there is none.
    #[serde(default)]
    pub title_version: String,
    /// Whether Deezer serves the track where Deemix runs.
    pub readable: bool,
}

fn non_empty<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    let text = Option::<String>::deserialize(deserializer)?;
    Ok(text.filter(|text| !text.trim().is_empty()))
}

/// `POST /api/addToQueue`. A refusal answers `result: false` with the
/// error's name; a success lists what it queued under `data.obj`.
#[derive(Debug, Deserialize)]
pub(crate) struct AddAnswer {
    pub(crate) result: bool,
    #[serde(default)]
    pub(crate) errid: Option<AddRefusal>,
    #[serde(default)]
    pub(crate) data: Option<AddData>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AddData {
    /// The items queued, one per link sent.
    #[serde(default)]
    pub(crate) obj: Option<Vec<AddedItem>>,
}

/// An item an add queued, by its uuid only.
#[derive(Debug, Deserialize)]
pub(crate) struct AddedItem {
    pub(crate) uuid: String,
}

/// Why Deemix refused an add.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(crate) enum AddRefusal {
    NotLoggedIn,
    CantStream,
    #[serde(other)]
    Other,
}

/// What an accepted add did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddOutcome {
    /// The item went into Deemix's queue under this uuid.
    Queued(QueueUuid),
    /// Nothing was queued: the item is there already, or Deemix could not
    /// build it (a network failure on its side answers the same way).
    NothingAdded,
}

/// `POST /api/removeFromQueue`.
#[derive(Debug, Deserialize)]
pub(crate) struct RemoveAnswer {
    pub(crate) result: bool,
}

/// `GET /api/getQueue`: every item Deemix holds, finished ones included,
/// the uuids waiting to start, and the item it is downloading.
///
/// The `queue` map is read leniently: an item under a key of the shape
/// DroppedNeedle adds (`^(album|track)_\d+_(1|3|9)$`) is decoded strictly,
/// and any other key (a Spotify playlist, a shape an older Deemix saved) is
/// only noted as present.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Queue {
    queue: QueueItems,
    /// Uuids waiting to start, in order. The item being downloaded has
    /// already left it, and a restart can lose a waiting one from it.
    pub queue_order: Vec<String>,
    /// Absent while nothing downloads.
    #[serde(default, deserialize_with = "current_item")]
    current: Option<CurrentItem>,
}

impl Queue {
    /// The item under `uuid`.
    #[must_use]
    pub fn item(&self, uuid: &QueueUuid) -> Option<&QueueItem> {
        self.queue.ours.get(uuid.as_str())
    }

    /// Every item of the shape DroppedNeedle adds, by uuid.
    pub fn items(&self) -> impl Iterator<Item = &QueueItem> {
        self.queue.ours.values()
    }

    /// Whether the queue holds `uuid`, of any shape.
    #[must_use]
    pub fn holds(&self, uuid: &str) -> bool {
        self.queue.keys.contains(uuid)
    }

    /// The item Deemix is downloading. One that has already left the
    /// queue (it names a cancelled item for a few seconds after the
    /// cancel) counts as nothing current.
    #[must_use]
    pub fn current(&self) -> Option<&CurrentItem> {
        self.current
            .as_ref()
            .filter(|current| self.holds(&current.uuid))
    }
}

/// The `queue` map: every key, and the items under keys of our shape.
#[derive(Debug, Default)]
struct QueueItems {
    keys: BTreeSet<String>,
    ours: BTreeMap<String, QueueItem>,
}

impl<'de> Deserialize<'de> for QueueItems {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Items;

        impl<'de> Visitor<'de> for Items {
            type Value = QueueItems;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of queue items")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<QueueItems, A::Error> {
                let mut items = QueueItems::default();
                while let Some(key) = map.next_key::<String>()? {
                    if QueueUuid::parse(&key).is_some() {
                        let item: QueueItem = map.next_value()?;
                        if item.uuid.as_str() != key {
                            return Err(A::Error::custom("a queue item under another uuid"));
                        }
                        items.ours.insert(key.clone(), item);
                    } else {
                        map.next_value::<IgnoredAny>()?;
                    }
                    items.keys.insert(key);
                }
                Ok(items)
            }
        }

        deserializer.deserialize_map(Items)
    }
}

/// One album or track item of the queue. A waiting item has only the
/// fields that describe it, so the counters, files and errors are absent
/// until it starts.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub uuid: QueueUuid,
    #[serde(rename = "type")]
    pub kind: ItemKind,
    #[serde(deserialize_with = "deezer_id")]
    pub id: u64,
    pub bitrate: Bitrate,
    pub status: ItemStatus,
    /// How many tracks the item holds.
    pub size: u32,
    /// Tracks finished.
    #[serde(default)]
    pub downloaded: Option<u32>,
    /// Tracks that failed.
    #[serde(default)]
    pub failed: Option<u32>,
    /// The item's folder on Deemix's side: the album folder, or the
    /// download folder for a track. Empty until the first track lands.
    pub extras_path: String,
    /// The tracks written, in the order they finished.
    #[serde(default)]
    pub files: Option<Vec<ItemFile>>,
    #[serde(default)]
    pub errors: Option<Vec<ItemError>>,
}

/// A queue item's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemStatus {
    InQueue,
    Downloading,
    Completed,
    /// Finished with at least one track failed.
    WithErrors,
    /// Finished with every track failed.
    Failed,
    /// A state this version does not know.
    #[serde(other)]
    Unknown,
}

/// One track an item wrote, as Deemix reports it: the paths are on
/// Deemix's side, as it built them.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ItemFile {
    pub path: String,
    /// The path below the item's `extrasPath`.
    pub filename: String,
    pub data: FileTrack,
}

/// The Deezer track a file holds.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FileTrack {
    #[serde(deserialize_with = "deezer_id")]
    pub id: u64,
}

/// One error an item met.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct ItemError {
    /// Set on a track's download failure; absent on other errors.
    #[serde(default)]
    pub errid: Option<ErrorId>,
    #[serde(rename = "type")]
    pub kind: ErrorKind,
}

/// What an error was met on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorKind {
    /// Downloading a track.
    Track,
    /// The steps after the downloads (covers, playlists).
    Post,
    #[serde(other)]
    Other,
}

/// A track download failure's `errid`. Deemix adds `NoAlternative` to the
/// name when no other copy of the track was found either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(from = "String")]
pub struct ErrorId {
    pub cause: ErrorCause,
    pub no_alternative: bool,
}

impl From<String> for ErrorId {
    fn from(errid: String) -> Self {
        let (name, no_alternative) = match errid.strip_suffix("NoAlternative") {
            Some(name) => (name, true),
            None => (errid.as_str(), false),
        };
        let cause = match name {
            "notOnDeezer" => ErrorCause::NotOnDeezer,
            "notEncoded" => ErrorCause::NotEncoded,
            "wrongBitrate" => ErrorCause::WrongBitrate,
            "wrongLicense" => ErrorCause::WrongLicense,
            "no360RA" => ErrorCause::No360Ra,
            "notAvailable" => ErrorCause::NotAvailable,
            "noSpaceLeft" => ErrorCause::NoSpaceLeft,
            "albumDoesntExists" => ErrorCause::AlbumDoesntExist,
            "notLoggedIn" => ErrorCause::NotLoggedIn,
            "wrongGeolocation" => ErrorCause::WrongGeolocation,
            _ => ErrorCause::Other,
        };
        ErrorId {
            cause,
            no_alternative,
        }
    }
}

/// Why a track failed, by the `errid` names deemix 3.14.1 raises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCause {
    NotOnDeezer,
    NotEncoded,
    /// Not found at the bitrate asked for.
    WrongBitrate,
    /// The account can't stream it at the bitrate asked for.
    WrongLicense,
    No360Ra,
    NotAvailable,
    NoSpaceLeft,
    AlbumDoesntExist,
    /// The session is not signed in.
    NotLoggedIn,
    /// The account can't stream it from its country.
    WrongGeolocation,
    /// A name this version does not know.
    Other,
}

/// `getQueue`'s `current`: the item being downloaded, with its progress.
#[derive(Debug, Clone, PartialEq)]
pub struct CurrentItem {
    /// Any uuid, ours or not.
    pub uuid: String,
    /// The item's title, when it has one.
    pub title: Option<String>,
    /// The progress, read only for an item of the shape DroppedNeedle adds.
    pub progress: Option<CurrentProgress>,
}

/// How far the current item has got.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CurrentProgress {
    pub downloaded: u32,
    pub failed: u32,
    /// Percent.
    pub progress: f64,
    /// The tracks finished so far, in the order they finished.
    pub files: Vec<ItemFile>,
}

#[derive(Deserialize)]
struct CurrentHead {
    uuid: String,
    #[serde(default)]
    title: Option<Text>,
}

/// A string, or any other value read as none.
#[derive(Deserialize)]
#[serde(untagged)]
enum Text {
    Text(String),
    Other(IgnoredAny),
}

/// `current` is buffered so its uuid decides how much of it is read: an
/// item DroppedNeedle did not add may be of any shape.
fn current_item<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<CurrentItem>, D::Error> {
    let Some(value) = Option::<serde_json::Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let head = CurrentHead::deserialize(&value).map_err(D::Error::custom)?;
    let progress = match QueueUuid::parse(&head.uuid) {
        Some(_) => Some(CurrentProgress::deserialize(&value).map_err(D::Error::custom)?),
        None => None,
    };
    Ok(Some(CurrentItem {
        uuid: head.uuid,
        title: match head.title {
            Some(Text::Text(title)) => Some(title),
            Some(Text::Other(_)) | None => None,
        },
        progress,
    }))
}

/// A Deezer id, sent as a number or as a string of digits.
fn deezer_id<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Id {
        Number(u64),
        Text(String),
    }
    match Id::deserialize(deserializer)? {
        Id::Number(id) => Ok(id),
        Id::Text(text) if !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()) => text
            .parse()
            .map_err(|_| D::Error::custom("a Deezer id out of range")),
        Id::Text(_) => Err(D::Error::custom("a Deezer id that is not a number")),
    }
}
