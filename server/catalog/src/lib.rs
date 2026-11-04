//! The catalog: library, search, artist and album pages, discover,
//! collections, artwork, YouTube links, lyrics, upcoming concerts, the
//! version check and wrapped. [`wiring`] builds them into one bundle.

// The local catalog's one `sql!` block expands one level per statement.
#![recursion_limit = "256"]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod artwork;
pub mod cache_hold;
pub mod collections;
pub mod concerts;
pub mod discover;
pub mod enrichment;
pub mod local;
pub mod lyrics;
pub mod metadata;
pub mod search;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod version;
pub mod wiring;
pub mod wrapped;
pub mod youtube;
