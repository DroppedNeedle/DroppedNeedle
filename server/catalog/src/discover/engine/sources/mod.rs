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
//! - [`gaps`]: albums missing from the artists they play that the library
//!   collects (`gap` candidates).
//! - [`band_links`]: albums by the members, bands and other names their
//!   seeds are linked to by MusicBrainz (`thread` candidates).
//! - [`shelves`]: their own albums played a lot but not lately, and the
//!   ones they asked for and never played; owned albums, read for the page
//!   and never put in the pool.

pub(crate) mod band_links;
pub(crate) mod fresh;
pub(crate) mod gaps;
pub(crate) mod listenbrainz_picks;
pub(crate) mod neighbours;
pub(crate) mod shelves;
