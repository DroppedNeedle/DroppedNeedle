//! The ranker, pure: from a person's seeds and the kept lists of who is
//! like each, which artists to look at and which of their albums to put in
//! the pool, in this order.
//!
//! 1. Support: an artist's support is the sum, over every seed whose
//!    list names it, of that seed's weight times the link's strength.
//!    Agreement between seeds is that sum. A link Last.fm's list names as
//!    well as labs' is [`proposed::AGREEMENT_BOOST`] times as strong; one
//!    only Last.fm names counts with its match, fame divided out the same
//!    way ([`fame_divided`]). A seed the person asked for less from
//!    ("Less from this") weighs [`proposed::LESS_FROM_SEED`] of its weight,
//!    and one they asked for more from [`proposed::MORE_FROM_SEED`] times
//!    it ([`tuned_weight`]), so every link of it counts that much less or
//!    more.
//! 2. Artist gates: not an artist the person knows, not one the library
//!    has with a MusicBrainz id (an album of theirs held or not), not one
//!    they turned down, not Various Artists or another special-purpose id.
//! 3. Hubs out: with two taste groups or more, an artist less than
//!    [`proposed::HUB_SHARE`] of whose support comes from one group is left
//!    out.
//! 4. Album choice ([`Walk`]): the ranking is walked spread over the groups
//!    by share ([`walk_order`]), and each artist gives up to
//!    [`proposed::ALBUMS_PER_ARTIST`] albums, most listeners first, that
//!    pass the album gates. An artist with none left uses no place; one
//!    whose list is not kept yet uses one, as pending. The walk stops once
//!    [`proposed::WALK_ARTISTS`] artists hold an album or are pending.
//! 5. A co-credited album: an album found through an artist the person
//!    does not know is offered even when another of its credited artists
//!    (a singer they play, say) is known; its paths name that credit.
//! 6. Order ([`order`]): by support, then artist id, each artist's albums
//!    in the order chosen. Nothing random, no date.
//! 7. Depth: the ranking's sort and the order read support tilted by the
//!    person's [`Depth`] ([`tilted`]); next door is support itself.

use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

use droppedneedle_library::identify::artist_links::placeholder;

use super::candidate::{Candidate, CandidateKind, Link, Path, PathArtist, PathKind, PathSource};
use super::select::{Exposure, Floors};
use super::weekly::Depth;
use crate::discover::feedback::Feedback;
use crate::discover::listening::similar::FAME_EXPONENT;
use crate::discover::listening::{RankedAlbum, gate};

/// The numbers the pool and the selection were built with, each proposed:
/// the owner judges them on a real set before they are settled.
pub mod proposed {
    /// A day, ms.
    const DAY_MS: i64 = 86_400_000;

    /// How much stronger a link is when Last.fm's list names it too.
    pub const AGREEMENT_BOOST: f64 = 1.25;
    /// With two taste groups or more, the least part of an artist's support
    /// one group gives before the artist is a hub, and left out.
    pub const HUB_SHARE: f64 = 0.6;
    /// How many artists the walk fills, each holding an album or pending.
    pub const WALK_ARTISTS: usize = 80;
    /// The most albums one artist gives the pool.
    pub const ALBUMS_PER_ARTIST: usize = 2;
    /// The most candidates a person's pool keeps.
    pub const POOL_MAX: usize = 400;
    /// The fewest listeners an artist has to be selected.
    pub const ARTIST_FLOOR: i64 = 200;
    /// The fewest listeners an album has to be selected.
    pub const ALBUM_FLOOR: i64 = 25;
    /// The most slots one taste group takes on the first pass.
    pub const FIRST_PASS_CAP: usize = 2;
    /// How long a pick shown and passed over rests, ms: twelve weeks.
    pub const REST_MS: i64 = 12 * 7 * DAY_MS;
    /// How many resting albums rest their artist too.
    pub const REST_ARTIST_ALBUMS: usize = 3;
    /// What part of its weight a seed keeps once the person asked for less
    /// from it.
    pub const LESS_FROM_SEED: f64 = 0.5;
    /// How many times its weight a seed weighs once the person asked for
    /// more from it.
    pub const MORE_FROM_SEED: f64 = 1.5;

    // -- band links ------------------------------------------------------------

