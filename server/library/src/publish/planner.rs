//! Management planner: bounded plans plus the sealed preview.
//!
//! Planning pins everything Apply needs: per-file source stat and
//! content hashes, the desired metadata, capability results, exact
//! destination collision keys, the settings and policy revisions, and
//! each moved track's catalog rows (see [`seal_rows`]). Planning never
//! mutates library files; it only writes management metadata,
//! content-addressed blobs, plan rows, and operation state.
//!
//! Manual Apply consumes the exact sealed preview and rejects stale
//! file, identity, profile, or policy state. Automatic work additionally
//! requires an enabled root, an enabled trigger, an accepted exact
//! MusicBrainz release, and a full track mapping.

use std::collections::{BTreeMap, HashSet};

use super::PublishError;
use super::paths::{CollisionKey, Sandbox, collision_key};

/// Content fingerprint pinned on a plan item.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileFingerprint {
    /// Byte length at plan time.
    pub size: u64,
    /// SHA-256 of the file bytes at plan time.
    pub sha256: String,
}

/// Accepted exact-edition identity for one file.
///
/// The release-track MBID is distinct from the recording MBID and is
/// never inferred from it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ReleaseIdentity {
    /// Accepted MusicBrainz release MBID.
    pub release_mbid: String,
    /// Release-group MBID the release belongs to.
    pub release_group_mbid: String,
    /// Recording MBID for this track.
    pub recording_mbid: String,
    /// Release-track MBID for this track on the accepted release.
    pub release_track_mbid: String,
    /// Accepted album identity revision, pinned separately from the
    /// per-track mapping revision.
    pub album_identity_revision: u64,
    /// Per-track mapping revision.
    pub mapping_revision: u64,
}

/// Per-track mapping evidence: the accepted release-track MBID plus
/// the medium/track positions automatic work requires.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TrackMapping {
    /// Stable local track id.
    pub track_id: String,
    /// Accepted release-track MBID.
    pub release_track_mbid: String,
    /// Medium position on the accepted release.
    pub medium_position: u32,
    /// Track position on the accepted release.
    pub release_position: u32,
}

/// One independently toggleable management capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Capability {
    /// Write managed metadata tags.
    Metadata,
    /// Write normalized genres.
    Genre,
    /// Embed or replace artwork.
    Artwork,
    /// Rename within the same directory.
    Rename,
    /// Move within the same root.
    SameRootMove,
    /// Manual cross-root move to an explicit destination root.
    CrossRootMove,
    /// Move recognized sidecars with their album.
    Sidecars,
    /// Full scrub of unmanaged tags (advanced, off by default).
    Scrub,
}

impl Capability {
    /// Stable lowercase name for gate messages.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Metadata => "metadata",
            Self::Genre => "genre",
            Self::Artwork => "artwork",
            Self::Rename => "rename",
            Self::SameRootMove => "same_root_move",
            Self::CrossRootMove => "cross_root_move",
            Self::Sidecars => "sidecars",
            Self::Scrub => "scrub",
        }
    }
}

/// What the plan wants for one file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PlanKind {
    /// Retag in place; the original is retained under a hidden backup
    /// across the publish/catalog window.
    SamePath,
    /// Publish to a new destination while retaining the source until
    /// after catalog commit.
    Move,
}

