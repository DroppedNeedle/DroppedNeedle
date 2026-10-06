//! Seam implementations between the library modules.
//!
//! Each adapter is narrow by design: it translates at a module boundary
//! and owns no domain logic. Gaps with no implementation yet (AcoustID key
//! config, album projection for contributions) fail loudly or stay
//! dormant, never silently wrong; each carries a note.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::contrib::error::ContribError;
use super::contrib::models::{
    ContributionRecord, MusicBrainzUrlResolution, MusicBrainzVerifiedRelease,
};
use super::contrib::seams::{
    AlbumIdentificationContext, AttachmentCandidate, AttachmentDecision, AttachmentEvidence,
    AttachmentOutcome, ContributionCatalog, ContributionIdentity, DuplicateSearchFacts,
    MusicBrainzContrib, ProviderFailure, UrlRelation,
};
use super::publish::PublishError;
use super::publish::planner::SpaceProbe;
use super::scan::seams::{ScannedTags, TagReadError, TagReader};

// ---------------------------------------------------------------------------
// Scan -> tags: read-only tag access over the tags module.
// ---------------------------------------------------------------------------

/// Read-only tag reader over [`crate::library::tags`]. Never writes:
/// tags and stream properties come from the container headers (no audio
/// decode), and every failure maps onto the scan seam's two errors. A
/// parser panic on a malformed file is caught and reads as a fatal tag
/// failure for that file instead of taking the scan worker down.
pub struct LoftyTagReader;

impl TagReader for LoftyTagReader {
    fn read_tags(&self, path: &Path) -> Result<ScannedTags, TagReadError> {
        let format = super::tags::format_for_path(path).map_err(|_| TagReadError::Fatal)?;
        let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            super::tags::read::read_scan_metadata(path, format)
        }));
        match read {
            Ok(Ok((tag, header))) => Ok(ScannedTags { tag, header }),
            Ok(Err(error)) => Err(map_tag_read_error(error)),
            Err(_) => {
                tracing::error!(path = %path.display(), "tag parser panicked; file skipped");
                Err(TagReadError::Fatal)
            }
        }
    }
}

/// Map a tag failure onto the scan seam. I/O races (a file vanishing
/// mid-read, contended disk) defer with re-offer; unrecognized and
/// unparseable files are fatal for this file.
fn map_tag_read_error(error: super::tags::TagsError) -> TagReadError {
    match error {
        super::tags::TagsError::Io { .. } => TagReadError::Deferred,
        _ => TagReadError::Fatal,
    }
}

// ---------------------------------------------------------------------------
// Publish -> filesystem: free-space probe over statvfs.
// ---------------------------------------------------------------------------

/// Free-space probe over `statvfs` on each root directory. The root
/// lookup re-reads the shared registry every call, never a captured
/// snapshot.
pub struct FsSpaceProbe {
    root_dirs: Arc<dyn Fn() -> HashMap<String, PathBuf> + Send + Sync>,
}

impl FsSpaceProbe {
    pub fn new(root_dirs: Arc<dyn Fn() -> HashMap<String, PathBuf> + Send + Sync>) -> Self {
        Self { root_dirs }
    }
}

impl SpaceProbe for FsSpaceProbe {
    fn free_bytes(&self, root_id: &str) -> Result<u64, PublishError> {
        let dirs = (self.root_dirs)();
        let dir = dirs
            .get(root_id)
            .ok_or_else(|| PublishError::UnsafePath(format!("unknown root {root_id}")))?;
        free_bytes_for(dir)
    }
}

fn free_bytes_for(dir: &Path) -> Result<u64, PublishError> {
    let text = dir.to_string_lossy().into_owned();
    let cstr = std::ffi::CString::new(text)
        .map_err(|_| PublishError::UnsafePath("root path holds a NUL byte".into()))?;
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // statvfs only reads filesystem metadata; the path is already sandbox-checked.
    let status = unsafe { libc::statvfs(cstr.as_ptr(), &mut stat) };
    if status != 0 {
        return Err(PublishError::Io(format!(
            "cannot stat free space under {}",
            dir.display()
        )));
    }
    Ok(stat.f_bavail * stat.f_frsize)
}

