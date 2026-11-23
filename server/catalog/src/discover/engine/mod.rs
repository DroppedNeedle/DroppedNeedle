//! What Discover works out for each person from their own rows, apart from
//! any page build: a picture of their taste and the set of what they know.
//!
//! - [`history`]: the one read of a person's history, their plays in the
//!   app and the listens imported from ListenBrainz, and the top lists the
//!   app makes from it.
//! - [`taste`]: the groups their seeds fall into, pure.
//! - [`taste_store`]: the four tables one job rebuilds whole per person,
//!   the build, the groups, the known artists and the known release groups,
//!   and the seeds read back from them.
//! - [`jobs`]: the `discover.taste` job kind.
//! - [`schedule`]: who is due a build, asked by
//!   [`Due`](crate::discover::due::Due).
//!
//! What the library holds is the other half of what a person knows, and is
//! read live where it is needed, never copied here.

pub mod history;
pub mod jobs;
pub(crate) mod schedule;
pub(crate) mod sql;
pub mod taste;
pub(crate) mod taste_store;
