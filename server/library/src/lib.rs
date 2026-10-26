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
//! [`adapters`] implements the seams between them (tag reads,
//! identify enqueue, tag staging, contribution ports) and [`wiring`]
//! owns the [`wiring::LibrarySetup`] bundle `create_app` mounts:
//! stores, services, HTTP routes, background loops, and startup
//! recovery. HTTP lives under [`http`].
//!
//! Store durability: roots, scan runs, the catalog, identification,
//! contributions and the publish journal (with its snapshots and
//! baselines) all live in the application database.

// The identity module's one `sql!` block expands one level per statement.
#![recursion_limit = "256"]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod activity;
pub mod adapters;
mod catalog_revision;
mod clock;
pub mod contrib;
pub mod corrections;
pub mod edition_prefs;
pub mod http;
pub mod identify;
pub mod identity;
pub mod import;
pub mod jobs;
mod loops;
pub use loops::{LEFTOVERS_JOB, LOOPS};
pub mod manage;
pub mod management;
pub mod matching;
pub mod mutations;
pub mod operations;
pub mod policy;
pub mod publish;
pub mod reconcile;
pub mod recycle_bin;
pub mod repairs;
pub mod scan;
pub mod scans;
pub mod service;
pub mod settings;
pub mod settings_models;
pub mod tags;
pub mod wiring;

/// Every `sql.rs` of this crate, one line each, for the schema test.
#[cfg(any(test, feature = "test-support"))]
pub fn statement_files() -> Vec<droppedneedle_kernel::db::statement::StatementFile> {
    vec![
        scan::sql::registered(),
        identity::sql::registered(),
        contrib::sql::registered(),
        publish::sql::registered(),
        catalog_revision::sql::registered(),
        jobs::sql::registered(),
        management::sql::registered(),
    ]
}
