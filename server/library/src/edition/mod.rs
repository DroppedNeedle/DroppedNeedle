//! An album's edition: choosing, confirming and undoing it, what a choice
//! would do to the files, matching a file by hand, where the album stands,
//! "Looks wrong" marks, and the edition finder.

pub mod choice;
pub mod decisions;
mod hand_match;
pub(crate) mod looks_wrong;
pub mod models;
pub mod plan;
pub mod port;
pub mod service;
pub(crate) mod sql;
pub mod status;

pub(crate) const PROVIDER: &str = "musicbrainz";
