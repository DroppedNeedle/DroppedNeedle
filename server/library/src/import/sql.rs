//! The SQL of the import module.

droppedneedle_kernel::sql! {
    pub(super) PLANNED_TRACK_ID = "SELECT id, local_album_id, availability FROM local_tracks \
                 WHERE root_id = ?1 AND relative_path = ?2";
    /// The indexed tracks among the JSON array of ids `?1`, with their files.
    pub(super) INDEXED_COPIES = "SELECT id, root_id, relative_path FROM local_tracks \
                     WHERE id IN (SELECT value FROM json_each(?1)) AND availability = 'indexed'";
    /// One track `INDEXED_COPIES` reads.
    pub(super) row IndexedCopy for [INDEXED_COPIES] {
        id: String,
        root_id: String,
        relative_path: String,
    }
}
