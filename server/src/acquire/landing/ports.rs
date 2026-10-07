//! What the landing needs from the library, as one port.
//!
//! The landing decides; the library looks releases up, knows what it
//! already holds, and publishes. Production binds this to the library
//! bundle (MusicBrainz through the identify release source, the catalog,
//! and the staged publisher); tests bind a scripted fake.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use futures_util::future::BoxFuture;

use crate::library::identify::sources::{ReleaseHit, SourceError};
use crate::library::matching::Release;

/// Tracks the library already holds for one release group.
#[derive(Debug, Clone, Default)]
pub struct OwnedTracks {
    /// Release-track MBIDs (lowercase).
    pub release_tracks: HashSet<String>,
    /// Recording MBIDs (lowercase).
    pub recordings: HashSet<String>,
}

/// One landed file to publish, and the release track it is.
#[derive(Debug, Clone)]
pub struct ImportFile {
    pub path: PathBuf,
    /// Index into [`ImportRequest::release`]'s tracks.
    pub track: usize,
}

/// A library file holding a track an upgrade may replace.
#[derive(Debug, Clone)]
pub struct OwnedCopy {
    pub track_id: String,
    /// Quality tier of the file (see [`super::quality::tier_for`]).
    pub tier: &'static str,
}

/// Files moved into the recycle bin, as (where it was, where it is now).
pub use crate::library::mutations::Recycled;

/// A verified download to publish into the library.
#[derive(Debug, Clone)]
pub struct ImportRequest {
    pub task_id: String,
    /// Name of the hidden staging folder, when it must differ from the
    /// task id (a held file imported while its task may land again).
    pub staging: Option<String>,
    /// The matched release; files are tagged from it.
    pub release: Release,
    pub files: Vec<ImportFile>,
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
}

/// Why an import did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportFailure {
    /// Our side cannot take files now (no usable root, disk, a publish
    /// already waiting on recovery). The download stays for a reimport.
    LocalFault(String),
    /// A file already sits where the import would go and the catalog
    /// cannot prove it is this track: hold for a person (v2
    /// `target_occupied`).
    Occupied(String),
}

impl ImportFailure {
    pub fn detail(&self) -> &str {
        match self {
            ImportFailure::LocalFault(detail) | ImportFailure::Occupied(detail) => detail,
        }
    }
}

/// The library as the landing sees it.
pub trait LandingLibrary: Send + Sync {
    /// Releases matching an album title and (unless blank) an artist.
    fn search<'a>(
        &'a self,
        title: &'a str,
        artist: &'a str,
    ) -> BoxFuture<'a, Result<Vec<ReleaseHit>, SourceError>>;

    /// One release with its tracklist.
    fn release<'a>(&'a self, mbid: &'a str) -> BoxFuture<'a, Result<Option<Release>, SourceError>>;

    /// The edition chosen for the library's copy of a release group, if
    /// any: a manual identity, else the edition pin, else the best fit
    /// ([`crate::acquire::edition::chosen_edition`], the same answer
    /// acquisition fetched by). The landing honours it over the edition
    /// the request carried.
    fn chosen_edition<'a>(&'a self, release_group_mbid: &'a str) -> BoxFuture<'a, Option<String>>;

    /// Tracks the library already holds for a release group.
    fn owned<'a>(&'a self, release_group_mbid: &'a str) -> BoxFuture<'a, OwnedTracks>;

    /// The worst tier the library holds for a release group, for the
    /// upgrade floor. `None` when it holds nothing.
    fn held_tier<'a>(&'a self, release_group_mbid: &'a str) -> BoxFuture<'a, Option<String>>;

    /// The copies an upgrade of one release track would replace: in the
    /// local album of `release_mbid` only, matched by release track, else
    /// by recording when no copy of the release track exists.
    fn owned_copies<'a>(
        &'a self,
        release_group_mbid: &'a str,
        release_mbid: &'a str,
        release_track_mbid: &'a str,
        recording_mbid: &'a str,
    ) -> BoxFuture<'a, Vec<OwnedCopy>>;

    /// Move library tracks' files into the recycle bin and take them out
    /// of the catalog, all or nothing. `actor` is the user the change is
    /// recorded for. Answers the moves, or a plain sentence on failure.
    fn recycle(
        &self,
        track_ids: Vec<String>,
        actor: String,
    ) -> BoxFuture<'_, Result<Recycled, String>>;

    /// Undo [`Self::recycle`]: put the files back and rescan their
    /// folders. False when a file could not be put back.
    fn put_back(&self, moved: Recycled, actor: String) -> BoxFuture<'_, bool>;

    /// AcoustID recordings heard in each file, by the caller's key. Empty
    /// when no AcoustID key is set or nothing could be looked up.
    fn fingerprints(
        &self,
        files: Vec<(String, PathBuf)>,
    ) -> BoxFuture<'_, HashMap<String, Vec<String>>>;

    /// The library root folders; landed paths inside them are refused.
    fn library_dirs(&self) -> Vec<PathBuf>;

    /// Publish verified files into the library.
    fn import(&self, request: ImportRequest)
    -> BoxFuture<'_, Result<ImportReceipt, ImportFailure>>;
}