    /// How many of the person's heaviest seeds have their band links read.
    pub const BAND_SEEDS: usize = 10;
    /// How many of a seed band's members have their own links read.
    pub const MEMBERS_PER_BAND: usize = 3;
    /// The most members' links one build reads.
    pub const MEMBER_READS: usize = 10;
    /// A band with more member links than this is a collective: its
    /// members' other bands are not followed.
    pub const BAND_HUB: usize = 12;
    /// A member in more bands than this gives none of them.
    pub const MEMBER_HUB: usize = 8;
    /// How many linked artists give albums, strongest first.
    pub const LINKED_ARTISTS: usize = 10;
    /// The most of them one seed gives.
    pub const LINKED_PER_SEED: usize = 3;
    /// The strength of a direct band link: a member, a band of theirs or
    /// another name.
    pub const DIRECT_LINK: f64 = 0.5;
    /// The strength of a band sharing a member, or a subgroup.
    pub const SHARED_MEMBER: f64 = 0.25;

    // -- depth -----------------------------------------------------------------

    /// The pool order's tilt at home ground: an artist's support times its
    /// listeners over the floor to the minus this, so more listeners lead.
    pub const HOME_GROUND_TILT: f64 = -0.15;
    /// The pool order's tilt far out: fewer listeners lead.
    pub const FAR_OUT_TILT: f64 = 0.15;
    /// How many times the floors home ground holds to, and the part of
    /// them far out holds to (never zero).
    pub const DEPTH_FLOOR_STEP: i64 = 2;
}

/// The key the pool is ordered by at `depth`: `support` times `listeners`
/// over the depth's artist floor, to the minus the depth's tilt. A count
/// under the floor counts as the floor, so an artist the floor would refuse
/// never leads for its want of listeners; a count not known reads as
/// `median`, the middle count of the ones being ordered (`median_count`),
/// so it neither leads nor sinks. Next door it is `support` itself. Stored
/// support, reasons and link strengths never carry it.
pub fn tilted(support: f64, listeners: Option<i64>, median: Option<i64>, depth: Depth) -> f64 {
    let tilt = match depth {
        Depth::NextDoor => return support,
        Depth::HomeGround => proposed::HOME_GROUND_TILT,
        Depth::FarOut => proposed::FAR_OUT_TILT,
    };
    let floor = Floors::for_depth(depth).artist_listeners.max(1);
    let count = listeners.or(median).unwrap_or(floor).max(floor);
    support * (count as f64 / floor as f64).powf(-tilt)
}

/// The middle of the known `counts` (the lower of the two middle ones for
/// an even number); `None` when none is known.
fn median_count(counts: impl Iterator<Item = Option<i64>>) -> Option<i64> {
    let mut known: Vec<i64> = counts.flatten().collect();
    if known.is_empty() {
        return None;
    }
    let mid = (known.len() - 1) / 2;
    Some(*known.select_nth_unstable(mid).1)
}

/// What the person asked of one seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeedTune {
    /// Nothing.
    AsIs,
    /// Less from it.
    Less,
    /// More from it.
    More,
}

/// A seed's weight in the ranking: `weight`, [`proposed::LESS_FROM_SEED`]
/// of it when the person asked for less from it, or
/// [`proposed::MORE_FROM_SEED`] times it when they asked for more.
pub fn tuned_weight(weight: f64, tune: SeedTune) -> f64 {
    match tune {
        SeedTune::AsIs => weight,
        SeedTune::Less => weight * proposed::LESS_FROM_SEED,
        SeedTune::More => weight * proposed::MORE_FROM_SEED,
    }
}

/// The artist each seed's labs list names first among the artists the
/// person does not know (`known`), one entry a seed; `None` for a seed
/// whose list names none. A special-purpose id is no artist, and is passed
/// over.
fn closest_new<'a>(lists: &'a [Vec<SeedLink>], known: &HashSet<String>) -> Vec<Option<&'a str>> {
    lists
        .iter()
        .map(|links| {
            links
                .iter()
                .filter(|link| link.source == PathSource::ListenbrainzLabs)
                .filter(|link| !link.mbid.is_empty() && !placeholder(&link.mbid))
                .find(|link| !known.contains(&link.mbid))
                .map(|link| link.mbid.as_str())
        })
        .collect()
}

/// One of the person's seeds.
#[derive(Debug, Clone, PartialEq)]
pub struct Seed {
    /// Its MusicBrainz id, lowercased.
    pub mbid: String,
    /// Its name.
    pub name: String,
    /// The person's plays and imported listens of it.
    pub plays: i64,
    /// Its weight.
    pub weight: f64,
    /// Its taste group, if it is in one.
    pub group: Option<i64>,
    /// How many of its albums the library holds.
    pub albums_owned: i64,
}

