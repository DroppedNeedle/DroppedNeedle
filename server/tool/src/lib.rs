//! The offline migration tool: v2 export, export validation and import,
//! and the operator helpers behind the `droppedneedle-tool` binary.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub(crate) mod allowlist;
pub mod export;
pub mod import;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
pub mod tooling;