/// One planned file inside an album bundle.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlanItem {
    /// Stable local track id.
    pub track_id: String,
    /// Source root and relative path.
    pub source_root: String,
    pub source_rel: String,
    /// Destination root and relative path.
    pub dest_root: String,
    pub dest_rel: String,
    /// Same-path write or move.
    pub kind: PlanKind,
    /// Source bytes pinned at plan time.
    pub fingerprint: FileFingerprint,
    /// Accepted identity pinned at plan time.
    pub identity: ReleaseIdentity,
    /// Override revision pinned at plan time.
    pub override_revision: u64,
    /// Capabilities this item exercises.
    pub capabilities: Vec<Capability>,
    /// Audio container, lowercased (flac, mp3, ogg, opus, m4a, aac,
    /// wav). WMA is cut everywhere: plans naming it block in the
    /// capability gate before any mutation.
    pub format: String,
    /// Managed-field updates for the staged writer.
    pub managed_updates: BTreeMap<String, Vec<String>>,
    /// Sidecars rebased to the destination directory.
    pub sidecars: Vec<SidecarPlan>,
    /// Estimated staged bytes for disk preflight.
    pub staged_bytes_estimate: u64,
    /// The file comes from outside the library (a finished download):
    /// the catalog commit adds its track instead of moving an existing
    /// one, and nothing is snapshotted for undo or baseline.
    #[serde(default)]
    pub adopt: Option<Adoption>,
    /// The file standing at the destination that this one replaces (a
    /// duplicate resolution that keeps the new copy). It is set aside
    /// under the journal like a same-path original, and moves to the
    /// recycle bin only after the bundle commits.
    #[serde(default)]
    pub replaces: Option<Replacement>,
    /// Files this item writes that come from no source file: a cover
    /// image or a lyrics file. Their bytes wait in the blob store; each
    /// one only ever lands on an empty path.
    #[serde(default)]
    pub outputs: Vec<GeneratedOutput>,
    /// Files an earlier run generated that undo takes away: each is set
    /// aside under the journal and deleted only after the commit, and only
    /// while it still holds the bytes that run wrote.
    #[serde(default)]
    pub removals: Vec<GeneratedOutput>,
    /// The catalog rows the item was sealed against. Every item but an
    /// adoption has one by the time it publishes; one without is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<SealedRow>,
}

/// A file generated from no source (see [`PlanItem::outputs`]).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GeneratedOutput {
    pub dest_root: String,
    pub dest_rel: String,
    /// The bytes, and their key in the blob store.
    pub sha256: String,
    pub size: u64,
    /// `artwork` or `lyrics`, for the pages.
    pub kind: String,
}

/// A file a plan item replaces at its destination.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Replacement {
    /// The bytes the replaced file must still hold.
    pub sha256: String,
    /// Its library track, retired (marked missing) in the commit.
    pub track_id: Option<String>,
    /// Absolute path in the recycle bin it moves to after commit.
    pub recycle_to: String,
}

/// How a file brought into the library is indexed: the root policy that
/// covers its destination (as a scan would apply it), and where it came
/// from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Adoption {
    pub policy: crate::scan::models::EffectivePolicy,
    pub policy_revision: String,
    /// The download task that brought the file.
    pub download_task_id: Option<String>,
    /// Where the file was downloaded to.
    pub source_path: Option<String>,
    /// How the file came in: `download` (a finished download) or `drop`
    /// (files a person uploaded). Bundles journaled before drop imports
    /// existed read as downloads.
    #[serde(default = "Adoption::download_source")]
    pub ingest_source: String,
    /// The library album the file was fetched for. With one, the file
    /// joins that album instead of being filed by its tags, and a row at
    /// its destination that the plan did not adopt gives up its path.
    #[serde(default)]
    pub join: Option<AlbumJoin>,
    /// The release track the file is on the join's release, when the
    /// import's release is that release: what places it on the album's
    /// edition.
    #[serde(default)]
    pub track: Option<JoinedTrack>,
}

/// The album files fetched for a library copy join. The commit follows
/// it to the album it was merged into, and the files join it while it
/// names `release_mbid` (placed on its edition) or another release of
/// `release_group_mbid` (not placed). Otherwise they are filed by their
/// tags.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AlbumJoin {
    pub album_id: String,
    /// The album's release as its identity row named it when the fetch
    /// was planned.
    pub release_mbid: String,
    /// The release group of the release the files were matched to.
    pub release_group_mbid: String,
}

/// Where an imported file sits on its release.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct JoinedTrack {
    pub recording_mbid: String,
    pub release_track_mbid: String,
    pub disc: u32,
    pub position: u32,
    /// The track of the album the file replaces as another version of its
    /// release track: what people made on it moves to the new file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replaced: Option<String>,
}