/// One row of a kept similar-artists list, best first.
#[derive(Debug, Clone, PartialEq)]
pub struct ListRow {
    /// The neighbour's MusicBrainz id, lowercased.
    pub mbid: String,
    /// Its name, when the list gave one.
    pub name: Option<String>,
    /// Its strength: co-listening with fame divided out, the best 1.0.
    pub strength: f64,
}

/// One seed's kept lists: labs' and, where a Last.fm key works, Last.fm's,
/// each with the date it was fetched.
#[derive(Debug, Clone, Default)]
pub struct SeedLists {
    /// Labs' list.
    pub labs: Vec<ListRow>,
    /// When labs' list was fetched, unix ms.
    pub labs_fetched_at: Option<i64>,
    /// Last.fm's list, its match fame divided ([`fame_divided`]).
    pub lastfm: Vec<ListRow>,
    /// When Last.fm's list was fetched, unix ms.
    pub lastfm_fetched_at: Option<i64>,
}

/// One link from a seed, as the ranker reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct SeedLink {
    /// The neighbour, lowercased.
    pub mbid: String,
    /// Its name, when a list gave one.
    pub name: Option<String>,
    /// The link.
    pub link: Link,
    /// The list it was read from: labs' when labs names it.
    pub source: PathSource,
    /// When that list was fetched, unix ms.
    pub fetched_at: Option<i64>,
}

/// A seed's links from its two lists: every neighbour labs names, in labs'
/// order, then every one only Last.fm names, in Last.fm's. A link both
/// name is [`proposed::AGREEMENT_BOOST`] times as strong as labs' and keeps
/// labs' rank; one only Last.fm names has Last.fm's strength and rank.
pub fn links(lists: &SeedLists) -> Vec<SeedLink> {
    let lastfm: HashMap<&str, usize> = lists
        .lastfm
        .iter()
        .enumerate()
        .map(|(at, row)| (row.mbid.as_str(), at))
        .collect();
    let of = |list: &[ListRow]| i64::try_from(list.len()).unwrap_or(i64::MAX);
    let rank = |at: usize| i64::try_from(at + 1).unwrap_or(i64::MAX);
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    for (at, row) in lists.labs.iter().enumerate() {
        if !seen.insert(row.mbid.as_str()) {
            continue;
        }
        let agreed = lastfm.contains_key(row.mbid.as_str());
        let boost = if agreed {
            proposed::AGREEMENT_BOOST
        } else {
            1.0
        };
        links.push(SeedLink {
            mbid: row.mbid.clone(),
            name: row.name.clone(),
            link: Link {
                strength: row.strength * boost,
                rank: rank(at),
                of: of(&lists.labs),
                labs: true,
                lastfm: agreed,
                closest_new: false,
            },
            source: PathSource::ListenbrainzLabs,
            fetched_at: lists.labs_fetched_at,
        });
    }
    for (at, row) in lists.lastfm.iter().enumerate() {
        if !seen.insert(row.mbid.as_str()) {
            continue;
        }
        links.push(SeedLink {
            mbid: row.mbid.clone(),
            name: row.name.clone(),
            link: Link {
                strength: row.strength,
                rank: rank(at),
                of: of(&lists.lastfm),
                labs: false,
                lastfm: true,
                closest_new: false,
            },
            source: PathSource::Lastfm,
            fetched_at: lists.lastfm_fetched_at,
        });
    }
    links
}

/// Last.fm's list for one artist (`mbid`, name, match), best first, as
/// labs' is read: counted neighbours by match over their listener count to
/// [`FAME_EXPONENT`], scaled so the best is 1.0, then the uncounted in match
/// order, scaled below the last counted one.
pub fn fame_divided(
    rows: Vec<(String, Option<String>, f64)>,
    listeners: &HashMap<String, i64>,
) -> Vec<ListRow> {
    let raw_best = rows.iter().map(|row| row.2).fold(0.0_f64, f64::max);
    let mut counted = Vec::new();
    let mut uncounted = Vec::new();
    for (mbid, name, score) in rows {
        match listeners.get(&mbid) {
            Some(count) => {
                let value = score / ((*count).max(1) as f64).powf(FAME_EXPONENT);
                counted.push((value, mbid, name));
            }
            None => uncounted.push((score, mbid, name)),
        }
    }
    counted.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    uncounted.sort_by(|a, b| b.0.total_cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let best = counted.first().map_or(0.0, |row| row.0);
    let scaled = |value: f64| if best > 0.0 { value / best } else { 0.0 };
    let floor = counted.last().map_or(1.0, |row| scaled(row.0));
    let mut ranked: Vec<ListRow> = counted
        .into_iter()
        .map(|(value, mbid, name)| ListRow {
            mbid,
            name,
            strength: scaled(value),
        })
        .collect();
    ranked.extend(uncounted.into_iter().map(|(score, mbid, name)| ListRow {
        mbid,
        name,
        strength: if raw_best > 0.0 {
            floor * score / raw_best
        } else {
            0.0
        },
    }));
    ranked
}

/// Who an artist may not be: one the person knows or the library has,
/// one they turned down.
pub struct ArtistGates<'a> {
    /// Every artist the person's rows name, lowercased.
    pub known: &'a HashSet<String>,
    /// Every library artist with a MusicBrainz id, lowercased, whether or
    /// not the library holds an album of theirs.
    pub library: &'a HashSet<String>,
    /// What the person told Discover.
    pub feedback: &'a Feedback,
}

