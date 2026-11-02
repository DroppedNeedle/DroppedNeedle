//! The SQL of the import module.

droppedneedle_kernel::sql! {
    pub(super) PLANNED_TRACK_ID = "SELECT id, local_album_id, availability FROM local_tracks \
                 WHERE root_id = ?1 AND relative_path = ?2";
    pub(super) OLD_COPIES = "SELECT root_id, relative_path FROM local_tracks \
                     WHERE id = ?1 AND availability = 'indexed'";
    pub(super) INDEXED_AMONG = "SELECT 1 FROM local_tracks WHERE id = ?1 AND availability = 'indexed'";
    pub(super) GROUP_HELD = "SELECT EXISTS (SELECT 1 FROM local_tracks t \
         JOIN local_albums b ON b.id = t.local_album_id AND b.retired_into_album_id IS NULL \
         LEFT JOIN local_album_external_identities ai ON ai.local_album_id = b.id \
         WHERE t.availability = 'indexed' \
         AND (lower(ai.release_group_mbid) = ?1 \
         OR (ai.local_album_id IS NULL AND lower(t.embedded_release_group_mbid) = ?1)))";
}
