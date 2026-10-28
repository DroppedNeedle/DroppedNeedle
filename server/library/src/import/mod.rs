//! Importing files from outside the library: the publisher seam a
//! finished download comes in through.
//!
//! The landing (in acquisition) decides that a download is the requested
//! release and which file is which track. This module puts those files in
//! the library the same way every other managed write happens: as one
//! staged publisher bundle.
//!
//! 1. Each file is copied into a hidden import folder inside the library
//!    root it is going to (`.droppedneedle-management-import/<task>/`).
//!    The scan skips the hidden prefix, and the publisher's rename then
//!    never crosses a filesystem, whatever disk the download client used.
//!    A plain download goes to the first root whose policy at every
//!    file's destination is Automatic, else to the first root that is not
//!    Excluded; joins, upgrades, drop imports and downloads for an album
//!    the library already holds always go to the latter.
//! 2. One bundle moves every copy to the path the import naming script gives
//!    and writes the release's full tag set on the way. A file whose
//!    destination is in a folder that keeps its own tags (the Local
//!    metadata policy, of the folder or a path rule) is placed as it came,
//!    its tags untouched; the match is recorded all the same.
//! 3. The publisher's catalog commit adds the tracks to the catalog in the
//!    same transaction (from the published files' own tags, exactly as a
//!    scan would index them), so no rescan is needed and nothing shows in
//!    the library before its file is in place.
//! 4. In that same transaction the album and its tracks get an automatic
//!    MusicBrainz identity from the match, unless a person decided them
//!    ([`crate::identity::adopt_import`]), and the album is queued for
//!    automatic management. A stop at any point leaves both or neither.
//!    The release document the match names is kept for later retags
//!    before anything moves, so the commit makes no outside call.
//!
//! A file that replaces library tracks (an upgrade, or another version of
//! its track) takes their files away in the same bundle: every old file is
//! set aside before any new file is placed, the commit marks the old
//! tracks missing before it adds the new ones, and only then does each old
//! file move into the recycle bin. A stop at any point leaves the old file
//! or the new one in place. A file held for any reason keeps its old
//! copies where they are.
//!
//! Files fetched for a library copy (an [`ImportJoin`]: "Get missing
//! tracks", or an upgrade of that copy's files) join that copy instead of
//! being filed by their tags, which may name another edition. Under the
//! publish lock the copy is followed to the album it was merged into; the
//! files join it while it names the release they were fetched for
//! (placed on its edition) or another release of the same album (not
//! placed). Otherwise they are filed by their tags. A joined file goes into
//! the copy's own folder when its files sit in one, and is locked to the
//! album as the album tools lock a moved track. A row at its destination
//! keeps its id for the new file only when it is on that album, not
//! excluded and not a track the file replaces as another version; any
//! other row there gives up its path and keeps its id and history. The
//! commit applies the same rule again, and on an album whose match is a
//! person's it places each file on its release track
//! ([`crate::operations::choice::place_imported`]); what people
//! made on a replaced track moves to the file that replaced it. The join
//! and each file's place ride in the provenance record, so a commit
//! resumed after a crash joins and places them the same way.
//!
//! An import is not an edit of library files, so it records no undo
//! snapshot and no first-management baseline: the files' first library
//! state is the one the import wrote.

pub mod naming;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use droppedneedle_kernel::db::{Db, Lane};

use super::clock::LibraryClock as _;
use super::matching::Release;
use super::publish::import_staging::{copy_in, remove_staging, staging_rel, staging_settled};
use super::publish::paths::{ResolvedRoot, left_out};
use super::publish::planner::{
    AlbumJoin, Capability, JoinedTrack, PlanBundle, PlanItem, PlanKind, ReleaseIdentity, Removal,
    RemoveItem, SealRecheck, SealedPreview,
};
use super::publish::publisher::{JoinedAlbum, joined_album};
use super::publish::remove::{self, Probe, Refusal, Target};
use super::publish::snapshots::sha256_hex;
use super::publish::{PublishError, TagUpdates};
use super::scan::models::EffectivePolicy;
use super::scan::roots::{LibraryRoot, RootRegistry};
use super::settings::LibrarySettings;
use super::tags::AudioFormat;
use super::wiring::LibraryCore;

/// Revisions the seal pins while profiles and naming have no store (the
/// same constants manual management uses).
const PINNED_REVISION: u64 = 1;

/// One file to import and the release track it is.
#[derive(Debug, Clone)]
pub struct ImportSource {
    pub path: PathBuf,
    /// Index into the release's tracks.
    pub track: usize,
    /// Library tracks this file replaces (an upgrade, or another version
    /// of its track). Their files go to the recycle bin in the bundle that
    /// places this one, and only when this one is placed.
    pub replaces: Vec<String>,
}

/// A file that came with an album but is none of the release's tracks (a
/// bonus track MusicBrainz lacks). It is filed with the album under its
/// own title and number, with no track identity (v2 drop import "extra
/// files kept alongside the album").
#[derive(Debug, Clone)]
pub struct ExtraSource {
    pub path: PathBuf,
    pub title: String,
    pub disc: u32,
    pub position: u32,
}

/// Where imported files come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImportOrigin {
    /// A finished download; `task_id` is its download task.
    #[default]
    Download,
    /// Files a person uploaded (drop import); `task_id` names the drop
    /// item and is not a download task.
    Drop,
}