impl Adoption {
    /// What a resumed commit knows: the provenance the import recorded
    /// next to its staged copies, and the automatic policy (the next scan
    /// re-reads the policy).
    pub fn recovered(
        download_task_id: Option<String>,
        source_path: Option<String>,
        ingest_source: String,
        join: Option<AlbumJoin>,
        track: Option<JoinedTrack>,
    ) -> Self {
        Self {
            policy: crate::scan::models::EffectivePolicy::Automatic,
            policy_revision: String::new(),
            download_task_id,
            source_path,
            ingest_source,
            join,
            track,
        }
    }

    /// The ingest source of a finished download.
    pub fn download_source() -> String {
        "download".to_owned()
    }
}

/// One sidecar travelling with its album.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SidecarPlan {
    /// Source root and relative path.
    pub source_root: String,
    pub source_rel: String,
    /// Destination root and relative path.
    pub dest_root: String,
    pub dest_rel: String,
    /// Source bytes pinned at plan time.
    pub fingerprint: FileFingerprint,
}

/// One complete album bundle: the atomic planning and publish unit.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlanBundle {
    /// Stable bundle id.
    pub id: String,
    /// Files in the bundle.
    pub items: Vec<PlanItem>,
    /// Effective profile revision pinned at plan time.
    pub profile_revision: u64,
    /// Naming script revision pinned at plan time.
    pub naming_revision: u64,
    /// Global typed-library policy revision.
    pub policy_revision: u64,
}

/// The catalog rows a moved or retagged item was sealed against, beside
/// what [`PlanItem`] already pins (its source place and identity
/// revisions). The identity values are the album identity row's and the
/// track mapping row's as the seal read them; the mapping values are
/// `None` when the track has no mapping row.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SealedRow {
    pub album_id: String,
    pub stat_revision: String,
    pub album_release_group_mbid: String,
    pub album_release_mbid: Option<String>,
    pub recording_mbid: Option<String>,
    pub release_track_mbid: Option<String>,
}

/// Everything a commit checks one sealed track's rows against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitSeal {
    pub source_root: String,
    pub source_rel: String,
    pub album_identity_revision: u64,
    pub mapping_revision: u64,
    pub row: SealedRow,
}

impl PlanItem {
    /// What the commit checks this item against; `None` for an adoption,
    /// or an item that was never sealed (which the check refuses).
    pub fn commit_seal(&self) -> Option<CommitSeal> {
        let row = self.sealed.clone()?;
        Some(CommitSeal {
            source_root: self.source_root.clone(),
            source_rel: self.source_rel.clone(),
            album_identity_revision: self.identity.album_identity_revision,
            mapping_revision: self.identity.mapping_revision,
            row,
        })
    }
}

/// One track's catalog rows as they are now.
struct OwnRow {
    root_id: String,
    relative_path: String,
    availability: String,
    stat_revision: String,
    album_id: String,
    album_revision: Option<i64>,
    album_release_group_mbid: Option<String>,
    album_release_mbid: Option<String>,
    mapping_revision: Option<i64>,
    recording_mbid: Option<String>,
    release_track_mbid: Option<String>,
}

/// The current rows of `track_ids`, by id, in one read. A track with no
/// row is absent.
fn own_rows<'a>(
    conn: &rusqlite::Connection,
    track_ids: impl Iterator<Item = &'a str>,
) -> Result<BTreeMap<String, OwnRow>, PublishError> {
    let ids = serde_json::to_string(&track_ids.collect::<Vec<_>>())?;
    let mut stmt = conn.prepare_cached(super::sql::OWN_ROWS.text())?;
    let rows = stmt.query_map([ids], |row| {
        Ok((
            row.get::<_, String>(0)?,
            OwnRow {
                root_id: row.get(1)?,
                relative_path: row.get(2)?,
                availability: row.get(3)?,
                stat_revision: row.get(4)?,
                album_id: row.get(5)?,
                album_revision: row.get(6)?,
                album_release_group_mbid: row.get(7)?,
                album_release_mbid: row.get(8)?,
                mapping_revision: row.get(9)?,
                recording_mbid: row.get(10)?,
                release_track_mbid: row.get(11)?,
            },
        ))
    })?;
    let mut out = BTreeMap::new();
    for row in rows {
        let (id, row) = row?;
        out.insert(id, row);
    }
    Ok(out)
}

