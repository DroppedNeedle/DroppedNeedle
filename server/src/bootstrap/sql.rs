//! The statements of the stamp probe, which reads an existing database
//! read-only before the runtime opens it.

droppedneedle_kernel::sql! {
    /// Whether any settings section is stored.
    pub(super) ANY_SETTINGS = "SELECT EXISTS(SELECT 1 FROM config_sections)";
    /// Whether any account exists.
    pub(super) ANY_ACCOUNT = "SELECT EXISTS(SELECT 1 FROM auth_users)";
    /// The tables a file holds, leaving out SQLite's own and the
    /// bookkeeping table sqlx creates before the first migration runs.
    pub(super) TABLE_COUNT = "SELECT count(*) FROM sqlite_master WHERE type = 'table' \
        AND name NOT LIKE 'sqlite_%' AND name <> '_sqlx_migrations'";
}
