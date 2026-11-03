//! The statements of the backup checks, which read a backup file
//! read-only.

droppedneedle_kernel::sql! {
    /// Rows that break a foreign key.
    pub(super) FOREIGN_KEY_VIOLATIONS = "SELECT COUNT(*) FROM pragma_foreign_key_check";
}
