//! Artist reconciliation: duplicate artist records an administrator can
//! review, and the progress of the reconciliation pass.
//!
//! Two current artist records with the same folded name are a duplicate
//! group when both still credit indexed music. [`groups`] decides what kind
//! of group it is from the MusicBrainz evidence the library holds (two
//! different artist IDs, an ambiguous credit, proof still missing, or just
//! the same name). Merges that already happened automatically show up as
//! resolved groups, read back from the catalog action log.
//!
//! An administrator can mark a group as distinct people. That records one
//! dismissal per pair at the members' current revisions, so the group comes
//! back if any member changes or a new same-name record appears.
//!
//! Layout: `models` holds the types, `store` the SQL, `groups` the pure
//! grouping rules, and `service` the entry points the HTTP handlers call.

pub mod groups;
pub mod models;
pub mod reasons;
pub mod service;
pub mod store;
