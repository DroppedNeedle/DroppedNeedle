//! Library contributions: draft lifecycle, MusicBrainz submission, and the
//! verification worker.
//!
//! A contribution carries one local album toward MusicBrainz: the curator
//! shapes a [`models::ReleaseDraft`], checks duplicates, opens the seeded
//! release editor in their browser, and the verification worker confirms the
//! resulting release and links the album.
//!
//! Ported from v2's library contribution service, its verification worker,
//! and its contribution models. Intended quirks keep `Quirk (v2 ...)`
//! citations at the code that preserves them.
//!
//! Boundaries (see `seams` for the traits):
//! - Identity reads, the evidence engine, and catalog invalidation are
//!   owned elsewhere; contributions consume them through narrow traits.
//! - Provider contact is reads-only with explicit priorities; the submission
//!   itself is a browser POST, mocked in tests by
//!   [`memory::FakeReleaseEditor`] - no live provider writes, ever.
//! - [`worker::spawn_verification_worker`] runs the background loop;
//!   `LibrarySetup::spawn_loops` starts it.

pub mod error;
pub mod memory;
pub mod models;
pub mod rules;
pub mod seams;
pub mod service;
pub mod worker;