impl ImportOrigin {
    /// The catalog's `ingest_source` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Download => "download",
            Self::Drop => "drop",
        }
    }
}

/// Verified files to put in the library.
#[derive(Debug, Clone)]
pub struct DownloadImport {
    /// Names the hidden import folder; for a download, its task id.
    pub task_id: String,
    pub origin: ImportOrigin,
    /// Staging folder name when it must differ from the task id.
    pub staging: Option<String>,
    pub release: Release,
    pub files: Vec<ImportSource>,
    /// Extra files, after `files` in request order.
    pub extras: Vec<ExtraSource>,
    /// The user the removal of replaced tracks is recorded for.
    pub actor: Option<String>,
}

/// The library album a download was fetched for, and its release as that
/// album's identity row named it when the fetch was planned.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportJoin {
    pub album_id: String,
    pub release_mbid: String,
    /// `(release track, track)`: the album's track each file on that
    /// release track replaces, another version of it. That track never
    /// lends the file its id, and what people made on it moves to the
    /// file in the catalog commit.
    pub replaced: Vec<(String, String)>,
}

/// Where the files went.
#[derive(Debug, Clone)]
pub struct ImportedAlbum {
    /// Empty when every file was skipped.
    pub bundle_id: String,
    pub album_id: String,
    pub paths: Vec<PathBuf>,
    /// Files not taken, by request index, with the reason: a second file
    /// bound for the same destination, or a destination already occupied.
    pub skipped: Vec<(usize, String)>,
    /// Files not taken because a track they replace could not be taken
    /// away, by request index, with the reason. Their old copies stay.
    pub blocked: Vec<(usize, String)>,
    /// Of `paths`, the files placed in a folder that keeps its own tags:
    /// their tags are as they came.
    pub tags_kept: usize,
}

/// Why an import did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportError {
    /// The library cannot take files now: no usable root, disk space, an
    /// unfinished managed write, or an I/O failure.
    LocalFault(String),
    /// A file already sits where an imported file would go.
    Occupied(String),
    /// The import naming script gave no usable path for a file.
    NamingScript(String),
    /// The library folder the files go to is not there (the folder).
    FolderMissing(String),
}

/// One planned file.
struct Placement {
    /// Index in the request.
    index: usize,
    source: PathBuf,
    /// The release track, or a stand-in for an extra file (empty ids).
    track: super::matching::ReleaseTrack,
    staged_rel: String,
    dest_rel: String,
    format: AudioFormat,
    tags: TagUpdates,
    /// The policy covering the final destination, set under the publish
    /// lock once a joined file's folder is known.
    policy: EffectivePolicy,
    /// The track id the plan pins, set under the publish lock.
    track_id: String,
    /// The tracks this file replaces, read before the publish lock, or
    /// why one of them cannot be taken away.
    replaces: Result<Vec<Target>, String>,
}

/// Finished downloads brought into the library, and the MusicBrainz and
/// AcoustID lookups the landing matches them with.
#[derive(Clone)]
pub struct Imports {
    /// The application database the landing reads its choices from.
    pub db: Db,
    /// The roots and the publish cell the import opens over.
    pub core: LibraryCore,
    pub(crate) identify_store: Arc<super::identify::sqlite::SqliteIdentifyStore>,
    pub(crate) settings: LibrarySettings,
    /// MusicBrainz releases for importing finished downloads: search and
    /// release lookups, identity-critical like identification, one source
    /// for background work and one for a person waiting.
    pub releases:
        super::identify::sources::Prioritised<Arc<dyn super::identify::sources::ReleaseSource>>,
    /// AcoustID for files being imported, keyed per call and keeping
    /// nothing; off while no AcoustID key is set.
    pub fingerprints: Arc<dyn super::identify::sources::FingerprintSource>,
}

impl Imports {
    /// Import a verified download as one staged bundle, filed by its
    /// tags. Blocking: callers run it off the async runtime.
    pub fn import_download(&self, import: &DownloadImport) -> Result<ImportedAlbum, ImportError> {
        self.import_download_joined(import, None)
    }

    /// Import a verified download as one staged bundle. With `join`, the
    /// files join the library album they were fetched for (see the module
    /// doc). Each source is opened by its path. Blocking: callers run it
    /// off the async runtime.
    pub fn import_download_joined(
        &self,
        import: &DownloadImport,
        join: Option<ImportJoin>,
    ) -> Result<ImportedAlbum, ImportError> {
        self.import_download_from(import, join, &|path| std::fs::File::open(path))
    }

