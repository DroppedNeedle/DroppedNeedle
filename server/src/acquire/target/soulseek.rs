//! Search Soulseek for a target and pick the folder to fetch from.
//!
//! Albums and single tracks both search with the album query ladder and
//! rank folders with [`rank_folders`]. A single track only falls back to
//! the track query ladder when no album folder holds it, and a lone-track
//! share is taken only when nothing better exists; the caller records the
//! reason.

use super::SearchTarget;
use super::reasons::TrackReason;
use crate::acquire::slskd::folders::{FolderPick, RankRequest, rank_folders};
use crate::acquire::slskd::{SearchResult, SlskdError, SlskdHttp, SlskdRepository};

/// The folder chosen, and why a lone-track share was used when it was.
#[derive(Debug, Clone)]
pub struct SoulseekChoice {
    /// The folder and the files to enqueue from it.
    pub pick: FolderPick,
    /// Set when the pick is a lone-track share.
    pub lone_reason: Option<TrackReason>,
}

/// Why nothing was picked.
#[derive(Debug)]
pub enum SoulseekMiss {
    /// slskd failed.
    Search(SlskdError),
    /// Nothing usable was found.
    Nothing(String),
}

/// Search and pick. `keep` drops hits that may not be tried again
/// (quarantined files, peers this task already tried).
pub async fn choose<H: SlskdHttp>(
    repo: &SlskdRepository<H>,
    target: &SearchTarget,
    keep: impl Fn(&SearchResult) -> bool,
) -> Result<SoulseekChoice, SoulseekMiss> {
    let request = RankRequest {
        tracklist: &target.tracklist,
        wanted: &target.wanted,
        single_track: target.track.is_some(),
    };
    let policy = repo.policy();
    let mut hits: Vec<SearchResult> = Vec::new();
    if target.has_album() {
        hits = repo
            .search_album(&target.artist, &target.album_title, target.year)
            .await
            .map_err(SoulseekMiss::Search)?
            .into_iter()
            .filter(|hit| keep(hit))
            .collect();
        let best = rank_folders(&hits, request, policy).into_iter().next();
        match (&target.track, best) {
            (None, Some(pick)) => {
                return Ok(SoulseekChoice {
                    pick,
                    lone_reason: None,
                });
            }
            (None, None) => {
                return Err(SoulseekMiss::Nothing(
                    "slskd has no untried folder for this album".to_owned(),
                ));
            }
            (Some(_), Some(pick)) if !pick.lone_track => {
                return Ok(SoulseekChoice {
                    pick,
                    lone_reason: None,
                });
            }
            (Some(_), _) => {}
        }
    }
    let Some(track) = target.track.as_ref() else {
        return Err(SoulseekMiss::Nothing(
            "slskd has no untried folder for this album".to_owned(),
        ));
    };
    // No album folder holds the track: search for the track itself. Its
    // results can still turn up an album folder, which wins as usual.
    let album = target.has_album().then_some(target.album_title.as_str());
    let more = repo
        .search_track(&track.artist, &track.title, album)
        .await
        .map_err(SoulseekMiss::Search)?;
    for hit in more {
        let seen = hits
            .iter()
            .any(|known| known.username == hit.username && known.filename == hit.filename);
        if !seen && keep(&hit) {
            hits.push(hit);
        }
    }
    let Some(pick) = rank_folders(&hits, request, policy).into_iter().next() else {
        return Err(SoulseekMiss::Nothing(
            TrackReason::TrackNotFound.to_string(),
        ));
    };
    let lone_reason = if pick.lone_track || !target.has_album() {
        Some(track.no_album.unwrap_or(TrackReason::NoAlbumFolder))
    } else {
        None
    };
    Ok(SoulseekChoice { pick, lone_reason })
}
