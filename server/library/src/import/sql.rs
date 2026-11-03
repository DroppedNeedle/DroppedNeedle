//! The SQL of the import module.

droppedneedle_kernel::sql! {
    pub(super) PLANNED_TRACK_ID = "SELECT id, local_album_id, availability FROM local_tracks \
                 WHERE root_id = ?1 AND relative_path = ?2";
    pub(super) OLD_COPIES = "SELECT root_id, relative_path FROM local_tracks \
                     WHERE id = ?1 AND availability = 'indexed'";
    pub(super) INDEXED_AMONG = "SELECT 1 FROM local_tracks WHERE id = ?1 AND availability = 'indexed'";
}
