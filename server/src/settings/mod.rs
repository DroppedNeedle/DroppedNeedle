//! Settings: the `/api/v3/settings` surface. Sections are served as their
//! `runtime_config` types (secret sections masked); `models` holds only
//! the wire shapes that are not sections. The library policy and Library
//! Management logic these routes call live in `library`.

pub mod effects;
pub mod handlers;
pub mod library_catalog;
pub mod library_policy_service;
pub mod models;
pub mod musicbrainz;
pub mod quality;
pub mod registry;
pub mod section_prefs;
pub mod services;
pub mod validator;
pub mod verify;
pub mod wiring;
