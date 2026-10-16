//! The read side: library, search, artist and album pages, discover,
//! collections and the platform reads, plus upcoming concerts.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod concerts;
pub mod reads;