    /// [`Self::import_download_joined`], with every source opened by
    /// `open_source` for its copy into staging (a download's files are opened
    /// beneath its client's folder, never through a link).
    pub fn import_download_from(
        &self,
        import: &DownloadImport,
        join: Option<ImportJoin>,
        open_source: &dyn Fn(&Path) -> std::io::Result<std::fs::File>,
    ) -> Result<ImportedAlbum, ImportError> {
        let fault = |message: String| ImportError::LocalFault(message);
        if import.files.is_empty() && import.extras.is_empty() {
            return Err(fault("nothing to import".to_owned()));
        }
        let registry = self.core.live_registry();
        // Settings that do not read never mean the default layout.
        let script = self
            .settings
            .library
            .read()
            .ok_or_else(|| fault("the library settings cannot be read".to_owned()))?
            .naming_script
            .clone();
        let task_dir = staging_rel(import.staging.as_deref().unwrap_or(&import.task_id));
        let mut sources = Vec::with_capacity(import.files.len() + import.extras.len());
        for file in &import.files {
            let track = import.release.tracks.get(file.track).ok_or_else(|| {
                fault("a file names a track the release does not have".to_owned())
            })?;
            sources.push((&file.path, track.clone(), file.replaces.as_slice()));
        }
        for extra in &import.extras {
            sources.push((
                &extra.path,
                super::matching::ReleaseTrack {
                    title: extra.title.clone(),
                    disc: extra.disc.max(1),
                    position: extra.position,
                    ..super::matching::ReleaseTrack::default()
                },
                &[],
            ));
        }
        let old_copies: Vec<Result<Vec<Target>, String>> = sources
            .iter()
            .map(|(_, _, replaces)| self.old_copies(&registry, replaces))
            .collect::<Result<_, _>>()?;
        let mut formats = Vec::with_capacity(sources.len());
        for (path, _, _) in &sources {
            formats.push(
                super::tags::format_for_path(path)
                    .map_err(|error| fault(format!("unsupported file: {error}")))?,
            );
        }
        let render = |root: &LibraryRoot| -> Result<Vec<String>, ImportError> {
            let root_path = root.path.to_string_lossy();
            sources
                .iter()
                .zip(&formats)
                .map(|((_, track, _), format)| {
                    let named = super::matching::ReleaseTrack {
                        disc: track.disc.max(1),
                        ..track.clone()
                    };
                    naming::render(
                        &script,
                        &naming::variables(&import.release, &named, format.as_str()),
                        &root_path,
                    )
                    .map_err(|error| ImportError::NamingScript(error.to_string()))
                })
                .collect()
        };
        // Joins, upgrades, drop imports and downloads for an album the
        // library already holds take the first folder that is not
        // excluded. A plain download takes the first folder whose policy
        // is automatic tags at every file's destination, else that same
        // first folder, where it keeps the tags it came with.
        let usable: Vec<&LibraryRoot> = registry
            .roots()
            .iter()
            .filter(|root| root.policy != EffectivePolicy::Excluded)
            .collect();
        let first = *usable
            .first()
            .ok_or_else(|| fault("no library root takes imports".to_owned()))?;
        let plain = join.is_none()
            && import.origin == ImportOrigin::Download
            && import.files.iter().all(|file| file.replaces.is_empty())
            && !self
                .db
                .read_blocking("import.group_held", |conn| {
                    Ok::<_, PublishError>(group_held(conn, &import.release.release_group_id)?)
                })
                .map_err(|error| fault(error.to_string()))?;
        let tagged = plain
            .then(|| {
                // A folder reached through a link into another takes no
                // managed writes.
                let resolved: Vec<ResolvedRoot> = registry
                    .roots()
                    .iter()
                    .map(|root| ResolvedRoot::of(&root.id, &root.path))
                    .collect();
                let left_out = left_out(&resolved);
                usable
                    .iter()
                    .filter(|root| !left_out.contains(&root.id))
                    .find_map(|root| {
                        let dests = render(root)
                            .inspect_err(|error| {
                                tracing::info!(
                                    root = root.id,
                                    ?error,
                                    "a download skips a folder the naming script gives no path in"
                                );
                            })
                            .ok()?;
                        dests
                            .iter()
                            .all(|dest| {
                                root.policy_for(&root.path.join(dest)) == EffectivePolicy::Automatic
                            })
                            .then_some((*root, dests))
                    })
            })
            .flatten();
        let (root, dests) = match tagged {
            Some(chosen) => chosen,
            None => (first, render(first)?),
        };
        let root = root.clone();
        let mut placements = Vec::with_capacity(sources.len());
        for (ordinal, ((((path, track, _), replaces), format), dest_rel)) in sources
            .into_iter()
            .zip(old_copies)
            .zip(formats)
            .zip(dests)
            .enumerate()
        {
            let track = &track;
            let mut tags = super::tags::picard::release_tags(&import.release, track);
            tags.retain(|name, values| {
                super::tags::TagField::from_name(name).is_some_and(|field| {
                    super::tags::save::accepts(&super::tags::TagEdit::new(field, values.clone()))
                })
            });
            if !super::tags::save::writable(format) {
                tags.clear();
            }
            let tags = TagUpdates::from_fields(tags)
                .map_err(|error| fault(format!("release tags: {error}")))?;
            placements.push(Placement {
                index: ordinal,
                source: path.clone(),
                track: track.clone(),
                staged_rel: format!("{task_dir}/{ordinal:03}.{}", format.as_str()),
                dest_rel,
                format,
                tags,
                policy: root.policy,
                track_id: String::new(),
                replaces,
            });
        }

        // The commit seals the match against this document and makes no
        // outside call, so it is kept first.
        self.identify_store
            .save_release(&import.release, Lane::Background)
            .map_err(|error| fault(format!("the release document was not kept: {error}")))?;

        let mut cell = self
            .core
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cell.refresh(&registry, &self.core.root_dirs)
            .map_err(|error| fault(error.to_string()))?;
        let open = cell.open().map_err(|error| fault(error.to_string()))?;
        let staged_dir = open
            .sandbox
            .resolve_no_symlink(&root.id, &task_dir)
            .map_err(|error| {
                if matches!(error, PublishError::FolderMissing) {
                    tracing::warn!(
                        root = root.id,
                        "a landing stopped: the library folder is not there"
                    );
                    return ImportError::FolderMissing(root.path.display().to_string());
                }
                fault(error.to_string())
            })?;
        let replaced = join
            .as_ref()
            .map(|join| join.replaced.clone())
            .unwrap_or_default();
        let join = join.map(|join| AlbumJoin {
            album_id: join.album_id,
            release_mbid: join.release_mbid,
            release_group_mbid: import.release.release_group_id.clone(),
        });
        // Whether an earlier landing still waits, the album the files
        // join, and the folder each joined file goes to, in one read.
        let (settled, joined, folders) = self
            .db
            .read_blocking("import.join_state", |conn| {
                let settled = staging_settled(conn, &task_dir)?;
                let joined = match &join {
                    Some(join) => joined_album(conn, join)?,
                    None => None,
                };
                let mut folders = Vec::with_capacity(placements.len());
                if let Some(joined) = &joined {
                    for placement in &placements {
                        folders.push(copy_folder(
                            conn,
                            &joined.album_id,
                            &root.id,
                            placement.track.disc,
                        )?);
                    }
                }
                Ok::<_, PublishError>((settled, joined, folders))
            })
            .map_err(|error| fault(error.to_string()))?;
        // A publish of an earlier landing that still waits for recovery
        // reads the provenance record in this folder: leave it as it is.
        if !settled {
            return Err(fault(
                "an earlier import of this download is waiting for recovery".to_owned(),
            ));
        }
        // A joined file goes beside the copy's own files.
        for (placement, folder) in placements.iter_mut().zip(folders) {
            if let (Some(folder), Some(name)) = (folder, Path::new(&placement.dest_rel).file_name())
            {
                placement.dest_rel = format!("{folder}/{}", name.to_string_lossy());
            }
        }
        // A folder that keeps its own tags takes the file as it came. The
        // policy is read here, at the final destination and under the lock,
        // so the bytes and the row's policy agree.
        for placement in &mut placements {
            placement.policy = root.policy_for(&root.path.join(&placement.dest_rel));
            if placement.policy == EffectivePolicy::LocalMetadata {
                placement.tags = TagUpdates::new();
            }
        }
        let bundle_id = self.core.ids.new_id();
        // A replaced track that left the catalog since it was read has no
        // file to take away.
        let wanted: Vec<String> = placements
            .iter()
            .filter_map(|placement| placement.replaces.as_ref().ok())
            .flatten()
            .map(|target| target.track_id.clone())
            .collect();
        let still_indexed = self
            .db
            .read_blocking("import.replaced_indexed", |conn| {
                Ok::<_, PublishError>(indexed_among(conn, &wanted)?)
            })
            .map_err(|error| fault(error.to_string()))?;
        // Files bound for a taken destination are held on their own (the
        // first of two files bound for one path keeps it); the rest import.
        // A file whose replaced tracks cannot be taken away is held too,
        // and its old copies stay where they are.
        let mut skipped = Vec::new();
        let mut blocked = Vec::new();
        let mut removals: Vec<RemoveItem> = Vec::new();
        let mut claimed = std::collections::HashSet::new();
        let mut claimed_tracks = std::collections::HashSet::new();
        let mut kept = Vec::with_capacity(placements.len());
        for mut placement in placements {
            let targets: Vec<Target> = match &placement.replaces {
                Ok(targets) => targets
                    .iter()
                    .filter(|target| still_indexed.contains(&target.track_id))
                    .cloned()
                    .collect(),
                Err(reason) => {
                    blocked.push((placement.index, reason.clone()));
                    continue;
                }
            };
            let key = super::publish::collision_key(&root.id, &placement.dest_rel);
            // The file's own old copies leave first, so the paths they hold
            // are not taken. Anything else at the destination, or under a
            // name that folds to it, is.
            let leaving: Vec<&Path> = targets
                .iter()
                .filter_map(|target| match &target.probe {
                    Probe::File { path, .. } => Some(path.as_path()),
                    Probe::Gone => None,
                })
                .collect();
            let occupied = open
                .sandbox
                .resolve_no_symlink(&root.id, &placement.dest_rel)
                .map(|dest| taken(&root.id, &dest, &leaving))
                .unwrap_or(true);
            let freed: Vec<String> = targets
                .iter()
                .map(|target| target.track_id.clone())
                .collect();
            let reason = if !claimed.insert(key.clone()) {
                Some(format!(
                    "another file of this download goes to {}",
                    placement.dest_rel
                ))
            } else if occupied {
                Some(format!("a file already sits at {}", placement.dest_rel))
            } else {
                match self
                    .db
                    .read_blocking("import.planned_track_id", |conn| {
                        Ok::<_, PublishError>(self.planned_track_id(
                            conn,
                            &root.id,
                            &placement.dest_rel,
                            join.is_some(),
                            joined.as_ref(),
                            &replaced,
                            &freed,
                        )?)
                    })
                    .map_err(|error| fault(error.to_string()))?
                {
                    Ok(track_id) => {
                        placement.track_id = track_id;
                        None
                    }
                    Err(reason) => Some(reason),
                }
            };
            if let Some(reason) = reason {
                skipped.push((placement.index, reason));
                continue;
            }
            // Only a file that imports takes its old copies away.
            let targets: Vec<Target> = targets
                .into_iter()
                .filter(|target| !claimed_tracks.contains(&target.track_id))
                .collect();
            if !targets.is_empty() {
                match remove::plan(
                    &self.db,
                    &open.sandbox,
                    &bundle_id,
                    &targets,
                    "FILE_RECYCLED",
                    import.actor.as_deref(),
                    None,
                    self.core.clock.now_system(),
                ) {
                    Ok(removal) => {
                        claimed_tracks.extend(targets.iter().map(|target| target.track_id.clone()));
                        removals.extend(removal.items);
                    }
                    Err(Refusal::Publish {
                        error:
                            error @ (PublishError::Store(_)
                            | PublishError::Interrupted(_)
                            | PublishError::TimedOut(_)),
                        ..
                    }) => return Err(fault(error.to_string())),
                    Err(refusal) => {
                        claimed.remove(&key);
                        blocked.push((placement.index, blocked_reason(&refusal)));
                        continue;
                    }
                }
            }
            kept.push(placement);
        }
        let placements = kept;
        if placements.is_empty() {
            return Ok(ImportedAlbum {
                bundle_id: String::new(),
                album_id: String::new(),
                paths: Vec::new(),
                skipped,
                blocked,
                tags_kept: 0,
            });
        }
        // Where a file sits on the join's release, when the import matched
        // that release (an extra file has no place).
        let joined_track = |track: &super::matching::ReleaseTrack| {
            join.as_ref()
                .filter(|join| {
                    !track.id.is_empty() && import.release.answers_to(&join.release_mbid)
                })
                .map(|_| JoinedTrack {
                    recording_mbid: track.recording_id.clone(),
                    release_track_mbid: track.id.clone(),
                    disc: track.disc.max(1),
                    position: track.position,
                    replaced: replaced
                        .iter()
                        .find(|(on, _)| on.eq_ignore_ascii_case(&track.id))
                        .map(|(_, old)| old.clone()),
                })
        };
        let dest_policy = |policy: EffectivePolicy| super::publish::planner::Adoption {
            policy,
            policy_revision: registry.policy_revision().to_owned(),
            download_task_id: (import.origin == ImportOrigin::Download)
                .then(|| import.task_id.clone()),
            source_path: None,
            ingest_source: import.origin.as_str().to_owned(),
            join: join.clone(),
            track: None,
        };
        let result = (|| {
            let mut items = Vec::with_capacity(placements.len());
            for placement in &placements {
                let staged = open
                    .sandbox
                    .resolve_no_symlink(&root.id, &placement.staged_rel)
                    .map_err(|error| fault(error.to_string()))?;
                let source = open_source(&placement.source)
                    .map_err(|error| fault(format!("copy into the library failed: {error}")))?;
                let fingerprint = copy_in(source, &staged)
                    .map_err(|error| fault(format!("copy into the library failed: {error}")))?;
                let track = &placement.track;
                items.push(PlanItem {
                    track_id: placement.track_id.clone(),
                    source_root: root.id.clone(),
                    source_rel: placement.staged_rel.clone(),
                    dest_root: root.id.clone(),
                    dest_rel: placement.dest_rel.clone(),
                    kind: PlanKind::Move,
                    staged_bytes_estimate: fingerprint.size + 65_536,
                    fingerprint,
                    identity: ReleaseIdentity {
                        release_mbid: import.release.id.clone(),
                        release_group_mbid: import.release.release_group_id.clone(),
                        recording_mbid: track.recording_id.clone(),
                        release_track_mbid: track.id.clone(),
                        album_identity_revision: 0,
                        mapping_revision: 0,
                    },
                    override_revision: 0,
                    capabilities: if placement.tags.is_empty() {
                        vec![Capability::SameRootMove]
                    } else {
                        vec![Capability::Metadata, Capability::SameRootMove]
                    },
                    format: placement.format,
                    managed_updates: placement.tags.clone(),
                    sidecars: Vec::new(),
                    adopt: Some(super::publish::planner::Adoption {
                        source_path: Some(placement.source.to_string_lossy().into_owned()),
                        track: joined_track(track),
                        ..dest_policy(placement.policy)
                    }),
                    replaces: None,
                    outputs: Vec::new(),
                    removals: Vec::new(),
                    sealed: None,
                });
            }
            write_provenance(&staged_dir, import, &bundle_id, &items)
                .map_err(|error| fault(format!("import provenance not written: {error}")))?;
            let bundle = PlanBundle {
                id: bundle_id.clone(),
                items,
                profile_revision: PINNED_REVISION,
                naming_revision: PINNED_REVISION,
                policy_revision: super::manage::policy_revision_u64(&registry),
                put_back: None,
                added: None,
                removal: (!removals.is_empty()).then(|| Removal {
                    reason_code: "FILE_RECYCLED".to_owned(),
                    actor: import.actor.clone(),
                    items: removals,
                    undo: None,
                    album_removed: None,
                }),
            };
            let token_hash = sha256_hex(bundle.id.as_bytes());
            let live = SealRecheck {
                fingerprints: bundle
                    .items
                    .iter()
                    .map(|item| (item.track_id.clone(), item.fingerprint.clone()))
                    .collect(),
                identities: bundle
                    .items
                    .iter()
                    .map(|item| (item.track_id.clone(), item.identity.clone()))
                    .collect(),
                overrides: bundle
                    .items
                    .iter()
                    .map(|item| (item.track_id.clone(), item.override_revision))
                    .collect(),
                profile_revision: PINNED_REVISION,
                naming_revision: PINNED_REVISION,
                policy_revision: bundle.policy_revision,
                settings_revision: PINNED_REVISION,
                today_day: self.core.clock.today_day(),
                token_hash: token_hash.clone(),
            };
            let sealed = SealedPreview::seal(
                bundle,
                token_hash,
                PINNED_REVISION,
                self.core.clock.today_day() + 1,
            );
            open.publisher
                .publish(&sealed, &live, &BTreeMap::new())
                .map_err(|error| match error {
                    PublishError::Collision(detail) => ImportError::Occupied(detail.message),
                    other => fault(other.to_string()),
                })?;
            Ok(sealed.bundle)
        })();
        // A publish that did not settle waits for recovery, which reads the
        // provenance record: the folder stays until the leftover sweep.
        match self.db.read_blocking("import.staging_settled", |conn| {
            Ok::<_, PublishError>(staging_settled(conn, &task_dir)?)
        }) {
            Ok(true) => remove_staging(&staged_dir),
            Ok(false) => tracing::warn!(
                dir = %staged_dir.display(),
                "import staging folder kept: its publish waits for recovery"
            ),
            Err(error) => tracing::warn!(
                dir = %staged_dir.display(),
                %error,
                "import staging folder kept: the publish journal is unreadable"
            ),
        }
        let bundle = result?;
        drop(cell);

        // The commit sealed the match on this album and queued it for
        // automatic management.
        let tracks: Vec<&str> = bundle
            .items
            .iter()
            .map(|item| item.track_id.as_str())
            .collect();
        let album_id = self
            .db
            .read_blocking("import.album", |conn| {
                Ok::<_, PublishError>(crate::identity::import_album(conn, &tracks)?)
            })
            .map_err(|error| fault(format!("the catalog could not be read: {error}")))?
            .ok_or_else(|| fault("imported track is not in the catalog".to_owned()))?;
        // The commit sealed the match; its artist proofs and links follow
        // in their own write, so they can never fail the publish.
        self.identify_store.link_after_command(&album_id);
        let paths = bundle
            .items
            .iter()
            .map(|item| root.path.join(&item.dest_rel))
            .collect();
        let tags_kept = placements
            .iter()
            .filter(|placement| placement.policy == EffectivePolicy::LocalMetadata)
            .count();
        Ok(ImportedAlbum {
            bundle_id: bundle.id.clone(),
            album_id,
            paths,
            skipped,
            blocked,
            tags_kept,
        })
    }

