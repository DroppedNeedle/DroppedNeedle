//! The statements of `compat_user_settings`.

droppedneedle_kernel::sql! {
    /// One person's choice; no row means off.
    pub(super) INCLUDE_PUBLIC_PLAYLISTS = "SELECT include_public_playlists \
        FROM compat_user_settings WHERE user_id = ?1";
    /// Save one person's choice.
    pub(super) SET_INCLUDE_PUBLIC_PLAYLISTS = "INSERT INTO compat_user_settings \
        (user_id, include_public_playlists) VALUES (?1, ?2) \
        ON CONFLICT (user_id) DO UPDATE SET \
        include_public_playlists = excluded.include_public_playlists";
}
