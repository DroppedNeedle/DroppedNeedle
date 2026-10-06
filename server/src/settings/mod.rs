//! Settings: the `/api/v3/settings` surface. Sections are served as their
//! `runtime_config` types (secret sections masked); `models` holds only
//! the wire shapes that are not sections; `management` is Library
//! Management.

pub mod effects;
pub mod error;
pub mod handlers;
pub mod library_catalog;
pub mod library_policy;
pub mod library_policy_service;
pub mod management;
pub mod models;
pub mod musicbrainz;
pub mod quality;
pub mod section_prefs;
pub mod services;
pub mod validator;
pub mod verify;
pub mod wiring;