    /// The track id a file bound for `dest_rel` takes, or why it is held.
    /// Without a join, as a scan would: the row at the path (a track whose
    /// file is gone), else the stable id. With one, the row at the path is
    /// taken only when it is on the joined album and not excluded; any
    /// other row there keeps its id and gives up the path in the commit,
    /// unless the catalog still lists its file, which holds this one as an
    /// occupied destination. A row the file replaces as another version
    /// (`replaced`) is never taken: it keeps what describes its own
    /// recording. The file then takes the stable id, or a new one when
    /// that is taken. A track the file replaces (`freed`) leaves the
    /// catalog in the same commit, so it never counts as still listed.
    #[allow(clippy::too_many_arguments)]
    fn planned_track_id(
        &self,
        conn: &rusqlite::Connection,
        root_id: &str,
        dest_rel: &str,
        join: bool,
        joined: Option<&JoinedAlbum>,
        replaced: &[(String, String)],
        freed: &[String],
    ) -> rusqlite::Result<Result<String, String>> {
        use rusqlite::OptionalExtension as _;
        if !join {
            return Ok(super::scan::sqlite_store::adoptable_track_id(
                conn, root_id, dest_rel, true,
            )?
            .ok_or_else(|| "no free track id for an imported file".to_owned()));
        }
        let at_path: Option<(String, String, String)> = conn
            .query_row(
                "SELECT id, local_album_id, availability FROM local_tracks \
                 WHERE root_id = ?1 AND relative_path = ?2",
                rusqlite::params![root_id, dest_rel],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        if let Some((id, album, availability)) = at_path {
            if availability != "excluded"
                && joined.is_some_and(|joined| joined.album_id == album)
                && !replaced.iter().any(|(_, old)| *old == id)
            {
                return Ok(Ok(id));
            }
            if availability == "indexed" && !freed.contains(&id) {
                return Ok(Err(format!(
                    "a track of another album is still listed at {dest_rel}"
                )));
            }
        }
        Ok(Ok(super::scan::sqlite_store::adoptable_track_id(
            conn, root_id, dest_rel, false,
        )?
        .unwrap_or_else(|| self.core.ids.new_id())))
    }

    /// The files of the indexed tracks in `replaces`, each read once before
    /// the publish lock (see [`remove::probe`]), or why one of them cannot
    /// be taken away. A track no longer indexed has no file to take away.
    fn old_copies(
        &self,
        registry: &RootRegistry,
        replaces: &[String],
    ) -> Result<Result<Vec<Target>, String>, ImportError> {
        use rusqlite::OptionalExtension as _;
        if replaces.is_empty() {
            return Ok(Ok(Vec::new()));
        }
        let rows = self
            .db
            .read_blocking("import.replaced_rows", |conn| {
                let mut statement = conn.prepare_cached(
                    "SELECT root_id, relative_path FROM local_tracks \
                     WHERE id = ?1 AND availability = 'indexed'",
                )?;
                let mut rows = Vec::with_capacity(replaces.len());
                for id in replaces {
                    if let Some((root, rel)) = statement
                        .query_row([id], |row| {
                            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                        })
                        .optional()?
                    {
                        rows.push((id.clone(), root, rel));
                    }
                }
                Ok::<_, PublishError>(rows)
            })
            .map_err(|error| ImportError::LocalFault(error.to_string()))?;
        let mut targets = Vec::with_capacity(rows.len());
        for (track_id, root_id, relative_path) in rows {
            let path = registry.resolve(&root_id).and_then(|root| {
                super::mutations::join_inside(&root.path, &relative_path).map(|path| (root, path))
            });
            let Some((root, path)) = path else {
                return Ok(Err(
                    "the copy it replaces is not in a library folder".to_owned()
                ));
            };
            match remove::probe(&root.path, &path) {
                Ok(probe) => targets.push(Target {
                    track_id,
                    root_id,
                    relative_path,
                    probe,
                    expect_sha256: None,
                }),
                Err(error) => {
                    tracing::warn!(%error, track_id, "the copy an imported file replaces could not be read");
                    return Ok(Err(format!(
                        "the copy it replaces could not be read: {error}"
                    )));
                }
            }
        }
        Ok(Ok(targets))
    }
}

/// Whether something other than the files in `leaving` sits at `dest`, or
/// beside it under a name that folds to the same one (case or Unicode), as
/// the publisher's collision gate judges it. A path that cannot be read
/// counts as taken.
fn taken(root_id: &str, dest: &Path, leaving: &[&Path]) -> bool {
    if std::fs::symlink_metadata(dest).is_ok() && !leaving.contains(&dest) {
        return true;
    }
    let (Some(parent), Some(wanted)) = (dest.parent(), dest.file_name().and_then(|n| n.to_str()))
    else {
        return true;
    };
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    let wanted_key = super::publish::collision_key(root_id, wanted);
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let Some(text) = name.to_str() else {
            return false;
        };
        text != wanted
            && super::publish::collision_key(root_id, text) == wanted_key
            && !leaving.contains(&parent.join(text).as_path())
    })
}