/// Seal every item that moves or retags a library track (every item but
/// an adoption) against its catalog rows as they are now. A track that is
/// no longer indexed at the item's source, or whose album has no
/// MusicBrainz identity, fails the seal.
pub fn seal_rows(
    db: &droppedneedle_kernel::db::Db,
    bundle: &mut PlanBundle,
) -> Result<(), PublishError> {
    let sealing = || bundle.items.iter().filter(|item| item.adopt.is_none());
    let current = db.read_blocking("publish.seal_rows", |conn| {
        own_rows(conn, sealing().map(|item| item.track_id.as_str()))
    })?;
    for item in bundle.items.iter_mut().filter(|item| item.adopt.is_none()) {
        let changed = || {
            PublishError::Validation(format!("Track {} changed under the preview", item.track_id))
        };
        let row = current.get(&item.track_id).ok_or_else(changed)?;
        if row.root_id != item.source_root
            || row.relative_path != item.source_rel
            || row.availability != "indexed"
        {
            return Err(changed());
        }
        let album_release_group_mbid = row.album_release_group_mbid.clone().ok_or_else(changed)?;
        item.sealed = Some(SealedRow {
            album_id: row.album_id.clone(),
            stat_revision: row.stat_revision.clone(),
            album_release_group_mbid,
            album_release_mbid: row.album_release_mbid.clone(),
            recording_mbid: row.recording_mbid.clone(),
            release_track_mbid: row.release_track_mbid.clone(),
        });
    }
    Ok(())
}

/// The first of `tracks` whose rows moved since its seal, read on `conn`:
/// the publish pregate's read, or the commit's own transaction. A track
/// with no seal counts as moved.
pub(crate) fn first_moved<'a>(
    conn: &rusqlite::Connection,
    tracks: &[(&'a str, Option<&CommitSeal>)],
) -> Result<Option<&'a str>, PublishError> {
    let current = own_rows(conn, tracks.iter().map(|(id, _)| *id))?;
    for (track_id, seal) in tracks {
        let unchanged = match (seal, current.get(*track_id)) {
            (Some(seal), Some(row)) => {
                let mapping = (seal.mapping_revision != 0).then_some(seal.mapping_revision as i64);
                row.root_id == seal.source_root
                    && row.relative_path == seal.source_rel
                    && row.availability == "indexed"
                    && row.stat_revision == seal.row.stat_revision
                    && row.album_id == seal.row.album_id
                    && row.album_revision == Some(seal.album_identity_revision as i64)
                    && row.album_release_group_mbid.as_deref()
                        == Some(seal.row.album_release_group_mbid.as_str())
                    && row.album_release_mbid == seal.row.album_release_mbid
                    && row.mapping_revision == mapping
                    && row.recording_mbid == seal.row.recording_mbid
                    && row.release_track_mbid == seal.row.release_track_mbid
            }
            _ => false,
        };
        if !unchanged {
            return Ok(Some(track_id));
        }
    }
    Ok(None)
}

/// Capability gate: unsupported format/field combinations hard-block
/// before mutation and are never silently half-managed.
#[derive(Debug, Clone, Default)]
pub struct CapabilityGate {
    /// Formats the staged writer can handle.
    pub writable_formats: HashSet<String>,
    /// Formats that can embed artwork.
    pub artwork_formats: HashSet<String>,
}

impl CapabilityGate {
    /// Production gate: every admitted format with the
    /// known artwork restriction that raw AAC cannot embed art. WMA
    /// stays out of both sets: no staged writer, no artwork path.
    pub fn production() -> Self {
        let writable: HashSet<String> = ["flac", "mp3", "ogg", "opus", "m4a", "aac", "wav"]
            .iter()
            .map(|name| (*name).to_string())
            .collect();
        let mut artwork = writable.clone();
        artwork.remove("aac");
        Self {
            writable_formats: writable,
            artwork_formats: artwork,
        }
    }

