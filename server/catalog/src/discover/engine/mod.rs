//! What Discover works out for each person from their own rows, apart from
//! any page build: a picture of their taste, the set of what they know, and
//! the pool of albums they could be offered.
//!
//! - [`history`]: the one read of a person's history, their plays in the
//!   app and the listens imported from ListenBrainz, and the top lists the
//!   app makes from it.
//! - [`taste`]: the groups their seeds fall into, pure.
//! - [`taste_store`]: the four tables one job rebuilds whole per person,
//!   the build, the groups, the known artists and the known release groups,
//!   and the seeds read back from them.
//! - [`jobs`]: the `discover.taste` job kind.
//! - [`candidate`]: what the pool holds, one release group with its
//!   evidence.
//! - [`rank`] and [`select`]: the ranker and the selection, pure.
//! - [`sources`]: where the pool's candidates come from.
//! - [`pool`]: the `discover.pool` job kind, and [`pool_store`] its three
//!   tables and the pool read back.
//! - [`reason`]: a pick's one line, pure, from its paths; [`evidence`]
//!   what "Why this?" opens, from stored rows.
//! - [`weekly`]: the `discover.weekly_set` job kind and the routes' answers,
//!   and [`weekly_store`] its tables: each person's weekly sets, kept as a
//!   record once built, and their set day.
//! - [`app_lists`]: the `discover.app_lists` job kind, the playlists kept
//!   for a person's apps from their newest set and their shelves.
//! - [`archive`]: past weeks, each set read back as it was shown, and the
//!   albums kept from them.
//! - [`schedule`]: who is due a build, asked by
//!   [`Due`](crate::discover::due::Due).
//!
//! What the library holds is the other half of what a person knows, and is
//! read live where it is needed, never copied here.

pub mod app_lists;
pub mod archive;
pub mod candidate;
pub mod evidence;
pub mod history;
pub mod jobs;
pub mod pool;
pub mod pool_store;
pub mod rank;
pub mod reason;
pub(crate) mod schedule;
pub mod select;
pub(crate) mod sources;
pub(crate) mod sql;
pub mod taste;
pub(crate) mod taste_store;
pub mod weekly;
pub(crate) mod weekly_store;
