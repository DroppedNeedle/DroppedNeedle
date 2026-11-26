//! What the pool holds: a candidate is one release group for one person,
//! with every piece of evidence that found it.
//!
//! The kinds and the path kinds and sources are declared whole, the values
//! later work first writes included, since their stored CHECKs cannot
//! change without rebuilding a table.

use super::rank::proposed;

/// How a candidate came into the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateKind {
    /// An album by an artist the person's artists point at.
    NewArtist,
    /// An album from a service's own picks for the person.
    SourcePick,
    /// An album missing from an artist the person collects.
    Gap,
    /// An album of the person's own shelves. Not written: those are owned
    /// albums, read for the page and never put in the pool.
    Shelf,
    /// A new release by one of the person's artists, or one close to theirs.
    Fresh,
    /// An album by an artist one of the person's artists is linked to by
    /// its band: a member, a band of theirs, another name or a member's
    /// other band.
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
    /// The library holds the artist's other albums.
    Library,
    /// The album is a new release.
    Fresh,
    /// A band link leads to the album's artist from one of the person's
    /// artists.
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
    /// found, and band links.
    Musicbrainz,
    /// The library.
    Library,
}

droppedneedle_kernel::spelled!(PathSource {
    ListenbrainzLabs => "listenbrainz_labs",
    Lastfm => "lastfm",
    Listenbrainz => "listenbrainz",
    Musicbrainz => "musicbrainz",
    Library => "library",
});

/// What a band-link pick's artist is to the person's artist it was found
/// through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BandLinkRole {
    /// A person in that band (a member, its founder, a collaborator in
    /// that project).
    Member,
    /// A band that artist is in.
    Band,
    /// That artist under another name.
    OtherName,
    /// A band sharing a member with it, or a subgroup of it.
    SharedMember,
}

droppedneedle_kernel::spelled!(BandLinkRole {
    Member => "member",
    Band => "band",
    OtherName => "other_name",
    SharedMember => "shared_member",
});

impl BandLinkRole {
    /// How strong a link of this role is: a direct one
    /// ([`proposed::DIRECT_LINK`]) or one through a member
    /// ([`proposed::SHARED_MEMBER`]).
    pub fn strength(self) -> f64 {
        match self {
            Self::Member | Self::Band | Self::OtherName => proposed::DIRECT_LINK,
            Self::SharedMember => proposed::SHARED_MEMBER,
        }
    }
}

/// The member a band link runs through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Via {
    /// Its MusicBrainz id, lowercased.
    pub mbid: String,
    /// Its name.
    pub name: String,
}

/// The band link behind a fact path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactLink {
    /// What the pick's artist is to the person's artist.
    pub role: BandLinkRole,
    /// The member it runs through, when it is one member; none for a
    /// subgroup or a band sharing several.
    pub via: Option<Via>,
    /// The linked artist's own name, as the link names it; an album's
    /// credit may name another.
    pub artist_name: Option<String>,
}

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
    /// The band link, for a fact path.
    pub fact: Option<FactLink>,
}

impl Path {
    /// What this path puts behind its candidate: its artist's weight times
    /// the link's strength, or times its band link's role's strength;
    /// nothing for a path with neither.
    pub fn support(&self) -> f64 {
        match (&self.artist, &self.link, &self.fact) {
            (Some(artist), Some(link), _) => artist.weight * link.strength,
            (Some(artist), None, Some(fact)) => artist.weight * fact.role.strength(),
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
    /// The person's artist behind its strongest seed or band-link path, if
    /// it has one; the first of equals.
    pub fn strongest_seed(&self) -> Option<&str> {
        let mut strongest: Option<&Path> = None;
        for path in self
            .paths
            .iter()
            .filter(|path| matches!(path.kind, PathKind::Seed | PathKind::Fact))
        {
            if strongest.is_none_or(|best| path.support() > best.support()) {
                strongest = Some(path);
            }
        }
        strongest
            .and_then(|path| path.artist.as_ref())
            .map(|artist| artist.mbid.as_str())
    }
}