    /// Check one item, returning the blocker when the plan exceeds
    /// what the staged writer supports.
    pub fn check(&self, item: &PlanItem) -> Result<(), PublishError> {
        // Capabilities the staged writer cannot express yet fail
        // loudly here, never as a silent half-manage downstream.
        for capability in [Capability::Genre, Capability::Rename, Capability::Scrub] {
            if item.capabilities.contains(&capability) {
                return Err(PublishError::Capability(format!(
                    "capability {} is not supported by the staged writer",
                    capability.as_str()
                )));
            }
        }
        let format = item.format.to_lowercase();
        if item.capabilities.contains(&Capability::Metadata)
            && !self.writable_formats.contains(&format)
        {
            return Err(PublishError::Capability(format!(
                "format {format} has no staged writer"
            )));
        }
        if item.capabilities.contains(&Capability::Artwork)
            && !self.artwork_formats.contains(&format)
        {
            return Err(PublishError::Capability(format!(
                "format {format} cannot embed artwork"
            )));
        }
        if item.kind == PlanKind::Move
            && item.source_root != item.dest_root
            && !item.capabilities.contains(&Capability::CrossRootMove)
        {
            return Err(PublishError::Capability(
                "cross-root move without the cross-root capability".into(),
            ));
        }
        Ok(())
    }
}

/// Collision evidence for one blocked destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CollisionEvidence {
    /// What collided: audio, sidecar, or artwork.
    pub subject: String,
    /// Root and relative path of the occupied destination.
    pub root_id: String,
    pub rel_path: String,
    /// Whether the occupier holds identical bytes. Evidence and label
    /// only: identical occupancy is still never overwritten.
    pub identical_bytes: bool,
}

/// Collision gate: no occupied destination is ever overwritten, even
/// when the bytes look identical. The gate reports exact, folded,
/// and in-bundle collisions as explicit evidence.
pub struct CollisionGate;

impl CollisionGate {
    /// Check every audio and sidecar destination in a bundle against
    /// the live filesystem view plus the bundle itself.
    pub fn check_bundle(sandbox: &Sandbox, bundle: &PlanBundle) -> Result<(), PublishError> {
        let mut seen: HashSet<CollisionKey> = HashSet::new();
        let mut blockers: Vec<CollisionEvidence> = Vec::new();
        for item in bundle.items.iter() {
            Self::check_output(
                sandbox,
                &mut seen,
                &mut blockers,
                "audio",
                &item.dest_root,
                &item.dest_rel,
                SamePathSelf::new(item),
            )?;
            for sidecar in item.sidecars.iter() {
                Self::check_output(
                    sandbox,
                    &mut seen,
                    &mut blockers,
                    "sidecar",
                    &sidecar.dest_root,
                    &sidecar.dest_rel,
                    SamePathSelf::absent(),
                )?;
            }
            for output in item.outputs.iter() {
                Self::check_output(
                    sandbox,
                    &mut seen,
                    &mut blockers,
                    &output.kind,
                    &output.dest_root,
                    &output.dest_rel,
                    SamePathSelf::absent(),
                )?;
            }
        }
        if blockers.is_empty() {
            return Ok(());
        }
        let summary = blockers
            .iter()
            .map(|blocker| {
                format!(
                    "{}:{}:{}",
                    blocker.subject, blocker.root_id, blocker.rel_path
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        Err(PublishError::Collision(format!(
            "{} occupied destination(s): {summary}",
            blockers.len()
        )))
    }

    fn check_output(
        sandbox: &Sandbox,
        seen: &mut HashSet<CollisionKey>,
        blockers: &mut Vec<CollisionEvidence>,
        subject: &str,
        dest_root: &str,
        dest_rel: &str,
        same_path_self: SamePathSelf,
    ) -> Result<(), PublishError> {
        let key = collision_key(dest_root, dest_rel);
        if !seen.insert(key) {
            blockers.push(CollisionEvidence {
                subject: subject.to_string(),
                root_id: dest_root.to_string(),
                rel_path: dest_rel.to_string(),
                identical_bytes: false,
            });
            return Ok(());
        }
        let dest = sandbox.resolve_no_symlink(dest_root, dest_rel)?;
        match std::fs::symlink_metadata(&dest) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(PublishError::UnsafePath(format!(
                        "destination is a symlink: {dest_root}:{dest_rel}"
                    )));
                }
                if same_path_self.is_self_destination(dest_root, dest_rel)
                    || same_path_self.is_own_source(sandbox, dest_root, dest_rel, &dest)
                    || same_path_self.is_replaced(&dest)
                {
                    return Ok(());
                }
                let identical = same_path_self.bytes_match_if_regular(&dest);
                blockers.push(CollisionEvidence {
                    subject: subject.to_string(),
                    root_id: dest_root.to_string(),
                    rel_path: dest_rel.to_string(),
                    identical_bytes: identical,
                });
                Ok(())
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Self::check_fold_siblings(
                    sandbox,
                    blockers,
                    subject,
                    dest_root,
                    dest_rel,
                    &same_path_self,
                )?;
                Ok(())
            }
            Err(err) => Err(PublishError::Io(err.to_string())),
        }
    }

