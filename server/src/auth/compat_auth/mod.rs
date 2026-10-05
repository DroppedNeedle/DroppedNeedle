//! Compat-auth contracts: byte-locked Subsonic and Jellyfin auth.
//!
//! The compat routers live in [`crate::compat`]; the auth contracts live
//! here as a tested module: Subsonic codes 10/40/43/44/50/70 with the binary-vs-
//! envelope split (including the getAvatar 403-as-text exception),
//! Jellyfin 401s with the login echo, and account-password rejection on
//! both protocols (app passwords only on compat paths, never native
//! tokens, never account passwords).
//!
//! Ported from v2's Subsonic and Jellyfin compat auth, errors, routers
//! (`_dispatch`, `_binary_error`, `getAvatar`, `_user_dto`,
//! `_authenticate`), serialization, and the app password service.
//!
//! Store contract: the production
//! [`subsonic::SubsonicPasswordStore`] and
//! [`jellyfin::JellyfinPasswordStore`] read `connect_app_passwords`
//! (decrypting `secret_encrypted` for the Subsonic token scheme) and
//! `auth_users`; `note_use` throttles `last_used_at` writes to one per
//! ~5 minutes per secret.

#[cfg(any(test, feature = "test-support"))]
pub mod fakes;
pub mod jellyfin;
pub mod prod;
pub mod subsonic;
