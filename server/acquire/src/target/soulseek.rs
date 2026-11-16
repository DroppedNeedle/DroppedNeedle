//! Search Soulseek for a target and pick the folder to fetch from.
//!
//! Albums and single tracks both search with the album query ladder and
//! rank folders with [`rank_folders`]. A single track only falls back to
//! the track query ladder when no album folder holds it, and a lone-track
//! share is taken only when nothing better exists; the caller records the
//! reason.

use super::SearchTarget;
use super::reasons::TrackReason;
use crate::client_error::ClientError;
use crate::reason::Reason;
use crate::slskd::folders::{FolderPick, RankRequest, rank_folders, rank_listing};
use crate::slskd::{DownloadPolicy, SearchResult, SlskdHttp, SlskdRepository};

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
    Search(ClientError),
    /// Nothing usable was found: why, and the text the log keeps.
    Nothing { reason: Reason, detail: String },
}

const NO_FOLDER: &str = "slskd has no untried folder for this album";

/// Why no folder could be picked: the settings refused a folder that holds
/// some of the release, or else `reason`.
fn miss(
    hits: &[SearchResult],
    request: RankRequest<'_>,
    policy: &DownloadPolicy,
    reason: Reason,
    detail: &str,
) -> SoulseekMiss {
    if rank_listing(hits, request, policy)
        .iter()
        .all(FolderPick::holds_nothing)
    {
        return SoulseekMiss::Nothing {
            reason,
            detail: detail.to_owned(),
        };
    }
    SoulseekMiss::Nothing {
        reason: Reason::NothingAllowed,
        detail: "slskd has folders for this, but none in your quality settings".to_owned(),
    }
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
                return Err(miss(&hits, request, policy, Reason::NoSource, NO_FOLDER));
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
        return Err(miss(&hits, request, policy, Reason::NoSource, NO_FOLDER));
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
        return Err(miss(
            &hits,
            request,
            policy,
            TrackReason::TrackNotFound.into(),
            &TrackReason::TrackNotFound.to_string(),
        ));
    };
    let lone_reason = if pick.lone_track || !target.has_album() {
        Some(track.no_album.unwrap_or(TrackReason::NoAlbumFolder))
    } else {
        None
    };
    Ok(SoulseekChoice { pick, lone_reason })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::downloads::plan::ExpectedTrack;

    /// A folder that holds none of the release's tracks was not refused by
    /// the settings, so a miss keeps its own reason.
    #[test]
    fn a_folder_holding_no_track_is_not_blamed_on_settings() {
        let tracklist = [ExpectedTrack {
            track_number: 1,
            disc_number: 1,
            duration_seconds: Some(200.0),
            recording_mbid: None,
            title: Some("Alpha".to_owned()),
            release_track_mbid: None,
        }];
        let stranger = SearchResult {
            username: "stranger".to_owned(),
            filename: "M\\Other Album\\Unrelated Song.flac".to_owned(),
            parent_directory: String::new(),
            size: 1,
            extension: "flac".to_owned(),
            bitrate: None,
            bit_depth: Some(16),
            sample_rate: Some(44_100),
            duration: Some(500.0),
            has_free_slot: true,
            upload_speed: 1,
            queue_length: 0,
        };
        let request = RankRequest {
            tracklist: &tracklist,
            wanted: &[],
            single_track: false,
        };
        let SoulseekMiss::Nothing { reason, .. } = miss(
            &[stranger],
            request,
            &DownloadPolicy::default(),
            Reason::NoSource,
            NO_FOLDER,
        ) else {
            panic!("no folder was picked, so the result is a miss");
        };
        assert_eq!(reason, Reason::NoSource);
    }
}