    /// Reject a destination whose name folds together with an existing
    /// sibling, so case/Unicode lookalikes can never publish side by
    /// side on any filesystem.
    fn check_fold_siblings(
        sandbox: &Sandbox,
        blockers: &mut Vec<CollisionEvidence>,
        subject: &str,
        dest_root: &str,
        dest_rel: &str,
        same_path_self: &SamePathSelf,
    ) -> Result<(), PublishError> {
        let dest = sandbox.resolve_no_symlink(dest_root, dest_rel)?;
        let parent = dest.parent().map(std::path::Path::to_path_buf);
        let Some(parent) = parent else {
            return Ok(());
        };
        if std::fs::symlink_metadata(&parent).is_err() {
            return Ok(());
        }
        let wanted = dest
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
        let wanted_key = collision_key(dest_root, &wanted);
        let entries = std::fs::read_dir(&parent).map_err(PublishError::from)?;
        for entry in entries {
            let entry = entry.map_err(PublishError::from)?;
            let name = entry.file_name();
            let Some(text) = name.to_str() else {
                continue;
            };
            // A case-only rename folds onto its own source, which is
            // leaving: that is the file itself, not a collision.
            if text == wanted
                || same_path_self.is_own_source(sandbox, dest_root, dest_rel, &parent.join(text))
            {
                continue;
            }
            if collision_key(dest_root, text) == wanted_key {
                blockers.push(CollisionEvidence {
                    subject: subject.to_string(),
                    root_id: dest_root.to_string(),
                    rel_path: dest_rel.to_string(),
                    identical_bytes: false,
                });
                return Ok(());
            }
        }
        Ok(())
    }
}

/// Same-path writes legitimately target their own source path; the
/// gate exempts exactly that destination and nothing else.
struct SamePathSelf {
    root: Option<String>,
    rel: Option<String>,
    sha256: Option<String>,
    /// A move's own source: a destination that is this file (a case-only
    /// rename) is not occupied by anyone else.
    move_source: Option<(String, String)>,
    /// The bytes of a file this item replaces at its destination.
    replaces_sha256: Option<String>,
}

impl SamePathSelf {
    fn new(item: &PlanItem) -> Self {
        if item.kind == PlanKind::SamePath {
            Self {
                root: Some(item.source_root.clone()),
                rel: Some(item.source_rel.clone()),
                sha256: Some(item.fingerprint.sha256.clone()),
                move_source: None,
                replaces_sha256: None,
            }
        } else {
            Self {
                move_source: Some((item.source_root.clone(), item.source_rel.clone())),
                replaces_sha256: item
                    .replaces
                    .as_ref()
                    .map(|replaced| replaced.sha256.clone()),
                ..Self::absent()
            }
        }
    }

    fn absent() -> Self {
        Self {
            root: None,
            rel: None,
            sha256: None,
            move_source: None,
            replaces_sha256: None,
        }
    }

