//! The statements the event streams follow: the library's revisions and
//! the global download activity revision.

crate::sql! {
    /// Every library stream's revision.
    pub(super) STREAM_REVISIONS = "SELECT stream_kind, value \
        FROM library_event_stream_revisions";
    /// The catalog revision.
    pub(super) CATALOG_REVISION = "SELECT value FROM library_catalog_revision \
        WHERE singleton = 1";
    /// The global download activity revision.
    pub(super) DOWNLOAD_REVISION = "SELECT revision FROM download_activity_global_revision \
        WHERE singleton = 1";

    /// One library stream's revision.
    pub(super) row StreamRevision for [STREAM_REVISIONS] {
        stream_kind: String,
        value: i64,
    }
}
