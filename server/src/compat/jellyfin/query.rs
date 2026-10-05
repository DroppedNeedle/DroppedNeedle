//! Pure query helpers for the browse routes: item-type priority and
//! in-memory paging of short lists (genres, playlists, favorites).

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paging_treats_zero_limit_as_everything_from_start() {
        let items = [1, 2, 3, 4];
        assert_eq!(page(&items, 1, 2), (vec![2, 3], 4));
        assert_eq!(page(&items, 1, 0), (vec![2, 3, 4], 4));
        assert_eq!(page(&items, 9, 2), (Vec::new(), 4));
    }

    #[test]
    fn item_type_priority() {
        let types = ["Audio".to_owned(), "MusicArtist".to_owned()];
        assert_eq!(primary_type(&types), "MusicArtist");
        assert_eq!(primary_type(&[]), "MusicAlbum");
    }
}
