//! Library identification: the queue, the proofs, and the curator.
//!
//! Automatic reconciliation needs provider proof (a folded name never
//! merges); album-level provider proof gates every name-anchored
//! substitution (see `rules`); `decision_source` protects curator rows;
//! retired ids live on as aliases; release pins steer editions only.
//!
//! Layout: `models` holds the types, `rules` the pure product rules,
//! `queue` the scheduling policy, `stores` the ports, `sqlite` the
//! durable stores, `memory` the test fakes, `providers` the MusicBrainz + AcoustID seams over the
//! provider clients, `review` the curator operations, and `service`
//! the orchestrator.

#[cfg(any(test, feature = "test-support"))]
pub mod memory;
pub mod models;
pub mod providers;
pub mod queue;
pub mod review;
pub mod rules;
pub mod service;
pub mod sqlite;
pub mod stores;
