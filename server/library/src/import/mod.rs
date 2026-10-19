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
//! 2. One bundle moves every copy to the path the import naming script gives
//!    and writes the release's full tag set on the way.
//! 3. The publisher's catalog commit adds the tracks to the catalog in the
//!    same transaction (from the published files' own tags, exactly as a
//!    scan would index them), so no rescan is needed and nothing shows in
//!    the library before its file is in place.
//! 4. The album and its tracks get an automatic MusicBrainz identity from
//!    the match, unless a person decided them, and the release document is
//!    kept for later retags.
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

use super::clock::today_day;
use super::identify::sqlite::{ImportSeal, ImportTrack};
use super::matching::Release;
use super::publish::planner::{
    AlbumJoin, Capability, FileFingerprint, JoinedTrack, PlanBundle, PlanItem, PlanKind,
    ReleaseIdentity, SealRecheck, SealedPreview,
};
use super::publish::publisher::{JoinedAlbum, joined_album};
use super::publish::snapshots::sha256_hex;
use super::publish::{HIDDEN_PREFIX, PublishError};
use super::scan::models::EffectivePolicy;
use super::wiring::LibrarySetup;

/// Revisions the seal pins while profiles and naming have no store (the
/// same constants manual management uses).
const PINNED_REVISION: u64 = 1;

/// One file to import and the release track it is.
#[derive(Debug, Clone)]
pub struct ImportSource {
    pub path: PathBuf,
    /// Index into the release's tracks.
    pub track: usize,
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
    format: String,
    tags: BTreeMap<String, Vec<String>>,
    /// The track id the plan pins, set under the publish lock.
    track_id: String,
}

impl LibrarySetup {
    /// Import a verified download as one staged bundle, filed by its
    /// tags. Blocking: callers run it off the async runtime.
    pub fn import_download(&self, import: &DownloadImport) -> Result<ImportedAlbum, ImportError> {
        self.import_download_joined(import, None)
    }

