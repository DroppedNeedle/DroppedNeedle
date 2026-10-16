//! SQLite stores behind the collections service. Reads use the reader pool,
//! writes the writer lane; every store is a cheap clone over one
//! [`CollectionsDb`](super::db::CollectionsDb).

pub mod favorites;
pub mod follows;
pub mod pins;
pub mod playlists;

pub use favorites::FavoriteStore;
pub use follows::FollowStore;
pub use pins::PinStore;
pub use playlists::PlaylistStore;

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
    /// Edition pins.
    pub pins: PinStore,
}

impl Stores {
    /// All stores over one database handle.
    pub fn new(db: &CollectionsDb) -> Self {
        Self {
            playlists: PlaylistStore::new(db.clone()),
            favorites: FavoriteStore::new(db.clone()),
            follows: FollowStore::new(db.clone()),
            pins: PinStore::new(db.clone()),
        }
    }
}
