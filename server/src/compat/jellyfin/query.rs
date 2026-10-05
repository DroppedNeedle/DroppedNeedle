//! Pure query helpers for the browse routes: item-type priority, paging,
//! text matching and the sort orders.

use super::params::SortKey;

/// `IncludeItemTypes` priority: MusicArtist > MusicAlbum > Audio > Playlist >
/// MusicGenre, default album (v2 `_primary_type`). Type names are
/// case-sensitive, like v2's set membership.
pub(super) fn primary_type(types: &[String]) -> &'static str {
    for candidate in [
        "MusicArtist",
        "MusicAlbum",
        "Audio",
        "Playlist",
        "MusicGenre",
    ] {
        if types.iter().any(|t| t == candidate) {
            return candidate;
        }
    }
    "MusicAlbum"
}

/// Page a built list: `limit == 0` means ALL from start (v2 `_build_page`).
pub(super) fn page<T: Clone>(items: &[T], start: usize, limit: usize) -> (Vec<T>, usize) {
    let total = items.len();
    let page = if start >= total {
        Vec::new()
    } else if limit == 0 {
        items[start..].to_vec()
    } else {
        items[start..(start.saturating_add(limit)).min(total)].to_vec()
    };
    (page, total)
}

pub(super) fn matches(haystacks: &[Option<String>], needle: &str) -> bool {
    let needle = needle.to_lowercase();
    haystacks
        .iter()
        .flatten()
        .any(|h| h.to_lowercase().contains(&needle))
}

pub(super) fn flip(desc: bool, ord: std::cmp::Ordering) -> std::cmp::Ordering {
    if desc { ord.reverse() } else { ord }
}

pub(super) fn sort_tracks(tracks: &mut [super::seams::TrackView], key: SortKey, desc: bool) {
    tracks.sort_by(|a, b| {
        let ord = match key {
            SortKey::Recent => a
                .created_at
                .partial_cmp(&b.created_at)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortKey::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            SortKey::Year | SortKey::PremiereDate => a.year.cmp(&b.year),
            SortKey::Random => fnv(&a.file_id).cmp(&fnv(&b.file_id)),
            SortKey::DatePlayed => a
                .last_played
                .partial_cmp(&b.last_played)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortKey::PlayCount => a.play_count.cmp(&b.play_count),
        };
        flip(desc, ord.then_with(|| a.file_id.cmp(&b.file_id)))
    });
}

pub(super) fn sort_albums(albums: &mut [super::seams::AlbumView], key: SortKey, desc: bool) {
    albums.sort_by(|a, b| {
        let ord = match key {
            SortKey::Recent => a
                .date_added
                .partial_cmp(&b.date_added)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortKey::Title => a.title.to_lowercase().cmp(&b.title.to_lowercase()),
            SortKey::Year | SortKey::PremiereDate => a.year.cmp(&b.year),
            SortKey::Random => fnv(&a.rg_mbid).cmp(&fnv(&b.rg_mbid)),
            SortKey::DatePlayed => a
                .last_played
                .partial_cmp(&b.last_played)
                .unwrap_or(std::cmp::Ordering::Equal),
            SortKey::PlayCount => a.play_count.cmp(&b.play_count),
        };
        flip(desc, ord.then_with(|| a.rg_mbid.cmp(&b.rg_mbid)))
    });
}

/// Stable stand-in shuffle for `SortBy=Random` (the real discover ordering
/// is not bound yet; tests only pin stability + completeness).
pub(super) fn fnv(s: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in s.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::jellyfin::seams::TrackView;

    fn track(id: &str, title: &str, year: i32) -> TrackView {
        TrackView {
            file_id: id.to_owned(),
            title: title.to_owned(),
            year: Some(year),
            ..TrackView::default()
        }
    }

    #[test]
    fn paging_treats_zero_limit_as_everything_from_start() {
        let items = [1, 2, 3, 4];
        assert_eq!(page(&items, 1, 2), (vec![2, 3], 4));
        assert_eq!(page(&items, 1, 0), (vec![2, 3, 4], 4));
        assert_eq!(page(&items, 9, 2), (Vec::new(), 4));
    }

    #[test]
    fn item_type_priority_and_text_match() {
        let types = ["Audio".to_owned(), "MusicArtist".to_owned()];
        assert_eq!(primary_type(&types), "MusicArtist");
        assert_eq!(primary_type(&[]), "MusicAlbum");
        assert!(matches(&[None, Some("Dummy".to_owned())], "dum"));
        assert!(!matches(&[Some("Dummy".to_owned())], "x"));
    }

    #[test]
    fn sorts_break_ties_by_id_and_flip_whole_order() {
        let mut tracks = vec![track("b", "Same", 2000), track("a", "Same", 1990)];
        sort_tracks(&mut tracks, SortKey::Title, false);
        assert_eq!(tracks[0].file_id, "a", "ties fall back to the id");
        sort_tracks(&mut tracks, SortKey::Year, true);
        assert_eq!(tracks[0].year, Some(2000));
    }
}
