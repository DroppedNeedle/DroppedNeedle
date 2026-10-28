//! What the landing needs from the library, as one port.
//!
//! The landing decides; the library looks releases up, knows what it
//! already holds, and publishes. Production binds this to the library
//! bundle (MusicBrainz through the identify release source, the catalog,
//! and the staged publisher); tests bind a scripted fake.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use futures_util::future::BoxFuture;

use droppedneedle_library::identify::sources::{ReleaseHit, SourceError};
use droppedneedle_library::matching::Release;
use droppedneedle_providers::RequestPriority;

/// Tracks the library already holds for one release group.
#[derive(Debug, Clone, Default)]
pub struct OwnedTracks {
    /// Release-track MBIDs (lowercase).
    pub release_tracks: HashSet<String>,
    /// Recording MBIDs (lowercase).
    pub recordings: HashSet<String>,
}

pub use droppedneedle_library::import::ExtraSource as ExtraFile;

/// One landed file to publish, and the release track it is.
#[derive(Debug, Clone)]
pub struct ImportFile {
    pub path: PathBuf,
    /// Index into [`ImportRequest::release`]'s tracks.
    pub track: usize,
    /// Library tracks it replaces. Their files go to the recycle bin in
    /// the same publish, and only if this file is placed.
    pub replaces: Vec<String>,
}

pub use droppedneedle_library::import::ImportOrigin;

/// A library file holding a track an upgrade may replace.
#[derive(Debug, Clone)]
pub struct OwnedCopy {
    pub track_id: String,
    /// The album it sits in, and the release that album's identity names
    /// (empty when it names none).
    pub album_id: String,
    pub release_mbid: String,
    /// Quality tier of the file (see [`super::quality::tier_for`]).
    pub tier: &'static str,
}

/// A library track a landed file may replace as another version of a
/// release track, as the catalog has it now.
#[derive(Debug, Clone)]
pub struct VersionRow {
    /// It is on the planned copy (followed to the album it was merged
    /// into), and that copy still names the planned release.
    pub on_planned: bool,
    /// `indexed`, `missing` or `excluded`.
    pub availability: String,
    /// Already an alias of another track.
    pub alias: bool,
    /// Mapped to a track of the release being landed.
    pub on_release: bool,
    /// The app leaves its album's files as they are
    /// ([`crate::edition::copy_is_held`]).
    pub held: bool,
    /// Quality tier from the format, bit rate and depth on its row, which
    /// stay after its file is gone.
    pub tier: &'static str,
}

/// A verified download to publish into the library.
#[derive(Debug, Clone)]
pub struct ImportRequest {
    /// The download task, or the drop item, the files came from.
    pub task_id: String,
    pub origin: ImportOrigin,
    /// Name of the hidden staging folder, when it must differ from the
    /// task id (a held file imported while its task may land again).
    pub staging: Option<String>,
    /// The client's downloads folders (real paths) a download's files are
    /// read beneath, never through a link below them. `None` for files in
    /// the app's own folders, read by path.
    pub read_beneath: Option<Vec<PathBuf>>,
    /// The matched release; files are tagged from it.
    pub release: Release,
    /// The library album the files were fetched for, which they join.
    pub join: Option<droppedneedle_library::import::ImportJoin>,
    pub files: Vec<ImportFile>,
    /// Files filed with the album that are none of its tracks; their
    /// request indexes follow `files`.
    pub extras: Vec<ExtraFile>,
    /// The user the removal of replaced tracks is recorded for.
    pub actor: String,
}

/// What the library did with an import.
#[derive(Debug, Clone, Default)]
pub struct ImportReceipt {
    /// The publisher bundle that placed the files.
    pub bundle_id: String,
    /// The library album the files joined.
    pub album_id: String,
    /// Where each imported file now lives.
    pub paths: Vec<PathBuf>,
    /// Request files the library did not take, by index, with the
    /// reason: two files bound for one destination, or a destination
    /// already occupied. They are held; the rest imported.
    pub skipped: Vec<(usize, String)>,
    /// Request files not taken because a library track they replace could
    /// not be moved to the recycle bin, by index, with the reason. Their
    /// old copies stay.
    pub blocked: Vec<(usize, String)>,
    /// Of `paths`, the files placed in a folder that keeps its own tags:
    /// their tags are as they came.
    pub tags_kept: usize,
}

/// Why an import did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportFailure {
    /// Our side cannot take files now (no usable root, disk, a publish
    /// already waiting on recovery). The download stays for a reimport.
    LocalFault(String),
    /// A file already sits where the import would go and the catalog
    /// cannot prove it is this track: hold for a person.
    Occupied(String),
    /// The library's import naming script gave no usable path for a file.
    /// The download stays for a reimport once the script is fixed.
    NamingScript(String),
    /// The library folder the files go to is not there (the folder). The
    /// download waits until it is back.
    FolderMissing(String),
}

impl ImportFailure {
    pub fn detail(&self) -> &str {
        match self {
            ImportFailure::LocalFault(detail)
            | ImportFailure::Occupied(detail)
            | ImportFailure::NamingScript(detail)
            | ImportFailure::FolderMissing(detail) => detail,
        }
    }
}

