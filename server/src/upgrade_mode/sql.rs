//! Upgrade mode's one read of the v2 database, prepared by the schema test
//! against the v2 fixture.

droppedneedle_kernel::sql! {
    schema = V2;

    /// Every admin's local sign-in name and stored password hash.
    pub(super) V2_ADMIN_PASSWORDS = "SELECT p.provider_uid, \
             json_extract(p.provider_data, '$.password_hash') AS password_hash \
             FROM auth_providers p JOIN auth_users u ON u.id = p.user_id \
             WHERE u.role = 'admin' AND p.provider = 'local'";

    /// One admin's local password, as v2 stored it.
    pub(super) row V2AdminPassword for [V2_ADMIN_PASSWORDS] {
        provider_uid: String,
        password_hash: Option<String>,
    }
}