impl ArtistGates<'_> {
    /// Whether `mbid` is known to the person: theirs or the library's.
    pub fn knows(&self, mbid: &str) -> bool {
        self.known.contains(mbid) || self.library.contains(mbid)
    }

    /// Whether `mbid` may not be offered for a reason other than being
    /// known: a special-purpose id, or one the person turned down.
    pub fn refuses(&self, mbid: &str) -> bool {
        mbid.is_empty() || placeholder(mbid) || self.feedback.hides_artist(Some(mbid))
    }
}

/// One artist in the ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct RankedArtist {
    /// Its MusicBrainz id, lowercased.
    pub mbid: String,
    /// Its name, from the first list naming it; empty when none did.
    pub name: String,
    /// Its listeners, when counted.
    pub listeners: Option<i64>,
    /// Its support.
    pub support: f64,
    /// The group most of its support comes from; `None` for seeds in none.
    pub group: Option<i64>,
    /// That group's part of its support.
    pub group_support: f64,
    /// One seed path per seed pointing at it, strongest first.
    pub paths: Vec<Path>,
}

/// The ranking and how it was reached.
#[derive(Debug, Clone, Default)]
pub struct Ranking {
    /// The artists left, most support first, then by id.
    pub artists: Vec<RankedArtist>,
    /// Distinct artists the seeds point at, before any gate.
    pub neighbours_found: usize,
    /// Of those, the ones the person knows or the library has.
    pub known_left_out: usize,
    /// Of the rest, the hubs left out; `None` when the rule did not run
    /// (fewer than two groups).
    pub hubs_left_out: Option<usize>,
}

/// The order of a group bucket: by group number, seeds in no group last.
fn bucket_order(a: Option<i64>, b: Option<i64>) -> Ordering {
    match (a, b) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}