/// The tracks among `ids` the catalog lists as indexed.
fn indexed_among(
    conn: &rusqlite::Connection,
    ids: &[String],
) -> rusqlite::Result<std::collections::HashSet<String>> {
    let mut statement = conn
        .prepare_cached("SELECT 1 FROM local_tracks WHERE id = ?1 AND availability = 'indexed'")?;
    let mut indexed = std::collections::HashSet::new();
    for id in ids {
        if statement.exists([id])? {
            indexed.insert(id.clone());
        }
    }
    Ok(indexed)
}

/// Why a file is held when a track it replaces cannot be taken away.
fn blocked_reason(refusal: &Refusal) -> String {
    match refusal {
        Refusal::Busy => "another write holds the copy it replaces".to_owned(),
        Refusal::Changed => "the copy it replaces changed while it was read".to_owned(),
        Refusal::OutsideRoot => "the copy it replaces is not in a library folder".to_owned(),
        Refusal::BinsUnknown(cause) => format!("no recycle bin is known while {cause}"),
        Refusal::NoBin(_) => {
            "the library folder of the copy it replaces has no usable recycle bin".to_owned()
        }
        Refusal::Publish { error, .. } => {
            format!("the copy it replaces could not be taken away: {error}")
        }
    }
}

/// Name of the file in a task's import folder that says where each staged
/// copy came from, so a commit resumed after a crash keeps provenance.
const PROVENANCE_FILE: &str = "provenance.json";

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Provenance {
    task_id: String,
    /// Staged file name to the downloaded file it copies.
    sources: BTreeMap<String, String>,
    /// `download` or `drop`; empty in records written before drop imports.
    #[serde(default)]
    ingest_source: String,
    /// The bundle that publishes the staged copies; empty in records
    /// written before joins, which then join nothing.
    #[serde(default)]
    bundle_id: String,
    /// The album the files were fetched for.
    #[serde(default)]
    join: Option<AlbumJoin>,
    /// Staged file name to the track id the plan pinned and the file's
    /// place on the join's release.
    #[serde(default)]
    files: BTreeMap<String, ProvenanceFile>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ProvenanceFile {
    track_id: String,
    #[serde(default)]
    track: Option<JoinedTrack>,
}

/// Record the task, the bundle and each staged copy's source, track id
/// and place, synced, before publish.
fn write_provenance(
    dir: &Path,
    import: &DownloadImport,
    bundle_id: &str,
    items: &[PlanItem],
) -> std::io::Result<()> {
    use std::io::Write as _;
    let staged = |item: &PlanItem| {
        Path::new(&item.source_rel)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    };
    let provenance = Provenance {
        task_id: import.task_id.clone(),
        ingest_source: import.origin.as_str().to_owned(),
        sources: items
            .iter()
            .filter_map(|item| {
                let source = item.adopt.as_ref()?.source_path.clone()?;
                Some((staged(item)?, source))
            })
            .collect(),
        bundle_id: bundle_id.to_owned(),
        join: items
            .iter()
            .find_map(|item| item.adopt.as_ref()?.join.clone()),
        files: items
            .iter()
            .filter_map(|item| {
                Some((
                    staged(item)?,
                    ProvenanceFile {
                        track_id: item.track_id.clone(),
                        track: item.adopt.as_ref()?.track.clone(),
                    },
                ))
            })
            .collect(),
    };
    let body = serde_json::to_vec(&provenance).map_err(std::io::Error::other)?;
    std::fs::create_dir_all(dir)?;
    let mut file = std::fs::File::create(dir.join(PROVENANCE_FILE))?;
    file.write_all(&body)?;
    file.sync_all()
}

/// What a resumed commit learns from an import's provenance record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Recovered {
    /// The download task, for a download import.
    pub task_id: Option<String>,
    pub source_path: Option<String>,
    /// `download` or `drop`.
    pub ingest_source: String,
    /// The album the file was fetched for, and its place on its release.
    pub join: Option<AlbumJoin>,
    pub track: Option<JoinedTrack>,
}

