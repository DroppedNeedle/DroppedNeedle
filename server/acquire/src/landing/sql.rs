//! What the landing reads of the library: the tracks a release group
//! already has, one track a download would replace, and the files a
//! finished import placed.

// `sql!` joins one token per part, so a part built by a definition macro
// is written in parentheses.
#![allow(unused_parens, clippy::double_parens)]

use droppedneedle_library::{names_another_group, not_yet_identified, owns_group, recording_mbid};

droppedneedle_kernel::sql! {
    /// Indexed library tracks of the albums that own the group (`?1`,
    /// lowered), with their album's identity and their own: the one notion
    /// of "the library holds it" the landing and the wanted watcher share.
    pub(crate) fragment OWNED_TRACKS_FROM = "FROM local_tracks t \
         JOIN local_albums b ON b.id = t.local_album_id \
         LEFT JOIN local_album_external_identities ai \
           ON ai.local_album_id = b.id AND ai.provider = 'musicbrainz' \
         LEFT JOIN local_track_external_identities ti \
           ON ti.local_track_id = t.id AND ti.provider = 'musicbrainz' \
         WHERE +t.availability = 'indexed' AND t.local_album_id IN (SELECT album_id FROM ("
         + (owns_group!("= ?1")) + "))";
    /// [`OWNED_TRACKS_FROM`] kept to one library album when `?2` names one,
    /// followed to the album it was merged into. An album since deleted
    /// keeps nothing: the whole group is read.
    fragment IN_ALBUM = " AND (?2 IS NULL \
         OR NOT EXISTS (SELECT 1 FROM local_albums WHERE id = ?2) \
         OR b.id = (SELECT COALESCE(retired_into_album_id, id) \
         FROM local_albums WHERE id = ?2))";
    /// Indexed tracks of one release group: release track and recording
    /// (sealed identity first, then the file's own tags) and quality facts.
    pub(super) GROUP_TRACKS = "SELECT \
         lower(COALESCE(ti.release_track_mbid, t.embedded_release_track_mbid, '')), \
         lower(COALESCE(" + (recording_mbid!("ti", "t")) + ", '')), \
         t.file_format, t.bit_rate, t.bit_depth " + OWNED_TRACKS_FROM + IN_ALBUM;
    /// Every indexed copy in a group, with the album it sits in, that
    /// album's release (sealed identity first, then the file's own tags)
    /// and the release its identity names, the copy's release track and
    /// recording, and its quality facts, in the order `GroupCopy::from_row`
    /// reads them.
    pub(super) GROUP_COPIES = "SELECT " + GROUP_COPY_COLUMNS + " " + OWNED_TRACKS_FROM + IN_ALBUM;
    /// What [`GROUP_COPIES`] reads.
    fragment GROUP_COPY_COLUMNS = "t.id, b.id, \
         lower(COALESCE(ai.release_mbid, t.embedded_release_mbid, '')), \
         COALESCE(ai.release_mbid, ''), \
         lower(COALESCE(ti.release_track_mbid, t.embedded_release_track_mbid, '')), \
         lower(COALESCE(" + (recording_mbid!("ti", "t")) + ", '')), \
         t.file_format, t.bit_rate, t.bit_depth";
    /// Files naming group `?1` on an album identification has not reached
    /// whose other files name another group. Such an album owns nothing, and
    /// these copies are never replaced by themselves.
    pub(super) MIXED_TAG_COPIES = "SELECT " + GROUP_COPY_COLUMNS + " \
         FROM local_tracks t \
         JOIN local_albums b ON b.id = t.local_album_id AND +b.retired_into_album_id IS NULL \
         LEFT JOIN local_album_external_identities ai \
           ON ai.local_album_id = b.id AND ai.provider = 'musicbrainz' \
         LEFT JOIN local_track_external_identities ti \
           ON ti.local_track_id = t.id AND ti.provider = 'musicbrainz' \
         WHERE t.embedded_release_group_mbid IS NOT NULL \
         AND lower(t.embedded_release_group_mbid) = ?1 AND +t.availability = 'indexed' AND "
         + (not_yet_identified!("b")) + " AND " + (names_another_group!("t"));
    /// One track (`?1`) a download would replace: its availability and
    /// quality, whether it sits on the planned album (`?2`) identified as
    /// the planned release (`?3`), whether it is an alias, whether its
    /// identity names one of the release's tracks (`?4`, a JSON list of
    /// lowered ids), and its album.
    pub(super) VERSION_ROW = "SELECT t.availability, t.file_format, t.bit_rate, t.bit_depth, \
         t.local_album_id = (SELECT COALESCE(retired_into_album_id, id) \
           FROM local_albums WHERE id = ?2) \
         AND EXISTS (SELECT 1 FROM local_album_external_identities e \
           WHERE e.local_album_id = t.local_album_id AND e.provider = 'musicbrainz' \
           AND lower(trim(e.release_mbid)) = lower(trim(?3))), \
         EXISTS (SELECT 1 FROM local_track_aliases x WHERE x.alias = t.id), \
         EXISTS (SELECT 1 FROM local_track_external_identities i, json_each(?4) j \
           WHERE i.local_track_id = t.id AND i.provider = 'musicbrainz' \
           AND lower(i.release_track_mbid) = j.value), \
         t.local_album_id FROM local_tracks t WHERE t.id = ?1";
    /// The source hashes of the files an import staged under one folder
    /// (`?1`) placed.
    pub(super) PLACED_SOURCES = "SELECT DISTINCT source_sha256 FROM library_publish_journal \
         WHERE substr(source_rel, 1, length(?1)) = ?1 AND source_sha256 IS NOT NULL \
           AND state IN ('committed', 'cleanup_pending', 'cleaned')";
}