// ---------------------------------------------------------------------------
// Contrib ports: dormant implementations.
// ---------------------------------------------------------------------------

/// Contribution identity over the (not yet projected) album catalog.
/// Album rows with titles do not exist durably yet: scan commits
/// per-file catalog entries without titles, and nothing projects albums
/// yet. Returning `None` keeps the service loud (`AlbumNotFound`) instead
/// of drafting from empty titles; the verification worker stays idle
/// until a projection exists.
pub struct EmptyContributionIdentity;

impl ContributionIdentity for EmptyContributionIdentity {
    fn album_context<'a>(
        &'a self,
        _album_id: &'a str,
    ) -> BoxFuture<'a, Option<AlbumIdentificationContext>> {
        Box::pin(async move { None })
    }

    fn input_revisions(
        &self,
        _tracks: &[super::contrib::seams::IdentityTrack],
    ) -> (String, String, String) {
        (String::new(), String::new(), String::new())
    }
}

/// Contribution catalog port. Reads serve straight from SQLite with
/// no identity-bearing cache to invalidate and no reindex hook to
/// fire, so all three calls are no-ops (traced, so a future
/// cache lands its invalidation here).
pub struct NoopContributionCatalog;

impl ContributionCatalog for NoopContributionCatalog {
    fn invalidate_identity_scope<'a>(
        &'a self,
        album_mbids: &'a [String],
        artist_mbids: &'a [String],
    ) -> BoxFuture<'a, ()> {
        let albums = album_mbids.len();
        let artists = artist_mbids.len();
        Box::pin(async move {
            tracing::debug!(
                albums,
                artists,
                "contribution catalog invalidation: no cached scope to clear"
            );
        })
    }

    fn invalidate_identification<'a>(&'a self) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            tracing::debug!("contribution catalog sweep: no cached scope to clear");
        })
    }

    fn after_identified<'a>(
        &'a self,
        local_album_id: &'a str,
        input_policy_revision: &'a str,
    ) -> BoxFuture<'a, ()> {
        let album = local_album_id.to_owned();
        let policy = input_policy_revision.to_owned();
        Box::pin(async move {
            tracing::debug!(
                album,
                policy,
                "contribution linked: no reindex hook registered"
            );
        })
    }
}

/// Attachment evidence: contradiction-only check in the v2 shape
/// (curator-verified release, no lone quorum). An empty verified
/// MBID or a title that disagrees with the draft after casefolding
/// needs review; anything else attaches. The full evidence engine
/// port stays a follow-up; this never invents a match, it only
/// refuses to contradict the curator silently.
pub struct MinimalAttachmentEvidence;

impl AttachmentEvidence for MinimalAttachmentEvidence {
    fn matcher_version(&self) -> String {
        "v3-minimal-1".to_owned()
    }

    fn decide_attachment<'a>(
        &'a self,
        contribution: &'a ContributionRecord,
        verified: &'a MusicBrainzVerifiedRelease,
        recording_mbids: &'a HashMap<String, Option<String>>,
        _relative_paths: &'a HashMap<String, String>,
    ) -> BoxFuture<'a, AttachmentDecision> {
        let candidate = AttachmentCandidate {
            release_group_mbid: verified.release_group_mbid.clone(),
            release_mbid: Some(verified.release_mbid.clone()),
            artist_mbid: verified.artist_mbid.clone(),
        };
        let key = candidate.key();
        let decision = if verified.release_mbid.trim().is_empty() {
            AttachmentDecision {
                outcome: AttachmentOutcome::NeedsReview,
                reason_code: Some("VERIFIED_MBID_MISSING".to_owned()),
                selected_candidate_key: None,
                candidates: vec![candidate],
            }
        } else if !titles_agree(
            contribution.draft.title.value.as_deref().unwrap_or(""),
            &verified.title,
        ) || recording_mbids
            .values()
            .any(|mbid| mbid.as_deref().is_some_and(|mbid| mbid.trim().is_empty()))
        {
            AttachmentDecision {
                outcome: AttachmentOutcome::NeedsReview,
                reason_code: Some("ATTACHMENT_CONTRADICTION".to_owned()),
                selected_candidate_key: None,
                candidates: vec![candidate],
            }
        } else {
            AttachmentDecision {
                outcome: AttachmentOutcome::Identified,
                reason_code: None,
                selected_candidate_key: Some(key),
                candidates: vec![candidate],
            }
        };
        Box::pin(async move { decision })
    }
}