/// Rank the artists `lists` point at (one entry a seed, in `seeds`' order),
/// for a person with `groups` taste groups, through `gates`. `listeners`
/// holds the counted artists' listeners. The artists are sorted by their
/// support tilted at `depth`.
pub fn rank(
    seeds: &[Seed],
    lists: &[Vec<SeedLink>],
    listeners: &HashMap<String, i64>,
    groups: usize,
    gates: &ArtistGates<'_>,
    depth: Depth,
) -> Ranking {
    struct Pointed {
        name: Option<String>,
        paths: Vec<(Option<i64>, Path)>,
    }
    let mut pointed: HashMap<String, Pointed> = HashMap::new();
    let closest = closest_new(lists, gates.known);
    for ((seed, links), closest) in seeds.iter().zip(lists).zip(closest) {
        for link in links {
            if link.mbid.is_empty() {
                continue;
            }
            let entry = pointed.entry(link.mbid.clone()).or_insert(Pointed {
                name: None,
                paths: Vec::new(),
            });
            if entry.name.is_none() {
                entry.name.clone_from(&link.name);
            }
            let from_seed = |path: &(Option<i64>, Path)| {
                path.1
                    .artist
                    .as_ref()
                    .is_some_and(|artist| artist.mbid == seed.mbid)
            };
            if entry.paths.iter().any(from_seed) {
                continue;
            }
            entry.paths.push((
                seed.group,
                Path {
                    kind: PathKind::Seed,
                    source: link.source,
                    fetched_at: link.fetched_at,
                    artist: Some(PathArtist {
                        mbid: seed.mbid.clone(),
                        name: seed.name.clone(),
                        plays: seed.plays.max(0),
                        weight: seed.weight,
                        albums_owned: seed.albums_owned.max(0),
                    }),
                    link: Some(Link {
                        closest_new: link.source == PathSource::ListenbrainzLabs
                            && closest == Some(link.mbid.as_str()),
                        ..link.link.clone()
                    }),
                    known_credit: None,
                    fact: None,
                },
            ));
        }
    }

    let mut ranking = Ranking {
        neighbours_found: pointed.len(),
        hubs_left_out: (groups >= 2).then_some(0),
        ..Ranking::default()
    };
    for (mbid, entry) in pointed {
        if gates.knows(&mbid) {
            ranking.known_left_out += 1;
            continue;
        }
        if gates.refuses(&mbid) {
            continue;
        }
        let support: f64 = entry.paths.iter().map(|(_, path)| path.support()).sum();
        let mut by_group: Vec<(Option<i64>, f64)> = Vec::new();
        for (group, path) in &entry.paths {
            match by_group.iter_mut().find(|(at, _)| at == group) {
                Some((_, part)) => *part += path.support(),
                None => by_group.push((*group, path.support())),
            }
        }
        let (group, group_support) = by_group
            .into_iter()
            .min_by(|a, b| b.1.total_cmp(&a.1).then_with(|| bucket_order(a.0, b.0)))
            .unwrap_or((None, 0.0));
        if let Some(hubs) = ranking.hubs_left_out.as_mut()
            && support > 0.0
            && group_support / support < proposed::HUB_SHARE
        {
            *hubs += 1;
            continue;
        }
        let mut paths: Vec<Path> = entry.paths.into_iter().map(|(_, path)| path).collect();
        paths.sort_by(|a, b| {
            b.support().total_cmp(&a.support()).then_with(|| {
                let seed = |path: &Path| path.artist.as_ref().map(|artist| artist.mbid.clone());
                seed(a).cmp(&seed(b))
            })
        });
        ranking.artists.push(RankedArtist {
            listeners: listeners.get(&mbid).copied(),
            name: entry.name.unwrap_or_default(),
            mbid,
            support,
            group,
            group_support,
            paths,
        });
    }
    let median = median_count(ranking.artists.iter().map(|artist| artist.listeners));
    ranking.artists.sort_by(|a, b| {
        tilted(b.support, b.listeners, median, depth)
            .total_cmp(&tilted(a.support, a.listeners, median, depth))
            .then_with(|| a.mbid.cmp(&b.mbid))
    });
    ranking
}

/// The order the walk looks at `artists` in (positions in it): their own
/// order within each group, the groups taking turns by share as seats are
/// shared out by votes (Sainte-Laguë), earlier groups first on a tie.
/// Artists whose group is not among `shares`, or in none, share what is
/// left of the shares between them, and come last on a tie.
pub fn walk_order(artists: &[RankedArtist], shares: &[(i64, f64)]) -> Vec<usize> {
    let mut buckets: Vec<(Option<i64>, f64, std::collections::VecDeque<usize>)> = shares
        .iter()
        .map(|(group, share)| (Some(*group), share.max(0.0), Default::default()))
        .collect();
    let grouped: f64 = buckets.iter().map(|bucket| bucket.1).sum();
    buckets.push((None, (1.0 - grouped).max(0.0), Default::default()));
    let last = buckets.len() - 1;
    for (at, artist) in artists.iter().enumerate() {
        let bucket = buckets
            .iter()
            .position(|bucket| bucket.0.is_some() && bucket.0 == artist.group)
            .unwrap_or(last);
        buckets[bucket].2.push_back(at);
    }
    let mut taken = vec![0usize; buckets.len()];
    let mut order = Vec::with_capacity(artists.len());
    loop {
        let mut best: Option<(usize, f64)> = None;
        for (at, (_, share, queue)) in buckets.iter().enumerate() {
            if queue.is_empty() {
                continue;
            }
            let turn = share / (2 * taken[at] + 1) as f64;
            if best.is_none_or(|(_, best)| turn > best) {
                best = Some((at, turn));
            }
        }
        let Some((at, _)) = best else {
            break;
        };
        taken[at] += 1;
        if let Some(next) = buckets[at].2.pop_front() {
            order.push(next);
        }
    }
    order
}

