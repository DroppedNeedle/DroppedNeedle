//! SQLite stores behind the collections service. Reads use the reader pool,
//! writes the writer lane; every store is a cheap clone over one
//! [`CollectionsDb`](super::db::CollectionsDb).

pub mod favorites;
pub mod follows;
pub mod play_marks;
pub mod playlists;
pub mod ratings;

pub use favorites::FavoriteStore;
pub use follows::FollowStore;
pub use play_marks::PlayMarkStore;
pub use playlists::PlaylistStore;
pub use ratings::RatingStore;

use super::db::CollectionsDb;

/// Every collections store over one database.
#[derive(Clone, Debug)]
pub struct Stores {
    /// Playlists, entries and covers.
    pub playlists: PlaylistStore,
    /// Favorites.
    pub favorites: FavoriteStore,
    /// Follows and the new-release feed.
    pub follows: FollowStore,
    /// Star ratings.
    pub ratings: RatingStore,
    /// Played and unplayed marks.
    pub play_marks: PlayMarkStore,
}

impl Stores {
    /// All stores over one database handle.
    pub fn new(db: &CollectionsDb) -> Self {
        Self {
            playlists: PlaylistStore::new(db.clone()),
            favorites: FavoriteStore::new(db.clone()),
            follows: FollowStore::new(db.clone()),
            ratings: RatingStore::new(db.clone()),
            play_marks: PlayMarkStore::new(db.clone()),
        }
    }
}