impl Default for Recovered {
    fn default() -> Self {
        Self {
            task_id: None,
            source_path: None,
            ingest_source: ImportOrigin::Download.as_str().to_owned(),
            join: None,
            track: None,
        }
    }
}

/// The provenance recorded for a staged import copy (download defaults
/// when the record is gone). The join and the file's place count only
/// when the record names the journal's bundle and the file's track id: a
/// record of another landing never steers this commit.
pub(crate) fn recovered_provenance(staged: &Path, bundle_id: &str, track_id: &str) -> Recovered {
    let Some(dir) = staged.parent() else {
        return Recovered::default();
    };
    let Some(provenance) = std::fs::read(dir.join(PROVENANCE_FILE))
        .ok()
        .and_then(|body| serde_json::from_slice::<Provenance>(&body).ok())
    else {
        return Recovered::default();
    };
    let name = staged
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let source_path = provenance.sources.get(&name).cloned();
    let origin = if provenance.ingest_source == ImportOrigin::Drop.as_str() {
        ImportOrigin::Drop
    } else {
        ImportOrigin::Download
    };
    let file = provenance
        .files
        .get(&name)
        .filter(|file| provenance.bundle_id == bundle_id && file.track_id == track_id);
    Recovered {
        task_id: Some(provenance.task_id)
            .filter(|id| !id.is_empty() && origin == ImportOrigin::Download),
        source_path,
        ingest_source: origin.as_str().to_owned(),
        join: file.and(provenance.join.clone()),
        track: file.and_then(|file| file.track.clone()),
    }
}

