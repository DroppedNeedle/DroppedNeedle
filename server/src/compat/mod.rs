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
//!   stays in each protocol layer (byte-identical rules); leases run under
//!   fixed `compat:subsonic` / `compat:jellyfin` principals until the seams
//!   carry a caller.
//! - Library: the in-memory [`MemoryStore`](adapters::empty::MemoryStore)
//!   (mutations round-trip, catalog reads empty) until the compat store is
//!   joined to the v3 catalog; Jellyfin reads use the empty
//!   [`MemoryLibrary`](jellyfin::seams::MemoryLibrary).
//!
//! # Golden format (one spelling)
//!
//! There is no on-disk byte corpus and no corpus runner: every golden is
//! inline in a test target, in exactly two shapes.
//!
//! - Journey traces + pinned references (`tests/it/compat_journeys.rs`,
//!   fixtures in `tests/fixtures/compat/`): multi-step `*.trace.json`
//!   lifecycles (request, status, header sidecar, body per step) plus
//!   single-response `pinned_*.json` shapes, compared by `diff_golden`
//!   under the exact / `re:` / `ignore` vocabulary (`re:`-prefixed
//!   string leaves are whole-leaf regexes for volatile ids and dates;
//!   `ignore` lists skip paths). `COMPAT_BLESS=1` regenerates both;
//!   re-add `re:` markers afterwards.
//! - Protocol suites (`tests/it/compat_subsonic.rs`, `tests/it/compat_jellyfin.rs`):
//!   dispatches against fixture seams, asserting status, content type,
//!   headers, and body per test. Subsonic asserts on the `Rendered`
//!   struct (exact bytes for key rows, field asserts elsewhere);
//!   Jellyfin asserts through the inline `Golden` harness (status +
//!   header sidecar + `BodyExp::Exact` wire bytes or parsed-JSON
//!   comparison with `__UUID__`/`__ISO_PY__`/`__ISO_O__`/`__ANY__`
//!   placeholders and exact key sets).

pub mod adapters;
pub mod http;
pub mod jellyfin;
pub mod setup;
pub mod shared;
pub mod subsonic;

pub use setup::CompatSetup;