/// What may not be an album in the pool, by release group: one the library
/// holds or is fetching, one the person has played, hidden or that rests.
pub struct AlbumGates<'a> {
    /// Release groups the library holds, lowercased.
    pub owned: &'a HashSet<String>,
    /// Release groups the library is fetching, lowercased.
    pub fetching: &'a HashSet<String>,
    /// Release groups the person has played, lowercased.
    pub played: &'a HashSet<String>,
    /// What the person told Discover.
    pub feedback: &'a Feedback,
    /// What was shown and passed over.
    pub exposure: &'a Exposure,
    /// Who the person knows, for the co-credit note.
    pub artists: &'a ArtistGates<'a>,
}

impl AlbumGates<'_> {
    /// Whether the album `release_group` filed under `artist` may go in.
    pub fn admits(&self, release_group: &str, artist: &str) -> bool {
        !self.owned.contains(release_group)
            && !self.fetching.contains(release_group)
            && !self.played.contains(release_group)
            && !self.feedback.hides_album(Some(release_group), Some(artist))
            && !self.exposure.rests_group(release_group)
    }
}

/// An artist's kept album list as the walk reads it.
#[derive(Debug, Clone, Copy)]
pub enum AlbumList<'a> {
    /// Not kept yet: the artist is pending, and its list is wanted.
    Missing,
    /// Kept, and the artist has no album to offer.
    Nothing,
    /// The kept list.
    Found(&'a [RankedAlbum]),
}

/// What one artist did in the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// It gave the pool an album or two.
    Held,
    /// Its list is not kept yet.
    Pending,
    /// It had no album left, or it rests: it uses no place.
    NoneLeft,
}

/// The album walk: it takes artists in [`walk_order`] and stops once
/// [`proposed::WALK_ARTISTS`] (or the limit given) hold an album or are
/// pending.
#[derive(Debug, Clone)]
pub struct Walk {
    limit: usize,
    held: usize,
    /// The artists whose list is not kept yet, in walk order.
    pub pending: Vec<String>,
    /// How many artists had no album left.
    pub without_album: usize,
    /// The albums taken, in walk order.
    pub candidates: Vec<Candidate>,
}

/// `rows` most listeners first, then by listens; the uncounted after them,
/// newest first. Ties keep the list's order.
pub(crate) fn by_listeners(rows: &[RankedAlbum]) -> Vec<&RankedAlbum> {
    let mut rows: Vec<&RankedAlbum> = rows.iter().collect();
    rows.sort_by(|a, b| {
        let (a_facts, b_facts) = (&a.album.facts, &b.album.facts);
        match (a_facts.listeners, b_facts.listeners) {
            (Some(left), Some(right)) => right.cmp(&left).then(b.listen_count.cmp(&a.listen_count)),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => match (&a_facts.first_release_date, &b_facts.first_release_date) {
                (Some(a), Some(b)) => b.cmp(a),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => Ordering::Equal,
            },
        }
    });
    rows
}

fn lower(text: &str) -> String {
    text.trim().to_ascii_lowercase()
}

/// `album` as a candidate filed under `artist`, kind `kind`, with `paths`.
/// A known artist credited beside `artist` is noted on every path.
pub fn album_candidate(
    album: &RankedAlbum,
    (artist_mbid, artist_name, listeners): (&str, &str, Option<i64>),
    kind: CandidateKind,
    (paths, support, group, group_support): (&[Path], f64, Option<i64>, f64),
    gates: &ArtistGates<'_>,
) -> Candidate {
    let facts = &album.album.facts;
    let credited: Vec<String> = facts
        .credited_artist_mbids
        .iter()
        .flatten()
        .map(|id| lower(id))
        .filter(|id| !id.is_empty())
        .collect();
    let known_credit = credited
        .iter()
        .find(|id| id.as_str() != artist_mbid && gates.knows(id))
        .cloned();
    let paths = paths
        .iter()
        .cloned()
        .map(|mut path| {
            path.known_credit.clone_from(&known_credit);
            path
        })
        .collect();
    let name = album.album.artist_name.trim();
    Candidate {
        release_group_mbid: lower(&album.album.release_group_mbid),
        title: album.album.title.clone(),
        artist_mbid: artist_mbid.to_owned(),
        artist_name: if name.is_empty() {
            artist_name.to_owned()
        } else {
            name.to_owned()
        },
        credited_artist_mbids: credited,
        primary_type: facts.primary_type.clone(),
        first_release_date: facts.first_release_date.clone(),
        album_listeners: facts.listeners,
        artist_listeners: listeners,
        kind,
        paths,
        support,
        group,
        group_support,
    }
}

