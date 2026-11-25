//! Pure query helpers for the browse routes: item-type priority, the page
//! size cap and in-memory paging of short lists (genres, playlists,
//! favorites).

use super::views::MAX_ITEMS;

/// `IncludeItemTypes` priority: MusicArtist > MusicAlbum > Audio > Playlist >
/// MusicGenre, default album. Type names are
/// case-sensitive.
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

/// The page size for `limit`: `0` means everything from the start, and no
/// page is longer than [`MAX_ITEMS`].
pub(super) fn capped(limit: usize) -> usize {
    if limit == 0 {
        MAX_ITEMS
    } else {
        limit.min(MAX_ITEMS)
    }
}

/// Page a built list: `limit == 0` means everything from `start`, up to
/// [`MAX_ITEMS`]. The total counts the whole list.
pub(super) fn page<T: Clone>(items: &[T], start: usize, limit: usize) -> (Vec<T>, usize) {
    let total = items.len();
    let page = if start >= total {
        Vec::new()
    } else {
        items[start..(start.saturating_add(capped(limit))).min(total)].to_vec()
    };
    (page, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zero limit pages everything from the start, and no page is longer
    /// than the cap; the highest listed type wins, album by default.
    #[test]
    fn query_paging_and_type() {
        let items = [1, 2, 3, 4];
        assert_eq!(page(&items, 1, 2), (vec![2, 3], 4));
        assert_eq!(page(&items, 1, 0), (vec![2, 3, 4], 4));
        assert_eq!(page(&items, 9, 2), (Vec::new(), 4));
        let long = vec![0u8; MAX_ITEMS + 1];
        for limit in [0, MAX_ITEMS + 1, usize::MAX] {
            assert_eq!(page(&long, 0, limit).0.len(), MAX_ITEMS);
        }
        assert_eq!(page(&long, MAX_ITEMS, 0), (vec![0], MAX_ITEMS + 1));

        let types = ["Audio".to_owned(), "MusicArtist".to_owned()];
        assert_eq!(primary_type(&types), "MusicArtist");
        assert_eq!(primary_type(&[]), "MusicAlbum");
    }
}