fn titles_agree(draft: &str, verified: &str) -> bool {
    let fold = |value: &str| caseless::default_case_fold_str(&value.trim().to_lowercase());
    draft.trim().is_empty() || verified.trim().is_empty() || fold(draft) == fold(verified)
}

/// MusicBrainz contribution reads with no client behind them. The
/// provider clients speak different seams (no background-priority
/// lanes) and are not adapted yet; until then verification
/// lookups defer as unavailable (the worker retries inside its
/// bounded window) and duplicate checks fail loudly. No live provider
/// contact happens through this adapter, ever.
pub struct UnavailableMusicBrainz;

impl MusicBrainzContrib for UnavailableMusicBrainz {
    fn resolve_url<'a>(
        &'a self,
        _url: &'a str,
        _relation: UrlRelation,
        _priority: crate::providers::slots::RequestPriority,
        _bypass_cache: bool,
    ) -> BoxFuture<'a, Result<MusicBrainzUrlResolution, ContribError>> {
        Box::pin(async move {
            Err(ContribError::Data(
                "The MusicBrainz adapter is not available.".into(),
            ))
        })
    }

    fn get_release_for_verification<'a>(
        &'a self,
        _release_mbid: &'a str,
        _priority: crate::providers::slots::RequestPriority,
        _bypass_cache: bool,
    ) -> BoxFuture<'a, Result<Option<MusicBrainzVerifiedRelease>, ProviderFailure>> {
        Box::pin(async move {
            Err(ProviderFailure::Unavailable {
                retry_after_seconds: None,
            })
        })
    }

    fn search_duplicate_releases<'a>(
        &'a self,
        _facts: &'a DuplicateSearchFacts,
        _limit: usize,
        _priority: crate::providers::slots::RequestPriority,
    ) -> BoxFuture<'a, Result<Vec<MusicBrainzVerifiedRelease>, ContribError>> {
        Box::pin(async move {
            Err(ContribError::Data(
                "The MusicBrainz adapter is not available.".into(),
            ))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new("tests/fixtures/library").join(name)
    }

    /// The scan reads the same tags the full read does, and its header
    /// duration agrees with the decode-counted probe, without decoding.
    #[test]
    fn scan_read_matches_full_read() {
        let reader = LoftyTagReader;
        for name in [
            "flac_full_01.flac",
            "flac_no_tags.flac",
            "mp3_full_01.mp3",
            "m4a_full_01.m4a",
            "management_full.ogg",
            "management_full.opus",
            "management_full.wav",
            "management_full.aac",
        ] {
            let path = fixture(name);
            let format = super::super::tags::format_for_path(&path).expect("fixture format");
            let scanned = reader.read_tags(&path).expect("scan read");
            let full = super::super::tags::read::read_tag_only(&path, format).expect("full read");
            assert_eq!(scanned.tag, full, "fixture {name}");
            let probed = super::super::tags::probe(&path)
                .expect("probe")
                .duration_seconds;
            let header = scanned.header.duration_seconds.expect("header duration");
            assert!(
                (header - probed).abs() < 0.5,
                "fixture {name}: {header} vs {probed}"
            );
        }
        assert_eq!(
            reader.read_tags(&fixture("management_full.wma")),
            Err(TagReadError::Fatal)
        );
        // A vanished file is an I/O failure: deferred, offered again.
        assert_eq!(
            reader.read_tags(Path::new("/nonexistent-missing-file.mp3")),
            Err(TagReadError::Deferred)
        );
    }
}
