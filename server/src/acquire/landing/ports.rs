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

/// A verified download to publish into the library.
#[derive(Debug, Clone)]
pub struct ImportRequest {
    pub task_id: String,
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

    /// The edition the owner pinned for the library's copy of a release
    /// group, if any. The landing honours it over the edition the request
    /// carried: the pin is the album's remembered choice.
    fn edition_pin<'a>(&'a self, release_group_mbid: &'a str) -> BoxFuture<'a, Option<String>>;

    /// Tracks the library already holds for a release group.
    fn owned<'a>(&'a self, release_group_mbid: &'a str) -> BoxFuture<'a, OwnedTracks>;

    /// The worst tier the library holds for a release group, for the
    /// upgrade floor. `None` when it holds nothing.
    fn held_tier<'a>(&'a self, release_group_mbid: &'a str) -> BoxFuture<'a, Option<String>>;

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
