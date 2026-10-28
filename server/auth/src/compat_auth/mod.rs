//! Compat-auth contracts: byte-locked Subsonic and Jellyfin auth.
//!
//! The compat routers live in the `compat` crate; the auth contracts live
//! here: the three Subsonic schemes and their codes (10/40/43/44), Jellyfin
//! 401s with the login echo, and account-password rejection on both
//! protocols (app passwords only on compat paths, never native tokens,
//! never account passwords). Compat answers a Subsonic denial in its own
//! envelope.
//!
//! Ported from v2's Subsonic and Jellyfin compat auth (`_user_dto`,
//! `_authenticate`) and the app password service.
//!
//! Store: [`prod::ProdCompatPasswords`] reads `connect_app_passwords`
//! (decrypting `secret_encrypted` for the Subsonic token scheme) and
//! `auth_users`; `note_use` throttles `last_used_at` writes to one per
//! ~5 minutes per secret.

pub mod jellyfin;
pub mod prod;
pub mod subsonic;
