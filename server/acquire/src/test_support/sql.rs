//! Statements only tests run.

droppedneedle_kernel::sql! {
    /// One user row, so foreign keys to `auth_users` hold in tests.
    pub(crate) INSERT_TEST_USER = "INSERT OR IGNORE INTO auth_users \
             (id, display_name, role, created_at) \
             VALUES (?1, ?2, ?3, 0)";
}