/// The library as the landing sees it. MusicBrainz is asked at the
/// caller's `priority`: user priority when a person waits on the answer,
/// background for the landing and other work nobody watches.
pub trait LandingLibrary: Send + Sync {
    /// Releases matching an album title and (unless blank) an artist.
    fn search<'a>(
        &'a self,
        title: &'a str,
        artist: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>>;

    /// One release with its tracklist.
    fn release<'a>(
        &'a self,
        mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<Release>, SourceError>>;

    /// The id a recording MBID resolves to after merges; `None` when
    /// MusicBrainz has no such recording.
    fn canonical_recording<'a>(
        &'a self,
        mbid: &'a str,
        priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Option<String>, SourceError>>;

    /// The edition chosen for the library's copy of a release group, if
    /// any: a manual identity, else the edition pin, else the best fit
    /// ([`crate::edition::chosen_edition`], the same answer
    /// acquisition fetched by). The landing honours it over the edition
    /// the request carried.
    fn chosen_edition<'a>(&'a self, release_group_mbid: &'a str) -> BoxFuture<'a, Option<String>>;

    /// Tracks the library already holds for a release group; with
    /// `album`, only in that library album (followed to the album it was
    /// merged into).
    fn owned<'a>(
        &'a self,
        release_group_mbid: &'a str,
        album: Option<&'a str>,
    ) -> BoxFuture<'a, OwnedTracks>;

    /// The worst tier the library holds for a release group (with
    /// `album`, in that album only), for the upgrade floor. `None` when it
    /// holds nothing.
    fn held_tier<'a>(
        &'a self,
        release_group_mbid: &'a str,
        album: Option<&'a str>,
    ) -> BoxFuture<'a, Option<String>>;

    /// The copies an upgrade of one release track would replace: in the
    /// local album of `release_mbid` only (with `album`, in that album
    /// only), matched by release track, else by recording when no copy of
    /// the release track exists.
    fn owned_copies<'a>(
        &'a self,
        release_group_mbid: &'a str,
        album: Option<&'a str>,
        release_mbid: &'a str,
        release_track_mbid: &'a str,
        recording_mbid: &'a str,
    ) -> BoxFuture<'a, Vec<OwnedCopy>>;

    /// The library track `track_id` as a file landed for `planned` on
    /// `release` would replace it ("Replace tracks you have in another
    /// version"). `None` when it is gone or cannot be read.
    fn version_row<'a>(
        &'a self,
        track_id: &'a str,
        planned: &'a crate::downloads::plan::PlannedAlbum,
        release: &'a Release,
    ) -> BoxFuture<'a, Option<VersionRow>>;

    /// AcoustID recordings heard in each file, by the caller's key. Empty
    /// when no AcoustID key is set or nothing could be looked up.
    fn fingerprints(
        &self,
        files: Vec<(String, PathBuf)>,
    ) -> BoxFuture<'_, HashMap<String, Vec<String>>>;

    /// Releases of one release group, best known first, for matching
    /// files a person assigned to that album by hand. Empty when the
    /// source cannot list them.
    fn group_releases<'a>(
        &'a self,
        _release_group_mbid: &'a str,
        _priority: RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// AcoustID releases each file's audio appears on, by the caller's
    /// key. Empty when no AcoustID key is set or nothing was heard.
    fn fingerprint_releases(
        &self,
        _files: Vec<(String, PathBuf)>,
    ) -> BoxFuture<'_, HashMap<String, Vec<String>>> {
        Box::pin(async { HashMap::new() })
    }

    /// The library root folders; landed paths inside them are refused.
    fn library_dirs(&self) -> Vec<PathBuf>;

    /// The library folder a plain download goes to, which downloads wait
    /// for ([`crate::library_ready`]): the first folder not set to
    /// Excluded nor left out whose policy is Automatic, else the first not
    /// set to Excluded. `None` when no folder takes imports. Blocking: it
    /// resolves the folders.
    fn import_folder(&self) -> Option<PathBuf> {
        None
    }

    /// Publish verified files into the library.
    fn import(&self, request: ImportRequest)
    -> BoxFuture<'_, Result<ImportReceipt, ImportFailure>>;

    /// Whether every write putting this download task's files into the
    /// library is over. An error means the publish journal could not be
    /// read, which is not an answer.
    fn publish_settled<'a>(&'a self, task_id: &'a str) -> BoxFuture<'a, Result<bool, String>>;

    /// The content hashes (lower-case hex SHA-256) of the files that writes
    /// staged under `staging` put into the library, once every write from
    /// that staging folder is over; `None` while one still waits for
    /// recovery. A file whose hash is listed is in the library, so a
    /// person's import cut off by a restart never places it twice. An error
    /// means the publish journal could not be read.
    fn placed_sources<'a>(
        &'a self,
        staging: &'a str,
    ) -> BoxFuture<'a, Result<Option<HashSet<String>>, String>>;
}
