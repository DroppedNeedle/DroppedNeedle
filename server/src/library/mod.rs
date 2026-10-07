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
//! Store durability: roots, scan runs, the catalog, identification, and
//! the publish journal (with its snapshots and baselines) all live in the
//! application database. Contribution state runs on an in-memory store.

pub mod activity;
pub mod adapters;
mod clock;
pub mod contrib;
pub mod http;
pub mod identify;
pub mod import;
mod loops;
pub mod manage;
pub mod matching;
pub mod mutations;
pub mod operations;
pub mod publish;
pub mod reconcile;
pub mod scan;
pub mod scans;
pub mod service;
pub mod settings;
pub mod tags;
pub mod wiring;
