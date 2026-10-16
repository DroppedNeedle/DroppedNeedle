//! Integration tests for the Subsonic and Jellyfin compat APIs.

#[path = "../common/mod.rs"]
mod common;

mod auth_compat_goldens;
mod compat_adapters;
mod compat_catalog;
mod compat_jellyfin;
mod compat_journeys;
mod compat_library_journeys;
mod compat_reference;
mod compat_shared;
mod compat_subsonic;
mod compat_wiring;
