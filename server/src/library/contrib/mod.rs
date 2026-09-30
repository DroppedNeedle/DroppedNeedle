//! Library contributions: draft lifecycle, MusicBrainz submission, and the
//! verification worker (DroppedNeedle v3 stage 8).
//!
//! A contribution carries one local album toward MusicBrainz: the curator
//! shapes a [`models::ReleaseDraft`], checks duplicates, opens the seeded
//! release editor in their browser, and the verification worker confirms the
//! resulting release and links the album.
//!
//! Ported from v2 `backend/services/native/library_contribution_service.py`,
//! `library_contribution_verification_worker.py`, and
//! `backend/models/library_contribution.py`. Deliberate quirks keep `Quirk
//! (v2 ...)` citations at the code that preserves them.
//!
//! Boundaries (see `seams` for the traits):
//! - Identity reads, the evidence engine, and catalog invalidation are
//!   sibling-owned; this slice consumes them through narrow traits.
//! - Provider contact is reads-only with explicit priorities; the submission
//!   itself is a browser POST, mocked in tests by
//!   [`memory::FakeReleaseEditor`] - no live provider writes, ever.
//! - [`worker::spawn_verification_worker`] exposes the background loop; the
//!   stage-8 integrator owns supervision and `main.rs` wiring.
//!
//! Wiring note: this module is intentionally freestanding (only
//! `droppedneedle::providers::slots` from the crate). Tests include it via
//! `#[path]`; the integrator mounts it under `server/src/library`.

// The `#[path]` test harness only reaches the curated root piecemeal; the
// re-exports below are the integrator-facing API, not dead imports.
#![allow(unused_imports)]

pub mod error;
pub mod memory;
pub mod models;
pub mod rules;
pub mod seams;
pub mod service;
pub mod worker;