impl Walk {
    /// A walk that stops at `limit` artists.
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            held: 0,
            pending: Vec::new(),
            without_album: 0,
            candidates: Vec::new(),
        }
    }

    /// Whether enough artists hold an album or are pending.
    pub fn done(&self) -> bool {
        self.held + self.pending.len() >= self.limit
    }

    /// Take `artist`, whose kept list is `list`, through `gates`: up to
    /// [`proposed::ALBUMS_PER_ARTIST`] of its albums, most listeners first.
    /// An artist with three resting albums rests too, and its list is not
    /// read.
    pub fn step(
        &mut self,
        artist: &RankedArtist,
        list: AlbumList<'_>,
        gates: &AlbumGates<'_>,
    ) -> Step {
        if gates.exposure.rests_artist(&artist.mbid) {
            self.without_album += 1;
            return Step::NoneLeft;
        }
        let rows = match list {
            AlbumList::Missing => {
                self.pending.push(artist.mbid.clone());
                return Step::Pending;
            }
            AlbumList::Nothing => &[][..],
            AlbumList::Found(rows) => rows,
        };
        let mut taken = 0;
        let mut seen = HashSet::new();
        for album in by_listeners(rows) {
            if taken >= proposed::ALBUMS_PER_ARTIST {
                break;
            }
            let group = lower(&album.album.release_group_mbid);
            if group.is_empty()
                || !seen.insert(group.clone())
                || !gate::may_offer(&album.album, &artist.mbid)
                || !gates.admits(&group, &artist.mbid)
            {
                continue;
            }
            self.candidates.push(album_candidate(
                album,
                (&artist.mbid, &artist.name, artist.listeners),
                CandidateKind::NewArtist,
                (
                    &artist.paths,
                    artist.support,
                    artist.group,
                    artist.group_support,
                ),
                gates.artists,
            ));
            taken += 1;
        }
        if taken == 0 {
            self.without_album += 1;
            Step::NoneLeft
        } else {
            self.held += 1;
            Step::Held
        }
    }
}

/// The pool's order at one depth: most support tilted at the depth first
/// ([`tilted`], a count not known read as the median of the candidates it
/// was made from), then by artist id. [`order`] sorts by it and the pool
/// places its band links by it, so the two never disagree.
pub struct PoolOrder {
    median: Option<i64>,
    depth: Depth,
}

impl PoolOrder {
    /// The order of `candidates` at `depth`.
    pub fn new(candidates: &[Candidate], depth: Depth) -> Self {
        let median = median_count(
            candidates
                .iter()
                .map(|candidate| candidate.artist_listeners),
        );
        Self { median, depth }
    }

    /// Whether `a` comes before, beside or after `b`.
    pub fn cmp(&self, a: &Candidate, b: &Candidate) -> Ordering {
        let key = |c: &Candidate| tilted(c.support, c.artist_listeners, self.median, self.depth);
        key(b)
            .total_cmp(&key(a))
            .then_with(|| a.artist_mbid.cmp(&b.artist_mbid))
    }
}

