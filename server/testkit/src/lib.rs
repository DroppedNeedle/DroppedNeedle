//! Test support for the DroppedNeedle server: in-repo mock servers for the
//! outside services it talks to, served on loopback. The server crate takes
//! this crate as a dev-dependency only, so no shipping build contains a
//! mock. It must never depend on the server crate.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

pub mod imports;
pub mod remotes;
pub mod slskd;
pub mod usenet;
