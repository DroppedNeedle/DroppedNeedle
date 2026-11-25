//! What the pool holds: a candidate is one release group for one person,
//! with every piece of evidence that found it.
//!
//! The kinds and the path kinds and sources are declared whole, the values
//! later work first writes included, since their stored CHECKs cannot
//! change without rebuilding a table.

/// How a candidate came into the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateKind {
    /// An album by an artist the person's artists point at.
    NewArtist,
    /// An album from a service's own picks for the person.
    SourcePick,
    /// An album missing from an artist the person collects. Not written
    /// yet: the gaps source writes it.
    Gap,
    /// An album of the person's own shelves. Not written yet: the shelves
    /// source writes it.
    Shelf,
    /// A new release by one of the person's artists, or one close to theirs.
    Fresh,
    /// An album along a thread of credits or labels. Not written yet: the
    /// threads write it.
    Thread,
}

droppedneedle_kernel::spelled!(CandidateKind {
    NewArtist => "new_artist",
    SourcePick => "source_pick",
    Gap => "gap",
    Shelf => "shelf",
    Fresh => "fresh",
    Thread => "thread",
});

/// What one piece of evidence is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathKind {
    /// One of the person's artists links to the candidate's artist.
    Seed,
    /// A service named the album for the person.
    Source,
    /// The library holds the artist's other albums. Not written yet: the
    /// gaps source writes it.
    Library,
    /// The album is a new release.
    Fresh,
    /// A credit or label leads to the album. Not written yet: the threads
    /// write it.
    Fact,
}

droppedneedle_kernel::spelled!(PathKind {
    Seed => "seed",
    Source => "source",
    Library => "library",
    Fresh => "fresh",
    Fact => "fact",
});

/// Where a path's fact came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PathSource {
    /// ListenBrainz labs' co-listening list.
    ListenbrainzLabs,
    /// Last.fm's similar artists.
    Lastfm,
    /// ListenBrainz: the person's weekly playlists, and its sitewide new
    /// releases.
    Listenbrainz,
    /// The install's MusicBrainz source: the new releases its follow poll
    /// found.
    Musicbrainz,
    /// The library. Not written yet: the gaps source writes it.
    Library,
    /// The project's data file. Not written yet: the threads write it.
    DataFile,
}

droppedneedle_kernel::spelled!(PathSource {
    ListenbrainzLabs => "listenbrainz_labs",
    Lastfm => "lastfm",
    Listenbrainz => "listenbrainz",
    Musicbrainz => "musicbrainz",
    Library => "library",
    DataFile => "data_file",
});

/// One of the person's artists a path runs through.
#[derive(Debug, Clone, PartialEq)]
pub struct PathArtist {
    /// Its MusicBrainz id, lowercased.
    pub mbid: String,
    /// Its name.
    pub name: String,
    /// The person's plays and imported listens of it.
    pub plays: i64,
    /// Its weight in their taste.
    pub weight: f64,
    /// How many of its albums the library holds.
    pub albums_owned: i64,
}

/// A link from one of the person's artists to the candidate's artist.
#[derive(Debug, Clone, PartialEq)]
pub struct Link {
    /// Its strength: co-listening with fame divided out, the best in the
    /// list 1.0, raised where both services name it.
    pub strength: f64,
    /// The candidate's artist's place in that artist's list, from 1.
    pub rank: i64,
    /// How long that list is.
    pub of: i64,
    /// Whether ListenBrainz labs' list names it.
    pub labs: bool,
    /// Whether Last.fm's list names it.
    pub lastfm: bool,
    /// Whether the candidate's artist was the first in that artist's labs
    /// list the person did not know, every artist above it being one of
    /// theirs.
    pub closest_new: bool,
}

/// One piece of evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct Path {
    /// What it is.
    pub kind: PathKind,
    /// Where its fact came from.
    pub source: PathSource,
    /// When that fact was fetched, unix ms, when known.
    pub fetched_at: Option<i64>,
    /// The person's artist it runs through, for a seed path.
    pub artist: Option<PathArtist>,
    /// The link, for a seed path.
    pub link: Option<Link>,
    /// A known artist credited on the album beside the candidate's own,
    /// lowercased: the album is offered all the same.
    pub known_credit: Option<String>,
}

impl Path {
    /// What this path puts behind its candidate: its artist's weight times
    /// the link's strength; nothing for a path with no link.
    pub fn support(&self) -> f64 {
        match (&self.artist, &self.link) {
            (Some(artist), Some(link)) => artist.weight * link.strength,
            _ => 0.0,
        }
    }
}

/// One release group the person could be offered.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// Its MusicBrainz release-group id, lowercased.
    pub release_group_mbid: String,
    /// Its title.
    pub title: String,
    /// The artist it is filed under, lowercased.
    pub artist_mbid: String,
    /// That artist's name.
    pub artist_name: String,
    /// Every credited artist, in credit order, when known.
    pub credited_artist_mbids: Vec<String>,
    /// Its primary type, when known.
    pub primary_type: Option<String>,
    /// Its first release date, when known.
    pub first_release_date: Option<String>,
    /// The album's listeners, when counted.
    pub album_listeners: Option<i64>,
    /// The artist's listeners, when counted.
    pub artist_listeners: Option<i64>,
    /// How it came in.
    pub kind: CandidateKind,
    /// The evidence, strongest first.
    pub paths: Vec<Path>,
    /// Its support: the sum of its paths' ([`Path::support`]).
    pub support: f64,
    /// The taste group most of its support comes from; `None` when that is
    /// seeds in no group, or it has none: such a pick is further out.
    pub group: Option<i64>,
    /// That group's part of its support.
    pub group_support: f64,
}

impl Candidate {
    /// The person's artist behind its strongest seed path, if it has one;
    /// the first of equals.
    pub fn strongest_seed(&self) -> Option<&str> {
        let mut strongest: Option<&Path> = None;
        for path in self.paths.iter().filter(|path| path.kind == PathKind::Seed) {
            if strongest.is_none_or(|best| path.support() > best.support()) {
                strongest = Some(path);
            }
        }
        strongest
            .and_then(|path| path.artist.as_ref())
            .map(|artist| artist.mbid.as_str())
    }
}