/// `candidates` in the pool's order `by`, each artist's albums in the order
/// they came; one row a release group, the first kept; at most
/// [`proposed::POOL_MAX`].
pub fn order(mut candidates: Vec<Candidate>, by: &PoolOrder) -> Vec<Candidate> {
    candidates.sort_by(|a, b| by.cmp(a, b));
    let mut seen = HashSet::new();
    candidates.retain(|candidate| seen.insert(candidate.release_group_mbid.clone()));
    candidates.truncate(proposed::POOL_MAX);
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(mbid: &str, weight: f64, group: Option<i64>) -> Seed {
        Seed {
            mbid: mbid.to_owned(),
            name: mbid.to_uppercase(),
            plays: 10,
            weight,
            group,
            albums_owned: 0,
        }
    }

    fn row(mbid: &str, strength: f64) -> ListRow {
        ListRow {
            mbid: mbid.to_owned(),
            name: Some(mbid.to_uppercase()),
            strength,
        }
    }

    fn labs(rows: Vec<ListRow>) -> Vec<SeedLink> {
        links(&SeedLists {
            labs: rows,
            ..SeedLists::default()
        })
    }

    fn ranked(
        seeds: &[Seed],
        lists: &[Vec<SeedLink>],
        listeners: &HashMap<String, i64>,
        groups: usize,
    ) -> Ranking {
        let (known, library, feedback) = (HashSet::new(), HashSet::new(), Feedback::default());
        let gates = ArtistGates {
            known: &known,
            library: &library,
            feedback: &feedback,
        };
        rank(seeds, lists, listeners, groups, &gates, Depth::NextDoor)
    }

    fn names(ranking: &Ranking) -> Vec<&str> {
        ranking
            .artists
            .iter()
            .map(|artist| artist.mbid.as_str())
            .collect()
    }

    #[test]
    fn rank_two_seeds_agreeing_beat_one_equal_link() {
        let seeds = [seed("s1", 1.0, Some(0)), seed("s2", 1.0, Some(0))];
        let lists = [
            labs(vec![row("lonely", 0.8), row("shared", 0.8)]),
            labs(vec![row("shared", 0.8)]),
        ];
        let ranking = ranked(&seeds, &lists, &HashMap::new(), 1);
        assert_eq!(names(&ranking), ["shared", "lonely"]);
        let shared = &ranking.artists[0];
        assert!((shared.support - 1.6).abs() < 1e-9, "{}", shared.support);
        assert_eq!(shared.paths.len(), 2, "one path a pointing seed");
    }

    #[test]
    fn rank_the_less_known_of_two_equally_co_listened_ranks_higher() {
        // The same co-listening score; the famous one has a hundred times
        // the listeners, so fame divided out puts it lower.
        let listeners = HashMap::from([
            ("famous".to_owned(), 1_000_000),
            ("small".to_owned(), 10_000),
        ]);
        let list = fame_divided(
            vec![
                ("famous".to_owned(), None, 50.0),
                ("small".to_owned(), None, 50.0),
            ],
            &listeners,
        );
        let seeds = [seed("s1", 1.0, Some(0))];
        let lists = [labs(list)];
        let ranking = ranked(&seeds, &lists, &listeners, 1);
        assert_eq!(names(&ranking), ["small", "famous"]);
        assert!(ranking.artists[0].support > ranking.artists[1].support);
    }

    #[test]
    fn rank_a_hub_split_evenly_over_two_groups_is_skipped() {
        let seeds = [seed("prog", 1.0, Some(0)), seed("pop", 1.0, Some(1))];
        let lists = [
            labs(vec![row("hub", 0.9), row("prog-only", 0.5)]),
            labs(vec![row("hub", 0.9), row("pop-only", 0.5)]),
        ];
        let ranking = ranked(&seeds, &lists, &HashMap::new(), 2);
        assert_eq!(names(&ranking), ["pop-only", "prog-only"]);
        assert_eq!(ranking.hubs_left_out, Some(1));
        assert_eq!(ranking.neighbours_found, 3);

        // With one group the rule does not run.
        let one = ranked(&seeds, &lists, &HashMap::new(), 1);
        assert_eq!(names(&one)[0], "hub");
        assert_eq!(one.hubs_left_out, None);
    }

    #[test]
    fn rank_a_link_both_sources_name_beats_the_same_link_from_one() {
        let seeds = [seed("s1", 1.0, Some(0))];
        let both = links(&SeedLists {
            labs: vec![row("labs-only", 0.8), row("agreed", 0.8)],
            lastfm: vec![row("agreed", 0.5)],
            ..SeedLists::default()
        });
        let ranking = ranked(&seeds, &[both], &HashMap::new(), 1);
        assert_eq!(names(&ranking), ["agreed", "labs-only"]);
        let link = ranking.artists[0].paths[0].link.as_ref().expect("a link");
        assert!(link.labs && link.lastfm, "both sources are recorded");
        assert!((link.strength - 0.8 * proposed::AGREEMENT_BOOST).abs() < 1e-9);
        assert_eq!(
            ranking.artists[0].paths[0].source,
            PathSource::ListenbrainzLabs
        );
    }

    #[test]
    fn rank_the_same_input_twice_gives_the_same_order() {
        let seeds: Vec<Seed> = (0..6)
            .map(|n| {
                seed(
                    &format!("s{n}"),
                    1.0 + f64::from(n % 3),
                    Some(i64::from(n % 2)),
                )
            })
            .collect();
        let lists: Vec<Vec<SeedLink>> = (0..6)
            .map(|n| {
                labs(
                    (0..40)
                        .map(|m| row(&format!("a{:02}", (m * 7 + n) % 50), 1.0 / f64::from(m + 1)))
                        .collect(),
                )
            })
            .collect();
        let first = ranked(&seeds, &lists, &HashMap::new(), 2);
        let again = ranked(&seeds, &lists, &HashMap::new(), 2);
        assert_eq!(first.artists, again.artists);
        assert!(!first.artists.is_empty());
    }
}
