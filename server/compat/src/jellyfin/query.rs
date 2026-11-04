//! Pure query helpers for the browse routes: item-type priority and
//! in-memory paging of short lists (genres, playlists, favorites).

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

/// Page a built list: `limit == 0` means ALL from start.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A zero limit pages everything from the start; the highest listed
    /// type wins, album by default.
    #[test]
    fn query_paging_and_type() {
        let items = [1, 2, 3, 4];
        assert_eq!(page(&items, 1, 2), (vec![2, 3], 4));
        assert_eq!(page(&items, 1, 0), (vec![2, 3, 4], 4));
        assert_eq!(page(&items, 9, 2), (Vec::new(), 4));

        let types = ["Audio".to_owned(), "MusicArtist".to_owned()];
        assert_eq!(primary_type(&types), "MusicArtist");
        assert_eq!(primary_type(&[]), "MusicAlbum");
    }
}