    /// True when the occupant is the file this item replaces, still
    /// holding the bytes the plan saw.
    fn is_replaced(&self, dest: &std::path::Path) -> bool {
        let Some(wanted) = self.replaces_sha256.as_deref() else {
            return false;
        };
        super::paths::read_regular_file(dest)
            .is_ok_and(|bytes| super::snapshots::sha256_hex(&bytes) == wanted)
    }

    /// True when `path`, found at or folding onto `dest_root:dest_rel`, is
    /// the moving file itself: the names fold together (a case-only
    /// rename) and it is the same file. A hard link elsewhere is not.
    fn is_own_source(
        &self,
        sandbox: &Sandbox,
        dest_root: &str,
        dest_rel: &str,
        path: &std::path::Path,
    ) -> bool {
        let Some((root, rel)) = &self.move_source else {
            return false;
        };
        collision_key(root, rel) == collision_key(dest_root, dest_rel)
            && sandbox
                .resolve_no_symlink(root, rel)
                .is_ok_and(|source| source == path || super::paths::same_file(&source, path))
    }

    fn is_self_destination(&self, root: &str, rel: &str) -> bool {
        self.root.as_deref() == Some(root) && self.rel.as_deref() == Some(rel)
    }

    fn bytes_match_if_regular(&self, dest: &std::path::Path) -> bool {
        let Some(wanted) = self.sha256.as_deref() else {
            return false;
        };
        let Ok(bytes) = super::paths::read_regular_file(dest) else {
            return false;
        };
        super::snapshots::sha256_hex(&bytes) == wanted
    }
}

/// Disk preflight: the summed staged-byte estimate must fit the
/// reported free space before the first byte is staged.
pub struct DiskPreflight;

impl DiskPreflight {
    /// Run preflight for a bundle against a space probe.
    pub fn check<P: SpaceProbe>(bundle: &PlanBundle, probe: &P) -> Result<(), PublishError> {
        let mut per_root: BTreeMap<&str, u64> = BTreeMap::new();
        for item in bundle.items.iter() {
            *per_root.entry(item.dest_root.as_str()).or_insert(0) += item.staged_bytes_estimate;
        }
        for (root, needed) in per_root {
            let free = probe.free_bytes(root)?;
            if free < needed {
                return Err(PublishError::Space(format!(
                    "root {root} needs {needed} bytes but has {free} free"
                )));
            }
        }
        Ok(())
    }
}

/// Destination free-space probe, injected so tests pin exact values.
pub trait SpaceProbe {
    /// Free bytes on the filesystem holding `root_id`.
    fn free_bytes(&self, root_id: &str) -> Result<u64, PublishError>;
}

/// Sealed preview: the immutable plan plus the token and revisions
/// Apply must recheck. Sealing never replans; Apply consumes exactly
/// what was sealed.
#[derive(Debug, Clone)]
pub struct SealedPreview {
    /// The immutable bundle.
    pub bundle: PlanBundle,
    /// Opaque token hash; confirmation must present the matching token.
    pub token_hash: String,
    /// Unix day the preview expires (exclusive).
    pub expires_day: i64,
    /// Settings revision pinned at seal time.
    pub settings_revision: u64,
}

/// Why a sealed preview no longer applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SealError {
    /// A source file changed, moved, or became unreadable.
    StaleFile(String),
    /// Accepted identity or a per-track mapping changed.
    StaleIdentity(String),
    /// The effective profile, naming, or scripts changed.
    StaleProfile(String),
    /// Policy (root assignment, triggers, retention) changed.
    StalePolicy(String),
    /// Wrong confirmation token.
    BadToken,
    /// Preview expired.
    Expired,
}

/// Live state Apply rechecks the seal against.
#[derive(Debug, Clone)]
pub struct SealRecheck {
    /// Current source fingerprints by track id.
    pub fingerprints: BTreeMap<String, FileFingerprint>,
    /// Current accepted identities by track id.
    pub identities: BTreeMap<String, ReleaseIdentity>,
    /// Current override revisions by track id.
    pub overrides: BTreeMap<String, u64>,
    /// Current effective profile revision.
    pub profile_revision: u64,
    /// Current naming revision.
    pub naming_revision: u64,
    /// Current global policy revision.
    pub policy_revision: u64,
    /// Current settings revision.
    pub settings_revision: u64,
    /// Today as a unix day.
    pub today_day: i64,
    /// Presented confirmation token hash.
    pub token_hash: String,
}

