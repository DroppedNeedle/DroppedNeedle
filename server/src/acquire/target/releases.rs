//! Order Usenet releases against the target. An NZB is opaque until it
//! downloads, so the evidence is the release title and the file count the
//! indexer reports: releases that name the album (and its artist) come
//! first, then those whose file count matches the tracklist. Ties keep the
//! indexer's order, so the list is the same on every failover.

use super::SearchTarget;
use crate::acquire::usenet::newznab::IndexerResult;
use crate::library::matching::strings::fold;

/// How well one release's title and file count fit the target. Lower
/// is better on both counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseFit {
    /// 0: names the album and artist; 1: the album only; 2: neither.
    pub names: u8,
    /// 0: the file count fits the tracklist; 1: unknown; 2: it does not.
    pub files: u8,
}

/// Judge one release against the target.
pub fn release_fit(hit: &IndexerResult, target: &SearchTarget) -> ReleaseFit {
    let album = fold(&target.album_title);
    let artist = fold(crate::acquire::slskd::query::primary_artist(&target.artist));
    let tracks = target.tracklist.len();
    let title = fold(&hit.usenet.title);
    let names = match (
        !album.is_empty() && title.contains(&album),
        !artist.is_empty() && title.contains(&artist),
    ) {
        (true, true) => 0,
        (true, false) => 1,
        _ => 2,
    };
    let files = match (hit.usenet.files, tracks) {
        (Some(files), tracks) if tracks > 0 => {
            let files = usize::try_from(files.max(0)).unwrap_or(0);
            // Releases often carry an .nfo, .sfv or cover besides the audio.
            if files >= tracks && files <= tracks + 4 {
                0
            } else {
                2
            }
        }
        _ => 1,
    };
    ReleaseFit { names, files }
}

/// Best album fit first; stable.
pub fn order_releases(mut hits: Vec<IndexerResult>, target: &SearchTarget) -> Vec<IndexerResult> {
    hits.sort_by_key(|hit| release_fit(hit, target));
    hits
}
