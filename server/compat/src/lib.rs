//! Third-party protocol shims: Subsonic/OpenSubsonic and Jellyfin.
//!
//! Both routers mount outside the `/api` session gate with their own
//! app-password auth ([`droppedneedle_auth::compat_auth`]), the stream engine for
//! bytes, and the shared edge policy (kill switches default off, CORS `*` creds
//! off, case-insensitive paths, rate limits plus auth backoff, log
//! redaction).
//!
//! # What the handlers call
//!
//! - Auth: [`CompatPasswords`](droppedneedle_auth::compat_auth::prod::CompatPasswords)
//!   over the real users tables (app passwords only, never native tokens
//!   or account passwords). The Subsonic
//!   [`SubsonicVerifier`](subsonic::principal::SubsonicVerifier) maps
//!   classified credentials back onto `compat_auth`'s `authenticate` so
//!   the contract runs exactly once; Jellyfin handlers call
//!   `resolve_token`/`authenticate_by_name` directly.
//! - Streaming: [`GatewayAudio`](audio::GatewayAudio), for both
//!   protocols, over the
//!   [`StreamEngine`](droppedneedle_media::stream::engine::StreamEngine), which
//!   makes the one transcode decision; ranges frame through
//!   [`framing`](droppedneedle_media::stream::framing), and leases count
//!   against the authenticated caller.
//! - Library: [`CompatLibrary`](library::CompatLibrary) over the
//!   v3 catalog and the shared collections, so playlists, favorites, queues
//!   and bookmarks are the same rows the web UI uses.
//!   [`SubsonicStore`](subsonic::store::SubsonicStore) and
//!   [`JellyfinLibrary`](jellyfin::library::JellyfinLibrary) shape
//!   it for each protocol.
//! - Jellyfin ids: [`CatalogIds`](jellyfin::library::CatalogIds)
//!   over the stored table ([`ids`](jellyfin::ids)),
//!   which compat's one loop ([`LOOPS`]) fills.
//! - Settings: [`LiveSettings`](settings::LiveSettings), read per request.
//!
//! # What is where
//!
//! - Both protocols: [`library`], [`audio`], [`playback`], [`queues`] (with
//!   their statements in [`sql`]), [`user_settings`], [`settings`], and the
//!   shared edge in [`http`], [`shared`] and [`body`].
//! - [`subsonic`]: the handlers, wire models and views, the production
//!   [`store`](subsonic::store), the [`records`](subsonic::records) it trades
//!   with the handlers, and the [`principal`](subsonic::principal).
//! - [`jellyfin`]: the handlers, wire models and builders, the catalog-backed
//!   [`library`](jellyfin::library), the stored [`ids`](jellyfin::ids), the
//!   [`views`](jellyfin::views) and the [`settings`](jellyfin::settings).
//!
//! # Tests
//!
//! Wire goldens live with the tests, in `server/tests/fixtures/compat/`.
//! `tests/compat/compat_library_journeys.rs` replays the client traces and
//! pinned shapes through the whole app over a library its own scanner
//! filled (the `library_*` goldens). `COMPAT_BLESS=1` rewrites the goldens
//! and is refused when `CI` is set. `tests/compat/compat_subsonic_library.rs`,
//! `tests/compat/compat_jellyfin_library.rs` and
//! `tests/compat/compat_catalog.rs` pin each protocol on that library. The
//! test app is given no ffmpeg unless a test asks for the committed
//! stand-in (`server/testkit/ffmpeg/ffmpeg`), which checks the transcode
//! command and answers one fixed line; the answers with transcoding on run
//! through it.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod audio;
pub mod body;
pub mod http;
pub mod jellyfin;
pub mod library;
pub mod playback;
pub mod queues;
pub mod settings;
pub mod setup;
pub mod shared;
pub mod sql;
pub mod subsonic;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod user_settings;

pub use setup::CompatSetup;

/// Every always-on compat loop, by name.
pub const LOOPS: &[&str] = &[jellyfin::ids::JOB_NAME];