/// Whether the library holds an indexed album of release group `group`
/// (its sealed identity, else its files' own tags), as the landing reads
/// what is already owned. An empty group is held by nothing.
fn group_held(conn: &rusqlite::Connection, group: &str) -> rusqlite::Result<bool> {
    if group.is_empty() {
        return Ok(false);
    }
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM local_tracks t \
         JOIN local_albums b ON b.id = t.local_album_id AND b.retired_into_album_id IS NULL \
         LEFT JOIN local_album_external_identities ai ON ai.local_album_id = b.id \
         WHERE t.availability = 'indexed' \
         AND (lower(ai.release_group_mbid) = ?1 \
         OR (ai.local_album_id IS NULL AND lower(t.embedded_release_group_mbid) = ?1)))",
        rusqlite::params![group.to_ascii_lowercase()],
        |row| row.get(0),
    )
}

/// The one folder of `root_id` that holds the album's indexed files on
/// `disc` (all of its indexed files when none is on that disc). `None`
/// when they sit in several folders, in another library folder, or at
/// the top of the root.
fn copy_folder(
    conn: &rusqlite::Connection,
    album_id: &str,
    root_id: &str,
    disc: u32,
) -> rusqlite::Result<Option<String>> {
    let rows: Vec<(String, String, i64)> = conn
        .prepare(
            "SELECT root_id, relative_path, disc_number FROM local_tracks \
             WHERE local_album_id = ?1 AND availability = 'indexed'",
        )?
        .query_map(rusqlite::params![album_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let disc = i64::from(disc.max(1));
    let on_disc: Vec<&(String, String, i64)> =
        rows.iter().filter(|row| row.2.max(1) == disc).collect();
    let files = if on_disc.is_empty() {
        rows.iter().collect()
    } else {
        on_disc
    };
    let mut folders = files.iter().map(|(root, rel, _)| {
        let folder = Path::new(rel)
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned())
            .unwrap_or_default();
        (root.as_str(), folder)
    });
    let Some(first) = folders.next() else {
        return Ok(None);
    };
    let one = folders.all(|folder| folder == first);
    Ok((one && first.0 == root_id && !first.1.is_empty()).then_some(first.1))
}
