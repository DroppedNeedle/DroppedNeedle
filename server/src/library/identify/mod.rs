//! Library identification: the queue, the proofs, and the curator.
//!
//! The stage-8 identify slice. Automatic reconciliation needs provider
//! proof (a folded name never merges); F-IDENT-01 option B gates every
//! album-level substitution; `decision_source` protects curator rows;
//! retired ids live on as aliases; release pins steer editions only.
//!
//! Layout: `models` holds the types, `rules` the pure product rules,
//! `queue` the scheduling policy, `stores` the ports, `memory` the
//! test fakes, `providers` the MusicBrainz + AcoustID seams over the
//! stage-5 clients, `review` the curator operations, and `service`
//! the orchestrator.

pub mod memory;
pub mod models;
pub mod providers;
pub mod queue;
pub mod review;
pub mod rules;
pub mod service;
pub mod stores;
