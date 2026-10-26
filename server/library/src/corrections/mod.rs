//! Catalog corrections: an administrator fixes how files are grouped into
//! albums, or folds duplicate artists together.
//!
//! - split: selected tracks of an album become a new album;
//! - merge: whole albums fold into another album;
//! - move: selected tracks join another album;
//! - reset grouping: tracks grouped by hand go back to automatic grouping;
//! - artist merge: duplicate artists fold into one survivor;
//! - artist convergence: at the end of a scan run, artist rows MusicBrainz
//!   proves to be one artist fold into one, with no one asking.
//!
//! Each is previewed first and applied with the preview's token. They
//! change catalog rows only, for the albums and artists involved: no file
//! is moved, renamed or retagged. Manual grouping is locked, so later
//! scans keep it. Album editions follow the rules in [`edition`]. As in
//! v2 these are not undoable; a reset undoes a split, merge or move.
//!
//! Layout: `models` holds the types, `reasons` the refusal and outcome
//! sentences, `token` the preview tokens, `membership` split, merge and
//! move, `reset` reset grouping, `edition` what happens to editions,
//! `artists` the artist merge, `artist_convergence` the automatic one,
//! `merge_basis` what each artist merge rests on,
//! `retag_merge` the join of an album
//! retagged across two scans, `retag_pairs` the pairs a person joins or
//! dismisses, `release_join` the join of an album whose files another
//! album replaced on the same edition, `placement` a scanned file placed
//! on its album's edition by its own ids, `continuation` a missing track
//! continuing into the track that took its place, and `service` the entry
//! points.

pub(crate) mod artist_convergence;
mod artists;
pub mod continuation;
mod edition;
pub(crate) mod membership;
mod merge_basis;
pub mod models;
pub(crate) mod placement;
pub mod reasons;
pub(crate) mod release_join;
mod reset;
pub(crate) mod retag_merge;
mod retag_pairs;
pub mod service;
mod token;

pub use service::Corrections;
