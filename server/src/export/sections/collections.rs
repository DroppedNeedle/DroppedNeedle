//! Playlists, their tracks and covers, and favorites with their names.
//!
//! v2 keeps these in its native `library_*` tables; v3 keeps playlists in
//! `playlists`/`playlist_tracks` and favorites in `library_user_favorites`.
//! Playlist entries and favorites point at v2 library ids, written as is
//! (v3 has no foreign key there) and recorded as pending links.

use super::{Column, FileSet, LinkKind, LinkMode, Parent, Source, TableSection, Target, col};

/// Playlists whose owner still exists (or that predate ownership).
const LIVE_PLAYLIST: &str = "s.playlist_id IN (SELECT p.id FROM v2.library_playlists p \
     WHERE p.user_id IS NULL OR p.user_id IN (SELECT id FROM v2.auth_users))";

/// Entries and covers hang off a playlist that landed in v3.
const ON_PLAYLIST: &[Parent] = &[Parent::required("playlists", &[("playlist_id", "id")])];

/// Source types whose `track_source_id` is a v2 library id.
const LOCAL_ENTRY: &str = "b.\"source_type\" IN ('local', 'droppedneedle-local', 'howler')";

/// Playlists. The v2 cover path stays behind: v3 keeps the cover bytes in
/// `playlist_covers` instead. A user holds one playlist per imported
/// source; a second v2 playlist with the same source counts as a conflict.
pub const PLAYLISTS: TableSection = TableSection {
    name: "playlist",
    source: Source::Table {
        table: "library_playlists",
        filter: "",
        requires: &[],
    },
    target: Target::Table("playlists"),
    columns: &[
        col("id"),
        col("name"),
        col("created_at"),
        col("updated_at"),
        col("source_ref"),
        col("user_id"),
        col("is_public"),
    ],
    key: &["id"],
    unique: &[&["user_id", "source_ref"]],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "playlists of deleted users",
};

/// Playlist entries. v3 reads the local track id from `library_file_id`.
/// v2's own `library_file_id` (an older file id on entries v2 could not
/// match) and the entry's album, artist and tombstone ids travel as
/// pending links only.
pub const PLAYLIST_TRACKS: TableSection = TableSection {
    name: "playlist_track",
    source: Source::Table {
        table: "library_playlist_tracks",
        filter: LIVE_PLAYLIST,
        requires: &[],
    },
    target: Target::Table("playlist_tracks"),
    columns: &[
        col("id"),
        col("playlist_id"),
        col("position"),
        col("track_name"),
        col("artist_name"),
        col("album_name"),
        col("album_id"),
        col("artist_id"),
        col("track_source_id").link_when(LinkKind::Track, LinkMode::AsWritten, LOCAL_ENTRY),
        col("cover_url"),
        col("source_type"),
        col("available_sources"),
        col("format"),
        col("track_number"),
        col("disc_number"),
        col("duration"),
        col("created_at"),
        col("plex_rating_key"),
        Column::new("library_file_id")
            .from_v2("local_track_id")
            .link(LinkKind::Track, LinkMode::AsWritten),
        Column::new("v2_library_file_id")
            .from_v2("library_file_id")
            .link(LinkKind::Track, LinkMode::LinkOnly),
        col("local_album_id").link(LinkKind::Album, LinkMode::LinkOnly),
        col("local_artist_id").link(LinkKind::Artist, LinkMode::LinkOnly),
        col("reference_tombstone_id").link(LinkKind::Tombstone, LinkMode::LinkOnly),
    ],
    key: &["id"],
    unique: &[&["playlist_id", "position"]],
    parents: ON_PLAYLIST,
    user_column: None,
    left_behind: "entries of playlists that stayed behind",
};

/// Playlist cover images, read from the v2 cover files.
pub const PLAYLIST_COVERS: TableSection = TableSection {
    name: "playlist_cover",
    source: Source::Files(FileSet::PlaylistCovers),
    target: Target::Table("playlist_covers"),
    columns: &[
        col("playlist_id"),
        col("content_type"),
        col("image"),
        col("updated_at"),
    ],
    key: &["playlist_id"],
    unique: &[],
    parents: ON_PLAYLIST,
    user_column: None,
    left_behind: "",
};

/// Favorite artists, albums and tracks.
pub const FAVORITES: TableSection = TableSection {
    name: "favorite",
    source: Source::Table {
        table: "library_user_favorites",
        filter: "",
        requires: &[],
    },
    target: Target::Table("library_user_favorites"),
    columns: &[
        col("user_id"),
        col("item_kind"),
        col("item_id").link(LinkKind::ByColumn("item_kind"), LinkMode::AsWritten),
        col("created_at"),
    ],
    key: &["user_id", "item_kind", "item_id"],
    unique: &[],
    parents: &[],
    user_column: Some("user_id"),
    left_behind: "favorites of deleted users",
};

/// The v2 library name of a favorite's item.
macro_rules! favorite_name_sql {
    () => {
        "CASE s.item_kind \
         WHEN 'track' THEN (SELECT t.title FROM v2.local_tracks t WHERE t.id = s.item_id) \
         WHEN 'album' THEN (SELECT a.title FROM v2.local_albums a WHERE a.id = s.item_id) \
         WHEN 'artist' THEN (SELECT r.display_name FROM v2.local_artists r \
         WHERE r.id = s.item_id) END"
    };
}

/// The name each favorite had in v2's library, so the favorites page can
/// label it while the rescanned library does not hold the item yet.
pub const FAVORITE_NAMES: TableSection = TableSection {
    name: "favorite_name",
    source: Source::Table {
        table: "library_user_favorites",
        filter: concat!("(", favorite_name_sql!(), ") IS NOT NULL"),
        requires: &["local_tracks", "local_albums", "local_artists"],
    },
    target: Target::Table("library_user_favorite_names"),
    columns: &[
        col("user_id"),
        col("item_kind"),
        col("item_id"),
        col("display_name").from_sql(favorite_name_sql!()),
    ],
    key: &["user_id", "item_kind", "item_id"],
    unique: &[],
    parents: &[Parent::required(
        "library_user_favorites",
        &[
            ("user_id", "user_id"),
            ("item_kind", "item_kind"),
            ("item_id", "item_id"),
        ],
    )],
    user_column: Some("user_id"),
    left_behind: "",
};
