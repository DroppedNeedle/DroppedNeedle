//! The statements of the crate's shared database handle.

droppedneedle_kernel::sql! {
    /// One user row, so foreign keys to `auth_users` hold in tests.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) INSERT_TEST_USER = "INSERT OR IGNORE INTO auth_users \
             (id, display_name, role, created_at) \
             VALUES (?1, ?2, ?3, '2024-01-01T00:00:00Z')";
}
