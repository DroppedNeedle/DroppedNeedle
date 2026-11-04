//! Library engine: scan, tags, identify, publish, contrib.
//!
//! Five modules, one engine. `scan` owns discovery (roots, walk,
//! scheduling, supervision) and never writes music files; `tags`
//! owns reads, probes, fingerprints, and the save wrapper; `identify`
//! owns provider identification plus the curator review queue;
//! `publish` owns every managed-file write through the staged
//! publisher; `contrib` owns MusicBrainz contributions plus the
//! verification worker.
//!
//! Beside them sit what people ask of the catalog: `edition` (choosing an
//! album's edition), `reidentify`, `reviews`, `removal`, `corrections` and
//! `repairs`. `jobs` runs the long work an administrator can watch and
//! stop, `catalog_actions` keeps the audit rows and the history read from
//! them, and `reasons` holds the sentences a refusal answers with.
//!
//! [`adapters`] implements the seams between them (tag reads,
//! identify enqueue, tag staging, contribution ports) and [`wiring`]
//! owns the [`wiring::LibrarySetup`] bundle `create_app` mounts:
//! stores, services, HTTP routes, background loops, and startup
//! recovery. HTTP lives under [`http`].
//!
//! Store durability: roots, scan runs, the catalog, identification,
//! contributions and the publish journal (with its snapshots and
//! baselines) all live in the application database.

// A `sql!` block expands one level per statement.
#![recursion_limit = "4096"]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod activity;
pub mod adapters;
pub mod catalog_actions;
mod catalog_revision;
mod clock;
pub mod contrib;
pub mod corrections;
pub mod edition;
pub mod edition_prefs;
pub mod http;
pub mod identify;
pub mod identity;
pub mod import;
pub mod jobs;
mod loops;
mod sql;
pub use loops::{LEFTOVERS_JOB, LOOPS};
pub mod management;
pub mod matching;
pub mod policy;
pub mod preview;
pub mod publish;
pub mod reasons;
pub mod reconcile;
pub mod reidentify;
pub mod removal;
pub mod repairs;
pub mod reviews;
pub mod scan;
pub mod scans;
pub mod service;
pub mod settings;
pub mod settings_models;
pub mod tags;
pub mod wiring;

#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
