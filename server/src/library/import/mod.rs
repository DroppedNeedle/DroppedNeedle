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
//! 2. One bundle moves every copy to the path the naming template gives
//!    and writes the release's full tag set on the way.
//! 3. The publisher's catalog commit adds the tracks to the catalog in the
//!    same transaction (from the published files' own tags, exactly as a
//!    scan would index them), so no rescan is needed and nothing shows in
//!    the library before its file is in place.
//! 4. The album and its tracks get an automatic MusicBrainz identity from
//!    the match, and the release document is kept for later retags.
//!
//! An import is not an edit of library files, so it records no undo
//! snapshot and no first-management baseline: the files' first library
//! state is the one the import wrote.

pub mod naming;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use self::naming::NamingFields;
use super::clock::today_day;
use super::identify::models::{AlbumIdentity, DecisionSource, TrackIdentity};
use super::identify::stores::{IdentityStore as _, ReleaseStore as _};
use super::matching::Release;
use super::matching::model::credit_text;
use super::publish::planner::{
    Capability, FileFingerprint, PlanBundle, PlanItem, PlanKind, ReleaseIdentity, SealRecheck,
    SealedPreview,
};
use super::publish::publisher::{Catalog as _, SqliteCatalog};
use super::publish::snapshots::sha256_hex;
use super::publish::{HIDDEN_PREFIX, PublishError};
use super::scan::models::EffectivePolicy;
use super::scan::store::CatalogStore as _;
use super::wiring::LibrarySetup;
use crate::runtime_config::secret_sections::TypedLibrary;
use crate::runtime_config::sections::DEFAULT_NAMING_TEMPLATE;

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

/// A verified download to put in the library.
#[derive(Debug, Clone)]
pub struct DownloadImport {
    pub task_id: String,
    pub release: Release,
    pub files: Vec<ImportSource>,
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
}

/// One planned file.
struct Placement {
    /// Index in the request.
    index: usize,
    source: PathBuf,
    track: usize,
    staged_rel: String,
    dest_rel: String,
    format: String,
    tags: BTreeMap<String, Vec<String>>,
}

