//! Compat-auth contracts: byte-locked Subsonic and Jellyfin auth.
//!
//! Stage 3 scope, stage0-auth.md D7 plus stage0-compat.md sections 1.2/1.3
//! and 2.1/2.3. Routers land in stage 9; the AUTH CONTRACTS land here as a
//! tested module: Subsonic codes 10/40/43/44/50/70 with the binary-vs-
//! envelope split (including the getAvatar 403-as-text exception),
//! Jellyfin 401s with the login echo, and account-password rejection on
//! both protocols (app passwords only on compat paths, never native
//! tokens, never account passwords).
//!
//! v2 provenance (read-only): `backend/api/compat/subsonic/auth.py`,
//! `backend/api/compat/subsonic/errors.py`,
//! `backend/api/compat/subsonic/router.py` (`_dispatch`, `_binary_error`,
//! `getAvatar`), `backend/api/compat/subsonic/serialization.py`,
//! `backend/api/compat/jellyfin/auth.py`,
//! `backend/api/compat/jellyfin/router.py` (`_user_dto`, `_authenticate`),
//! `backend/api/compat/jellyfin/models.py`,
//! `backend/api/compat/jellyfin/serialization.py`,
//! `backend/services/compat/app_password_service.py`.
//!
//! Assumed sibling/store APIs (flagged for wiring): the production
//! [`subsonic::SubsonicPasswordStore`] and
//! [`jellyfin::JellyfinPasswordStore`] read `connect_app_passwords`
//! (decrypting `secret_encrypted` for the Subsonic token scheme) and
//! `auth_users`; `note_use` throttles `last_used_at` writes to one per
//! ~5 minutes per secret.

pub mod fakes;
pub mod jellyfin;
pub mod prod;
pub mod subsonic;
