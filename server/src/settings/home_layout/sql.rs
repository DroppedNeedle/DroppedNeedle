//! The statements of `app_home_layouts`.

droppedneedle_kernel::sql! {
    /// User `?1`'s home layout, when they saved one.
    pub(super) LAYOUT = "SELECT version, widgets, hidden FROM app_home_layouts \
        WHERE user_id = ?1";
    /// Save user `?1`'s layout at `?5`.
    pub(super) PUT_LAYOUT = "INSERT INTO app_home_layouts \
        (user_id, version, widgets, hidden, updated_at) VALUES (?1, ?2, ?3, ?4, ?5) \
        ON CONFLICT (user_id) DO UPDATE SET version = excluded.version, \
        widgets = excluded.widgets, hidden = excluded.hidden, updated_at = excluded.updated_at";
    /// The first admin's id: force mode serves that account's layout.
    pub(super) FIRST_ADMIN = "SELECT id FROM auth_users WHERE role = 'admin' \
        ORDER BY created_at, id LIMIT 1";

    /// One stored layout.
    pub(super) row LayoutRow for [LAYOUT] {
        version: i64,
        widgets: String,
        hidden: String,
    }
}
