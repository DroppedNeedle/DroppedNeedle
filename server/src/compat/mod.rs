//! Third-party protocol shims: Subsonic/OpenSubsonic and Jellyfin.
//!
//! Both routers mount outside the `/api` session gate with their own
//! app-password auth ([`crate::auth::compat_auth`]), the stream engine for
//! bytes, and the shared edge policy (kill switches default off, CORS `*` creds
//! off, case-insensitive paths, rate limits plus auth backoff, log
//! redaction).
//!
//! # Seams
//!
//! - Auth: [`ProdCompatPasswords`](crate::auth::compat_auth::prod::ProdCompatPasswords)
//!   over the real users tables (app passwords only, never native tokens
//!   or account passwords). The Subsonic [`Verifier`](subsonic::Verifier)
//!   maps classified credentials back onto `compat_auth`'s `authenticate`
//!   so the contract runs exactly once; Jellyfin handlers call
//!   `resolve_token`/`authenticate_by_name` directly.
//! - Streaming: [`GatewayAudio`](adapters::engines::GatewayAudio) and
//!   [`GatewayStream`](adapters::engines::GatewayStream) over the
//!   [`StreamEngine`](crate::stream::routes::StreamEngine). Range slicing
//!   stays in each protocol layer (byte-identical rules); leases count
//!   against the authenticated caller.
//! - Library: [`CompatLibrary`](adapters::library::CompatLibrary) over the
//!   v3 catalog and the shared collections, so playlists, favorites, queues
//!   and bookmarks are the same rows the web UI uses.
//!   [`SubsonicStore`](adapters::subsonic_store::SubsonicStore) and
//!   [`JellyfinLibrary`](adapters::jellyfin_library::JellyfinLibrary) shape
//!   it for each protocol.
//! - Settings: [`LiveSettings`](settings::LiveSettings), read per request.
//!
//! # Tests
//!
//! Wire goldens live with the tests: `tests/it/compat_journeys.rs` replays
//! the client traces and pinned shapes in `tests/fixtures/compat/`
//! (`COMPAT_BLESS=1` rewrites them), `tests/it/compat_subsonic.rs` and
//! `tests/it/compat_jellyfin.rs` pin each protocol over the test doubles,
//! and `tests/it/compat_catalog.rs` runs both over a real database.

pub mod adapters;
pub mod http;
pub mod jellyfin;
pub mod settings;
pub mod setup;
pub mod shared;
pub mod subsonic;

pub use setup::CompatSetup;
