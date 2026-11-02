//! The upgrade's writes to the v2 database, prepared by the schema test
//! against the v2 fixture.

droppedneedle_kernel::sql! {
    schema = V2;

    /// Follows whose user is gone.
    pub(super) ORPHAN_FOLLOWS = "DELETE FROM user_followed_artists \
             WHERE user_id NOT IN (SELECT id FROM auth_users)";
    /// Approvals whose user is gone.
    pub(super) ORPHAN_APPROVALS = "DELETE FROM auto_download_approvals \
             WHERE user_id NOT IN (SELECT id FROM auth_users)";
}
