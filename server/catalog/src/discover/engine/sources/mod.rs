//! Where the pool's candidates come from. Each source reads kept rows and
//! answers what the ranker needs, or says its facts are missing, in which
//! case the build keeps the previous pool's rows of that source.
//!
//! - [`neighbours`]: the artists each seed's kept similar-artists lists
//!   name (`new_artist` candidates, one seed path a pointing seed).
//! - [`listenbrainz_picks`]: the person's own weekly ListenBrainz playlist
//!   while it is this week's (`source_pick` candidates).
//! - [`fresh`]: the albums and EPs on the person's list of new releases
//!   from their artists (`fresh` candidates).

pub(crate) mod fresh;
pub(crate) mod listenbrainz_picks;
pub(crate) mod neighbours;
