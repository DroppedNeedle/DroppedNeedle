//! Stage-8 library engine: scan, tags, identify, publish, contrib.
//!
//! Five slices, one engine. `scan` owns discovery (roots, walk,
//! scheduling, supervision) and never writes music files; `tags`
//! owns reads, probes, fingerprints, and the save wrapper; `identify`
//! owns provider identification plus the curator review queue;
//! `publish` owns every managed-file write through the staged
//! publisher; `contrib` owns MusicBrainz contributions plus the
//! verification worker.
//!
//! [`adapters`] implements the seams between slices (tag reads,
//! identify enqueue, tag staging, contribution ports) and [`wiring`]
//! owns the [`wiring::LibrarySetup`] bundle `create_app` mounts:
//! stores, services, HTTP routes, background loops, and startup
//! recovery. HTTP lives under [`http`].
//!
//! Store durability follows the stage-6 precedent: scan, identify,
//! and contribution state run on the slices' memory stores (durable
//! SQLite ports are a later persistence tier), while publish
//! journals, snapshots, baselines, and the catalog shadow run on a
//! dedicated rusqlite database with idempotent schema.

pub mod adapters;
pub mod contrib;
pub mod http;
pub mod identify;
pub mod publish;
pub mod scan;
pub mod tags;
pub mod wiring;