impl SealedPreview {
    /// Seal a bundle with its token and settings revision.
    pub fn seal(
        bundle: PlanBundle,
        token_hash: String,
        settings_revision: u64,
        expires_day: i64,
    ) -> Self {
        Self {
            bundle,
            token_hash,
            expires_day,
            settings_revision,
        }
    }

    /// Consume the exact sealed preview, rejecting any stale
    /// file, identity, profile, or policy state first.
    pub fn recheck(&self, live: &SealRecheck) -> Result<(), SealError> {
        if live.token_hash != self.token_hash {
            return Err(SealError::BadToken);
        }
        if live.today_day >= self.expires_day {
            return Err(SealError::Expired);
        }
        if live.settings_revision != self.settings_revision
            || live.profile_revision != self.bundle.profile_revision
            || live.naming_revision != self.bundle.naming_revision
        {
            return Err(SealError::StaleProfile("profile or settings moved".into()));
        }
        if live.policy_revision != self.bundle.policy_revision {
            return Err(SealError::StalePolicy("policy revision moved".into()));
        }
        for item in self.bundle.items.iter() {
            match live.fingerprints.get(&item.track_id) {
                Some(current) if *current == item.fingerprint => {}
                _ => {
                    return Err(SealError::StaleFile(format!(
                        "track {} changed under the preview",
                        item.track_id
                    )));
                }
            }
            match live.identities.get(&item.track_id) {
                Some(current) if *current == item.identity => {}
                _ => {
                    return Err(SealError::StaleIdentity(format!(
                        "identity for track {} changed",
                        item.track_id
                    )));
                }
            }
            match live.overrides.get(&item.track_id) {
                Some(current) if *current == item.override_revision => {}
                _ => {
                    return Err(SealError::StaleProfile(format!(
                        "override for track {} changed",
                        item.track_id
                    )));
                }
            }
        }
        Ok(())
    }
}

/// Why automatic work holds a unit in staging.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutomaticHold {
    /// The root is not enabled for management.
    RootDisabled,
    /// The trigger for this acquisition kind is off.
    TriggerDisabled(String),
    /// No accepted exact MusicBrainz release for the unit.
    NoAcceptedRelease,
    /// At least one affected file lacks an accepted track mapping.
    MissingTrackMapping(String),
}

/// Automatic eligibility: enabled root plus enabled trigger plus an
/// accepted exact release plus a full track mapping.
pub struct AutomaticEligibility;

impl AutomaticEligibility {
    /// Check one acquisition unit. Owning every track from the release
    /// is not required; mapping every affected file is.
    ///
    /// Manual Apply is the only wired trigger, so nothing calls this
    /// outside the tests yet. The first automatic trigger (root
    /// watcher, scheduler, or acquisition completion) must gate its
    /// units through this check before planning.
    pub fn check(
        root_enabled: bool,
        trigger: &str,
        trigger_enabled: bool,
        accepted_release: Option<&ReleaseIdentity>,
        mappings: &BTreeMap<String, TrackMapping>,
        affected_tracks: &[String],
    ) -> Result<(), AutomaticHold> {
        if !root_enabled {
            return Err(AutomaticHold::RootDisabled);
        }
        if !trigger_enabled {
            return Err(AutomaticHold::TriggerDisabled(trigger.to_string()));
        }
        let Some(release) = accepted_release else {
            return Err(AutomaticHold::NoAcceptedRelease);
        };
        if release.release_mbid.is_empty() || release.release_track_mbid.is_empty() {
            return Err(AutomaticHold::NoAcceptedRelease);
        }
        for track in affected_tracks {
            match mappings.get(track) {
                Some(mapping) if !mapping.release_track_mbid.is_empty() => {}
                _ => return Err(AutomaticHold::MissingTrackMapping(track.clone())),
            }
        }
        Ok(())
    }
}
