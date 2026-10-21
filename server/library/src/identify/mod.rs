//! Library identification: the queue, the proofs, and the curator.
//!
//! Automatic reconciliation needs provider proof (a folded name never
//! merges); `decision_source` protects curator rows; retired ids live on
//! as aliases; release pins steer editions only.
//!
//! Layout: `models` holds the types, `rules` the pure product rules,
//! `queue` the scheduling policy, `stores` the ports, `sqlite` the
//! durable stores, `sources` the MusicBrainz and AcoustID seams,
//! `providers` the recall built on them, `evidence` the bridge to the
//! matching engine, `review` the curator operations, and `service` the
//! orchestrator.

pub mod artist_links;
pub mod evidence;
pub mod merged_ids;
pub mod models;
pub mod providers;
pub mod queue;
pub mod review;
pub mod rules;
pub mod service;
pub mod sources;
pub mod sqlite;
pub mod stores;
