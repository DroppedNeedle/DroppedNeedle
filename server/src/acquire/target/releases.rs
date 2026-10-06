//! Order Usenet releases against the target. An NZB is opaque until it
//! downloads, so the evidence is the release title and the file count the
//! indexer reports: releases that name the album (and its artist) come
//! first, then those whose file count matches the tracklist. Ties keep the
//! indexer's order, so the list is the same on every failover.

use super::SearchTarget;
use crate::acquire::usenet::newznab::IndexerResult;
use crate::library::matching::strings::fold;

/// Best album fit first; stable.
pub fn order_releases(mut hits: Vec<IndexerResult>, target: &SearchTarget) -> Vec<IndexerResult> {
    let album = fold(&target.album_title);
    let artist = fold(crate::acquire::slskd::query::primary_artist(&target.artist));
    let tracks = target.tracklist.len();
    hits.sort_by_key(|hit| {
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
        (names, files)
    });
    hits
}