    /// Import a verified download as one staged bundle. With `join`, the
    /// files join the library album they were fetched for (see the module
    /// doc). Blocking: callers run it off the async runtime.
    pub fn import_download_joined(
        &self,
        import: &DownloadImport,
        join: Option<ImportJoin>,
    ) -> Result<ImportedAlbum, ImportError> {
        let fault = |message: String| ImportError::LocalFault(message);
        if import.files.is_empty() && import.extras.is_empty() {
            return Err(fault("nothing to import".to_owned()));
        }
        let registry = self.live_registry();
        let root = registry
            .roots()
            .iter()
            .find(|root| root.policy != EffectivePolicy::Excluded)
            .cloned()
            .ok_or_else(|| fault("no library root takes imports".to_owned()))?;
        // Settings that do not read never mean the default layout.
        let script = self
            .settings
            .library
            .read()
            .ok_or_else(|| fault("the library settings cannot be read".to_owned()))?
            .naming_script
            .clone();
        let root_path = root.path.to_string_lossy().into_owned();
        let task_dir = staging_rel(import.staging.as_deref().unwrap_or(&import.task_id));
        let mut sources = Vec::with_capacity(import.files.len() + import.extras.len());
        for file in &import.files {
            let track = import.release.tracks.get(file.track).ok_or_else(|| {
                fault("a file names a track the release does not have".to_owned())
            })?;
            sources.push((&file.path, track.clone()));
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
            ));
        }
        let mut placements = Vec::with_capacity(sources.len());
        for (ordinal, (path, track)) in sources.into_iter().enumerate() {
            let track = &track;
            let format = super::tags::format_for_path(path)
                .map_err(|error| fault(format!("unsupported file: {error}")))?
                .as_str()
                .to_owned();
            let named = super::matching::ReleaseTrack {
                disc: track.disc.max(1),
                ..track.clone()
            };
            let dest_rel = naming::render(
                &script,
                &naming::variables(&import.release, &named, &format),
                &root_path,
            )
            .map_err(|error| ImportError::NamingScript(error.to_string()))?;
            let mut tags = super::tags::picard::release_tags(&import.release, track);
            tags.retain(|name, values| {
                super::tags::TagField::from_name(name).is_some_and(|field| {
                    super::tags::save::accepts(&super::tags::TagEdit::new(field, values.clone()))
                })
            });
            if !super::tags::save::writable(
                super::tags::format_for_path(path)
                    .map_err(|error| fault(format!("unsupported file: {error}")))?,
            ) {
                tags.clear();
            }
            placements.push(Placement {
                index: ordinal,
                source: path.clone(),
                track: track.clone(),
                staged_rel: format!("{task_dir}/{ordinal:03}.{format}"),
                dest_rel,
                format,
                tags,
                track_id: String::new(),
            });
        }

        let mut cell = self
            .publish
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _guards = self.publish_guards(&registry);
        cell.refresh(&registry, &self.root_dirs)
            .map_err(|error| fault(error.to_string()))?;
        let open = cell.open().map_err(|error| fault(error.to_string()))?;
        let staged_dir = open
            .sandbox
            .resolve_no_symlink(&root.id, &task_dir)
            .map_err(|error| fault(error.to_string()))?;
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
        // Files bound for a taken destination are held on their own (the
        // first of two files bound for one path keeps it); the rest import.
        let mut skipped = Vec::new();
        let mut claimed = std::collections::HashSet::new();
        let mut kept = Vec::with_capacity(placements.len());
        for mut placement in placements {
            let key = super::publish::collision_key(&root.id, &placement.dest_rel);
            let occupied = open
                .sandbox
                .resolve_no_symlink(&root.id, &placement.dest_rel)
                .map(|dest| std::fs::symlink_metadata(dest).is_ok())
                .unwrap_or(true);
            let reason = if !claimed.insert(key) {
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
            match reason {
                Some(reason) => skipped.push((placement.index, reason)),
                None => kept.push(placement),
            }
        }
        let placements = kept;
        if placements.is_empty() {
            return Ok(ImportedAlbum {
                bundle_id: String::new(),
                album_id: String::new(),
                paths: Vec::new(),
                skipped,
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
        let dest_policy = |dest_rel: &str| super::publish::planner::Adoption {
            policy: root.policy_for(&root.path.join(dest_rel)),
            policy_revision: registry.policy_revision().to_owned(),
            download_task_id: (import.origin == ImportOrigin::Download)
                .then(|| import.task_id.clone()),
            source_path: None,
            ingest_source: import.origin.as_str().to_owned(),
            join: join.clone(),
            track: None,
        };
        let bundle_id = self.ids.new_id();
        let result = (|| {
            let mut items = Vec::with_capacity(placements.len());
            for placement in &placements {
                let staged = open
                    .sandbox
                    .resolve_no_symlink(&root.id, &placement.staged_rel)
                    .map_err(|error| fault(error.to_string()))?;
                let fingerprint = copy_in(&placement.source, &staged)
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
                    format: placement.format.clone(),
                    managed_updates: placement.tags.clone(),
                    sidecars: Vec::new(),
                    adopt: Some(super::publish::planner::Adoption {
                        source_path: Some(placement.source.to_string_lossy().into_owned()),
                        track: joined_track(track),
                        ..dest_policy(&placement.dest_rel)
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
                today_day: today_day(),
                token_hash: token_hash.clone(),
            };
            let sealed = SealedPreview::seal(bundle, token_hash, PINNED_REVISION, today_day() + 1);
            open.publisher
                .publish(&sealed, &live, &BTreeMap::new())
                .map_err(|error| match error {
                    PublishError::Collision(message) => ImportError::Occupied(message),
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

        let first = &bundle.items[0].track_id;
        let album_id = self
            .scan_store
            .album_for_track(first)
            .map_err(|error| fault(format!("the catalog could not be read: {error}")))?
            .ok_or_else(|| fault("imported track is not in the catalog".to_owned()))?;
        let joined = joined.is_some_and(|joined| joined.album_id == album_id);
        self.seal_import_identity(&album_id, import, &bundle, joined);
        let paths = bundle
            .items
            .iter()
            .map(|item| root.path.join(&item.dest_rel))
            .collect();
        // Automatic management runs the folder's profile on it once the
        // catalog settles, when the folder asks for that.
        self.queue_automatic_management(&album_id);
        Ok(ImportedAlbum {
            bundle_id: bundle.id.clone(),
            album_id,
            paths,
            skipped,
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
    /// that is taken.
    fn planned_track_id(
        &self,
        conn: &rusqlite::Connection,
        root_id: &str,
        dest_rel: &str,
        join: bool,
        joined: Option<&JoinedAlbum>,
        replaced: &[(String, String)],
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
            if availability == "indexed" {
                return Ok(Err(format!(
                    "a track of another album is still listed at {dest_rel}"
                )));
            }
        }
        Ok(Ok(super::scan::sqlite_store::adoptable_track_id(
            conn, root_id, dest_rel, false,
        )?
        .unwrap_or_else(|| self.ids.new_id())))
    }

    /// The match is the identity: seal the album and its tracks as an
    /// automatic MusicBrainz identification, unless a person already
    /// decided them, and keep the release document for retags, in one
    /// transaction. Files that joined the album they were fetched for
    /// (`joined`) seal only their tracks: a few fetched tracks never
    /// confirm the album's match. When the seal does not land, the album
    /// is offered to identification instead.
    fn seal_import_identity(
        &self,
        album_id: &str,
        import: &DownloadImport,
        bundle: &PlanBundle,
        joined: bool,
    ) {
        let store = &self.identify_store;
        let seal = ImportSeal {
            local_album_id: album_id,
            release: &import.release,
            joined,
            tracks: bundle
                .items
                .iter()
                // Extra files carry no recording: nothing to seal.
                .filter(|item| !item.identity.recording_mbid.is_empty())
                .map(|item| ImportTrack {
                    local_track_id: item.track_id.clone(),
                    recording_mbid: item.identity.recording_mbid.clone(),
                    release_track_mbid: item.identity.release_track_mbid.clone(),
                })
                .collect(),
        };
        let Err(error) = store.seal_import(&seal) else {
            return;
        };
        tracing::error!(
            %error,
            album = album_id,
            "the import's match was not sealed; the album is offered to identification"
        );
        if let Err(error) = store.offer(album_id) {
            tracing::error!(
                %error,
                album = album_id,
                "the imported album could not be offered to identification"
            );
        }
    }
}

/// The hidden folder, relative to its library root, that an import
/// staged under `name` copies its files into. Publish journal rows name
/// it as their source, which is how a download task's writes are found.
pub(crate) fn staging_rel(name: &str) -> String {
    format!("{HIDDEN_PREFIX}import/{}", safe_name(name))
}

/// A task id as one folder name.
fn safe_name(task_id: &str) -> String {
    let cleaned: String = task_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "_".to_owned()
    } else {
        cleaned
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

/// Whether every publish that took files from the staging folder
/// `task_dir` is over, the question `Management::download_publish_settled`
/// asks for a download task: cleaned up, undone, committed with only its
/// own cleanup left, or closed by an administrator after needing
/// attention.
fn staging_settled(conn: &rusqlite::Connection, task_dir: &str) -> rusqlite::Result<bool> {
    let folder = format!("{task_dir}/");
    let unsettled: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM library_publish_journal \
         WHERE bundle_id IN (SELECT bundle_id FROM library_publish_journal \
         WHERE substr(source_rel, 1, length(?1)) = ?1) \
         AND state NOT IN ('cleaned', 'compensated', 'cleanup_pending') \
         AND NOT (state = 'needs_attention' AND bundle_id IN \
         (SELECT bundle_id FROM library_publish_resolutions)))",
        rusqlite::params![folder],
        |row| row.get(0),
    )?;
    Ok(!unsettled)
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

/// Copy one landed file into the hidden import folder, synced, hashing
/// the bytes as they stream through.
fn copy_in(source: &Path, staged: &Path) -> std::io::Result<FileFingerprint> {
    use sha2::Digest as _;
    use std::io::{Read as _, Write as _};
    if let Some(parent) = staged.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut reader = std::fs::File::open(source)?;
    let mut writer = std::fs::File::create(staged)?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut size = 0u64;
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        writer.write_all(&buffer[..read])?;
        size += read as u64;
    }
    writer.sync_all()?;
    let sha256 = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    Ok(FileFingerprint { size, sha256 })
}

/// Remove import folders a stopped process left behind, once no publish
/// is waiting on recovery (an unsettled bundle may still name one of
/// their copies). Blocking.
pub(crate) fn sweep_import_leftovers(roots: &[PathBuf], journal_settled: bool) {
    if !journal_settled {
        return;
    }
    for root in roots {
        let dir = root.join(format!("{HIDDEN_PREFIX}import"));
        match std::fs::symlink_metadata(&dir) {
            Ok(meta) if meta.is_dir() => {
                if let Err(error) = std::fs::remove_dir_all(&dir) {
                    tracing::warn!(dir = %dir.display(), %error, "import leftovers not removed");
                } else {
                    tracing::info!(dir = %dir.display(), "removed import leftovers");
                }
            }
            Ok(_) | Err(_) => {}
        }
    }
}

/// Remove the hidden import folder (and its parent when empty). The
/// publisher already removed the copies it published; anything left is a
/// copy of a failed import.
fn remove_staging(dir: &Path) {
    if let Err(error) = std::fs::remove_dir_all(dir)
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(dir = %dir.display(), %error, "import staging folder not removed");
    }
    if let Some(parent) = dir.parent()
        && std::fs::remove_dir(parent).is_err()
    {
        tracing::debug!("import staging parent kept: other imports use it");
    }
}
