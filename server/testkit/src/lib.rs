//! Test support for the DroppedNeedle server: in-repo mock servers for the
//! outside services it talks to, served on loopback. The server crate takes
//! this crate as a dev-dependency only, so no shipping build contains a
//! mock. It must never depend on the server crate.
//!
//! The `droppedneedle-mocks` binary serves the mock slskd and MusicBrainz
//! on given addresses, beside the server container in the browser journeys.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod imports;
pub mod musicbrainz;
pub mod remotes;
pub mod scripted;
pub mod slskd;
pub mod usenet;