impl LibrarySetup {
    /// Import a verified download as one staged bundle. Blocking: callers
    /// run it off the async runtime.
    pub fn import_download(&self, import: &DownloadImport) -> Result<ImportedAlbum, ImportError> {
        let fault = |message: String| ImportError::LocalFault(message);
        if import.files.is_empty() {
            return Err(fault("nothing to import".to_owned()));
        }
        let registry = self.live_registry();
        let root = registry
            .roots()
            .iter()
            .find(|root| root.policy != EffectivePolicy::Excluded)
            .cloned()
            .ok_or_else(|| fault("no library root takes imports".to_owned()))?;
        let template = self
            .config
            .get_raw::<TypedLibrary>()
            .map(|settings| settings.naming_template)
            .ok()
            .filter(|template| !template.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_NAMING_TEMPLATE.to_owned());
        let task_dir = format!("{HIDDEN_PREFIX}import/{}", safe_name(&import.task_id));
        let mut placements = Vec::with_capacity(import.files.len());
        for (ordinal, file) in import.files.iter().enumerate() {
            let track = import.release.tracks.get(file.track).ok_or_else(|| {
                fault("a file names a track the release does not have".to_owned())
            })?;
            let format = super::tags::format_for_path(&file.path)
                .map_err(|error| fault(format!("unsupported file: {error}")))?
                .as_str()
                .to_owned();
            let fields = NamingFields {
                artist: credit_text(if track.artists.is_empty() {
                    &import.release.artists
                } else {
                    &track.artists
                }),
                album: import.release.title.clone(),
                album_artist: import.release.artist_text(),
                title: track.title.clone(),
                year: import.release.original_year().or(import.release.year()),
                track: track.position,
                disc: track.disc.max(1),
                genre: String::new(),
                release_group_mbid: import.release.release_group_id.clone(),
                artist_mbid: import
                    .release
                    .artists
                    .first()
                    .map(|artist| artist.id.clone())
                    .unwrap_or_default(),
                ext: format.clone(),
            };
            let mut tags = super::tags::picard::release_tags(&import.release, track);
            tags.retain(|name, values| {
                super::tags::TagField::from_name(name).is_some_and(|field| {
                    super::tags::save::accepts(&super::tags::TagEdit::new(field, values.clone()))
                })
            });
            if !super::tags::save::writable(
                super::tags::format_for_path(&file.path)
                    .map_err(|error| fault(format!("unsupported file: {error}")))?,
            ) {
                tags.clear();
            }
            placements.push(Placement {
                index: ordinal,
                source: file.path.clone(),
                track: file.track,
                staged_rel: format!("{task_dir}/{ordinal:03}.{format}"),
                dest_rel: naming::render(&template, &fields),
                format,
                tags,
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
        // Files bound for a taken destination are held on their own (the
        // first of two files bound for one path keeps it); the rest import.
        let mut skipped = Vec::new();
        let mut claimed = std::collections::HashSet::new();
        placements.retain(|placement| {
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
                None
            };
            match reason {
                Some(reason) => {
                    skipped.push((placement.index, reason));
                    false
                }
                None => true,
            }
        });
        if placements.is_empty() {
            return Ok(ImportedAlbum {
                bundle_id: String::new(),
                album_id: String::new(),
                paths: Vec::new(),
                skipped,
            });
        }
        let dest_policy = |dest_rel: &str| super::publish::planner::Adoption {
            policy: root.policy_for(&root.path.join(dest_rel)),
            policy_revision: registry.policy_revision().to_owned(),
            download_task_id: Some(import.task_id.clone()),
            source_path: None,
        };
        let result = (|| {
            let mut items = Vec::with_capacity(placements.len());
            for placement in &placements {
                let staged = open
                    .sandbox
                    .resolve_no_symlink(&root.id, &placement.staged_rel)
                    .map_err(|error| fault(error.to_string()))?;
                let fingerprint = copy_in(&placement.source, &staged)
                    .map_err(|error| fault(format!("copy into the library failed: {error}")))?;
                let track = &import.release.tracks[placement.track];
                let track_id = super::scan::sqlite_store::adoptable_track_id(
                    open.publisher.connection(),
                    &root.id,
                    &placement.dest_rel,
                )
                .map_err(|error| fault(error.to_string()))?
                .ok_or_else(|| fault("no free track id for an imported file".to_owned()))?;
                items.push(PlanItem {
                    track_id,
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
                        ..dest_policy(&placement.dest_rel)
                    }),
                });
            }
            write_provenance(&staged_dir, &import.task_id, &placements)
                .map_err(|error| fault(format!("import provenance not written: {error}")))?;
            let catalog_revision = SqliteCatalog
                .revision(open.publisher.connection())
                .map_err(|error| fault(error.to_string()))?;
            let bundle = PlanBundle {
                id: self.ids.new_id(),
                items,
                profile_revision: PINNED_REVISION,
                naming_revision: PINNED_REVISION,
                policy_revision: super::manage::policy_revision_u64(&registry),
                catalog_revision,
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
                catalog_revision,
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
        remove_staging(&staged_dir);
        let bundle = result?;
        drop(cell);

        let first = &bundle.items[0].track_id;
        let album_id = self
            .scan_store
            .album_for_track(first)
            .ok_or_else(|| fault("imported track is not in the catalog".to_owned()))?;
        self.seal_import_identity(&album_id, import, &bundle);
        let paths = bundle
            .items
            .iter()
            .map(|item| root.path.join(&item.dest_rel))
            .collect();
        Ok(ImportedAlbum {
            bundle_id: bundle.id.clone(),
            album_id,
            paths,
            skipped,
        })
    }

    /// The match is the identity: seal the album and its tracks as an
    /// automatic MusicBrainz identification, unless a person already
    /// decided them, and keep the release document for retags.
    fn seal_import_identity(&self, album_id: &str, import: &DownloadImport, bundle: &PlanBundle) {
        let store = &self.identify_store;
        store.save_release(&import.release);
        let current = store.album_identity(album_id);
        if current
            .as_ref()
            .is_some_and(|row| !row.decision_source.automatic_may_overwrite())
        {
            return;
        }
        // An album that already has an exact edition keeps it: a download
        // matched to another edition adds its files without flipping the
        // album. Files verified against the album's own edition confirm an
        // unsure match.
        let held = current
            .as_ref()
            .and_then(|row| row.release_mbid.clone())
            .filter(|held| !import.release.answers_to(held));
        if held.is_none() {
            store.save_album_identity(AlbumIdentity {
                local_album_id: album_id.to_owned(),
                provider: "musicbrainz".to_owned(),
                release_group_mbid: Some(import.release.release_group_id.clone()),
                release_mbid: Some(import.release.id.clone()),
                decision_source: DecisionSource::Automatic,
                row_revision: current.as_ref().map_or(1, |row| row.row_revision + 1),
            });
            store.set_match_flag(album_id, None);
        } else {
            tracing::info!(
                album = album_id,
                release = import.release.id,
                "download matched another edition; the album keeps its own"
            );
        }
        for item in &bundle.items {
            let current = store.track_identity(&item.track_id);
            if current
                .as_ref()
                .is_some_and(|row| !row.decision_source.automatic_may_overwrite())
            {
                continue;
            }
            store.save_track_identity(TrackIdentity {
                local_track_id: item.track_id.clone(),
                provider: "musicbrainz".to_owned(),
                recording_mbid: Some(item.identity.recording_mbid.clone()),
                // A track of another edition keeps only its recording.
                release_track_mbid: held
                    .is_none()
                    .then(|| item.identity.release_track_mbid.clone()),
                decision_source: DecisionSource::Automatic,
                row_revision: current.map_or(1, |row| row.row_revision + 1),
            });
        }
    }
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
}

/// Record the task and each staged copy's source, synced, before publish.
fn write_provenance(dir: &Path, task_id: &str, placements: &[Placement]) -> std::io::Result<()> {
    use std::io::Write as _;
    let provenance = Provenance {
        task_id: task_id.to_owned(),
        sources: placements
            .iter()
            .filter_map(|placement| {
                let name = Path::new(&placement.staged_rel).file_name()?;
                Some((
                    name.to_string_lossy().into_owned(),
                    placement.source.to_string_lossy().into_owned(),
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

/// The download task and source path recorded for a staged import copy
/// (`None`s when the record is gone).
pub(crate) fn recovered_provenance(staged: &Path) -> (Option<String>, Option<String>) {
    let Some(dir) = staged.parent() else {
        return (None, None);
    };
    let Some(provenance) = std::fs::read(dir.join(PROVENANCE_FILE))
        .ok()
        .and_then(|body| serde_json::from_slice::<Provenance>(&body).ok())
    else {
        return (None, None);
    };
    let source = staged
        .file_name()
        .and_then(|name| provenance.sources.get(name.to_string_lossy().as_ref()))
        .cloned();
    (Some(provenance.task_id).filter(|id| !id.is_empty()), source)
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
