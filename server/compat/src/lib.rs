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
//! - Auth: [`ProdCompatPasswords`](droppedneedle_auth::compat_auth::prod::ProdCompatPasswords)
//!   over the real users tables (app passwords only, never native tokens
//!   or account passwords). The Subsonic
//!   [`SubsonicVerifier`](adapters::principal::SubsonicVerifier) maps
//!   classified credentials back onto `compat_auth`'s `authenticate` so
//!   the contract runs exactly once; Jellyfin handlers call
//!   `resolve_token`/`authenticate_by_name` directly.
//! - Streaming: [`GatewayAudio`](adapters::audio::GatewayAudio), for both
//!   protocols, over the
//!   [`StreamEngine`](droppedneedle_media::stream::routes::StreamEngine), which
//!   makes the one transcode decision; ranges frame through the stream
//!   routes' framing, and leases count against the authenticated caller.
//! - Library: [`CompatLibrary`](adapters::library::CompatLibrary) over the
//!   v3 catalog and the shared collections, so playlists, favorites, queues
//!   and bookmarks are the same rows the web UI uses.
//!   [`SubsonicStore`](adapters::subsonic_store::SubsonicStore) and
//!   [`JellyfinLibrary`](adapters::jellyfin_library::JellyfinLibrary) shape
//!   it for each protocol.
//! - Jellyfin ids: [`CatalogIds`](adapters::jellyfin_library::CatalogIds)
//!   over the stored table ([`jellyfin_ids`](adapters::jellyfin_ids)),
//!   which compat's one loop ([`LOOPS`]) fills.
//! - Settings: [`LiveSettings`](settings::LiveSettings), read per request.
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

pub mod adapters;
pub mod body;
pub mod http;
pub mod jellyfin;
pub mod settings;
pub mod setup;
pub mod shared;
pub mod subsonic;

pub use setup::CompatSetup;

/// Every always-on compat loop, by name.
pub const LOOPS: &[&str] = &[adapters::jellyfin_ids::JOB_NAME];

/// Every `sql.rs` of this crate, one line each, for the schema test.
#[cfg(any(test, feature = "test-support"))]
pub fn statement_files() -> Vec<droppedneedle_kernel::db::statement::StatementFile> {
    vec![adapters::jellyfin_ids::sql::registered()]
}
