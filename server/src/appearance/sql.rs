//! The statements of `app_user_appearances` and `app_custom_css_saves`.

use super::ThemeChoice;
use droppedneedle_kernel::db::time::Millis;

droppedneedle_kernel::sql! {
    /// User `?1`'s appearance, when they saved one.
    pub(super) APPEARANCE = "SELECT theme, custom_css, custom_css_enabled, css_saved_at \
        FROM app_user_appearances WHERE user_id = ?1";
    /// Save user `?1`'s appearance at `?6`.
    pub(super) PUT_APPEARANCE = "INSERT INTO app_user_appearances \
        (user_id, theme, custom_css, custom_css_enabled, css_saved_at, updated_at) \
        VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
        ON CONFLICT (user_id) DO UPDATE SET theme = excluded.theme, \
        custom_css = excluded.custom_css, custom_css_enabled = excluded.custom_css_enabled, \
        css_saved_at = excluded.css_saved_at, updated_at = excluded.updated_at";
    /// Switch user `?1`'s CSS off at `?2`, keeping its text.
    pub(super) TURN_OFF_CSS = "UPDATE app_user_appearances \
        SET custom_css_enabled = 0, updated_at = ?2 WHERE user_id = ?1";
    /// Keep CSS `?2` as one of user `?1`'s saves, made at `?3`.
    pub(super) ADD_CSS_SAVE = "INSERT INTO app_custom_css_saves (user_id, css, saved_at) \
        VALUES (?1, ?2, ?3)";
    /// Forget all but user `?1`'s newest `?2` saves.
    pub(super) TRIM_CSS_SAVES = "DELETE FROM app_custom_css_saves WHERE user_id = ?1 \
        AND id NOT IN (SELECT id FROM app_custom_css_saves WHERE user_id = ?1 \
        ORDER BY saved_at DESC, id DESC LIMIT ?2)";
    /// User `?1`'s saves, newest first.
    pub(super) CSS_SAVES = "SELECT id, css, saved_at FROM app_custom_css_saves \
        WHERE user_id = ?1 ORDER BY saved_at DESC, id DESC";

    /// One person's stored appearance.
    pub(super) row AppearanceRow for [APPEARANCE] {
        theme: Option<ThemeChoice>,
        custom_css: String,
        custom_css_enabled: bool,
        css_saved_at: Option<Millis>,
    }
    /// One kept save of a person's CSS.
    pub(super) row CssSaveRow for [CSS_SAVES] {
        id: i64,
        css: String,
        saved_at: Millis,
    }
}
