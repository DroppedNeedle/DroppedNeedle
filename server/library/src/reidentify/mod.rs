//! Identifying an album again on a person's request, and the scored
//! candidates the matcher ranks against an album's files.

pub mod candidates;
pub mod models;
pub mod service;
pub(crate) mod sql;
pub mod store;
