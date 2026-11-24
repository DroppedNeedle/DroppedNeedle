//! The statements of `app_home_layouts`.

droppedneedle_kernel::sql! {
    /// User `?1`'s home layout, when they saved one.
    pub(super) LAYOUT = "SELECT version, widgets, hidden, updated_at FROM app_home_layouts \
        WHERE user_id = ?1";
    /// Save user `?1`'s layout. A missing row is inserted only when the
    /// expected revision is 0; an existing row updates only when its
    /// `updated_at` equals the expected revision `?6`.
    pub(super) PUT_LAYOUT = "INSERT INTO app_home_layouts \
        (user_id, version, widgets, hidden, updated_at) \
        SELECT ?1, ?2, ?3, ?4, ?5 \
        WHERE NOT EXISTS (SELECT 1 FROM app_home_layouts WHERE user_id = ?1) \
          AND ?6 = 0 \
        ON CONFLICT (user_id) DO UPDATE SET version = excluded.version, \
        widgets = excluded.widgets, hidden = excluded.hidden, \
        updated_at = excluded.updated_at \
        WHERE app_home_layouts.updated_at = ?6";
    /// The first admin's id and layout in one row: force mode serves that
    /// account's layout. A LEFT JOIN keeps the id even with no saved row.
    pub(super) FIRST_ADMIN_LAYOUT = "SELECT u.id, l.version, l.widgets, l.hidden, l.updated_at \
        FROM auth_users u LEFT JOIN app_home_layouts l ON l.user_id = u.id \
        WHERE u.role = 'admin' ORDER BY u.created_at, u.id LIMIT 1";

    /// One stored layout.
    pub(super) row LayoutRow for [LAYOUT] {
        version: i64,
        widgets: String,
        hidden: String,
        updated_at: i64,
    }
}
