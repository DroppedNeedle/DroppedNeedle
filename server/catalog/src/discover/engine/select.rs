//! Choosing picks from a pool, pure: [`select`] answers up to `n`.
//!
//! - Floors are applied here, not when the pool is built, so the depth
//!   control moves them at once ([`Floors::for_depth`]): next door, an
//!   artist needs [`proposed::ARTIST_FLOOR`] listeners and an album
//!   [`proposed::ALBUM_FLOOR`]. A count not known
//!   passes, as every unknown fact does.
//! - Proportion: slots go to the taste groups by share, the largest
//!   remainder first, at most [`proposed::FIRST_PASS_CAP`] a group on the
//!   first pass; slots still empty go round again in share order without
//!   the cap. A candidate in no group takes a slot only when the groups
//!   cannot fill their own.
//! - Variety: one album an artist, and one seed is the strongest path of
//!   at most [`SEED_PICKS`] picks.
//! - Memory ([`Exposure`]): an album shown in the last
//!   [`proposed::REST_MS`] and neither sampled nor asked for rests; so does
//!   an artist with [`proposed::REST_ARTIST_ALBUMS`] such albums.
//! - Never padded: a short answer says why.

use std::collections::{HashMap, HashSet};

use super::candidate::Candidate;
use super::rank::proposed;
use super::weekly::Depth;
use crate::discover::models::SectionCondition;

/// The most picks one seed is the strongest path of.
pub const SEED_PICKS: usize = 2;

/// One pick of the person's that was shown, as memory reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    /// Its release group, lowercased.
    pub release_group: String,
    /// Its artist, lowercased.
    pub artist: String,
    /// When it was last shown, unix ms.
    pub last_shown_at: i64,
    /// Whether the person sampled it.
    pub sampled: bool,
    /// Whether the person asked for it.
    pub requested: bool,
}

/// What rests: albums shown and passed over, and their artists once they
/// have enough of them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Exposure {
    groups: HashSet<String>,
    artists: HashSet<String>,
}

impl Exposure {
    /// What rests at `now_ms` after `shown`.
    pub fn from_shown(shown: &[Shown], now_ms: i64) -> Self {
        let mut exposure = Self::default();
        let mut per_artist: HashMap<&str, usize> = HashMap::new();
        for pick in shown {
            if pick.sampled || pick.requested || pick.last_shown_at < now_ms - proposed::REST_MS {
                continue;
            }
            if exposure.groups.insert(pick.release_group.clone()) && !pick.artist.is_empty() {
                *per_artist.entry(pick.artist.as_str()).or_default() += 1;
            }
        }
        exposure.artists = per_artist
            .into_iter()
            .filter(|(_, albums)| *albums >= proposed::REST_ARTIST_ALBUMS)
            .map(|(artist, _)| artist.to_owned())
            .collect();
        exposure
    }

    /// Whether the album `release_group` rests.
    pub fn rests_group(&self, release_group: &str) -> bool {
        self.groups.contains(release_group)
    }

    /// Whether the artist `mbid` rests.
    pub fn rests_artist(&self, mbid: &str) -> bool {
        self.artists.contains(mbid)
    }

    /// Whether `candidate` rests, by its album or its artist.
    pub fn rests(&self, candidate: &Candidate) -> bool {
        self.rests_group(&candidate.release_group_mbid) || self.rests_artist(&candidate.artist_mbid)
    }
}

/// The least listeners an artist and an album need to be selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Floors {
    /// The artist's.
    pub artist_listeners: i64,
    /// The album's.
    pub album_listeners: i64,
}

impl Floors {
    /// The floors this engine starts with.
    pub const PROPOSED: Self = Self {
        artist_listeners: proposed::ARTIST_FLOOR,
        album_listeners: proposed::ALBUM_FLOOR,
    };

    /// The floors at `depth`: [`proposed::DEPTH_FLOOR_STEP`] times
    /// [`Floors::PROPOSED`] at home ground, that part of them far out (never
    /// zero), and those floors next door.
    pub fn for_depth(depth: Depth) -> Self {
        let step = proposed::DEPTH_FLOOR_STEP;
        match depth {
            Depth::HomeGround => Self {
                artist_listeners: proposed::ARTIST_FLOOR * step,
                album_listeners: proposed::ALBUM_FLOOR * step,
            },
            Depth::NextDoor => Self::PROPOSED,
            Depth::FarOut => Self {
                artist_listeners: (proposed::ARTIST_FLOOR / step).max(1),
                album_listeners: (proposed::ALBUM_FLOOR / step).max(1),
            },
        }
    }

    /// Whether `candidate` clears both; a count not known clears its floor.
    pub fn clears(&self, candidate: &Candidate) -> bool {
        candidate
            .artist_listeners
            .is_none_or(|count| count >= self.artist_listeners)
            && candidate
                .album_listeners
                .is_none_or(|count| count >= self.album_listeners)
    }
}

/// One taste group's share of the person's taste.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Share {
    /// The group.
    pub group: i64,
    /// Its share.
    pub share: f64,
}

/// What [`select`] answered.
#[derive(Debug, Clone, PartialEq)]
pub struct Selection<'a> {
    /// The picks, in the pool's order.
    pub picks: Vec<&'a Candidate>,
    /// Why there are fewer than were asked for, when there are.
    pub short: Option<SectionCondition>,
}

/// How many slots each of `shares` (in share order) gets of `n` on the
/// first pass: largest remainder, at most [`proposed::FIRST_PASS_CAP`] each.
fn quotas(shares: &[Share], n: usize) -> Vec<usize> {
    let total: f64 = shares.iter().map(|share| share.share.max(0.0)).sum();
    if total <= 0.0 || n == 0 {
        return vec![0; shares.len()];
    }
    let exact: Vec<f64> = shares
        .iter()
        .map(|share| n as f64 * share.share.max(0.0) / total)
        .collect();
    let mut quotas: Vec<usize> = exact
        .iter()
        .map(|exact| (exact.floor() as usize).min(proposed::FIRST_PASS_CAP))
        .collect();
    let mut by_remainder: Vec<usize> = (0..shares.len()).collect();
    by_remainder.sort_by(|a, b| {
        let remainder = |at: usize| exact[at] - exact[at].floor();
        remainder(*b).total_cmp(&remainder(*a)).then(a.cmp(b))
    });
    let mut left = n.saturating_sub(quotas.iter().sum());
    for at in by_remainder {
        if left == 0 {
            break;
        }
        if quotas[at] < proposed::FIRST_PASS_CAP && exact[at] > quotas[at] as f64 {
            quotas[at] += 1;
            left -= 1;
        }
    }
    quotas
}

/// The picks so far and what variety allows.
struct Dealt<'a> {
    picks: Vec<(usize, &'a Candidate)>,
    taken: HashSet<usize>,
    artists: HashSet<&'a str>,
    seeds: HashMap<&'a str, usize>,
}

impl<'a> Dealt<'a> {
    /// Take `candidate` (at `at` in the pool) when variety allows.
    fn take(&mut self, at: usize, candidate: &'a Candidate) -> bool {
        if self.taken.contains(&at) || self.artists.contains(candidate.artist_mbid.as_str()) {
            return false;
        }
        let seed = candidate.strongest_seed();
        if let Some(seed) = seed
            && self.seeds.get(seed).copied().unwrap_or(0) >= SEED_PICKS
        {
            return false;
        }
        self.taken.insert(at);
        self.artists.insert(candidate.artist_mbid.as_str());
        if let Some(seed) = seed {
            *self.seeds.entry(seed).or_default() += 1;
        }
        self.picks.push((at, candidate));
        true
    }
}

/// Up to `n` picks from `pool` (in the pool's order), slots shared by
/// `shares`, leaving out what `exposure` rests and what is under `floors`.
pub fn select<'a>(
    pool: &'a [Candidate],
    n: usize,
    shares: &[Share],
    exposure: &Exposure,
    floors: Floors,
) -> Selection<'a> {
    let mut shares: Vec<Share> = shares.to_vec();
    shares.sort_by(|a, b| b.share.total_cmp(&a.share).then(a.group.cmp(&b.group)));
    let eligible: Vec<(usize, &Candidate)> = pool
        .iter()
        .enumerate()
        .filter(|(_, candidate)| floors.clears(candidate) && !exposure.rests(candidate))
        .collect();
    let of_group = |group: Option<i64>| {
        eligible
            .iter()
            .filter(move |(_, candidate)| candidate.group == group)
            .copied()
    };
    let mut dealt = Dealt {
        picks: Vec::new(),
        taken: HashSet::new(),
        artists: HashSet::new(),
        seeds: HashMap::new(),
    };
    let fill = |dealt: &mut Dealt<'a>, group: Option<i64>, want: usize| {
        let mut got = 0;
        for (at, candidate) in of_group(group) {
            if got >= want {
                break;
            }
            if dealt.take(at, candidate) {
                got += 1;
            }
        }
        got
    };

    for (share, quota) in shares.iter().zip(quotas(&shares, n)) {
        fill(&mut dealt, Some(share.group), quota);
    }
    // Round again in share order, one slot a group a turn, without the cap.
    loop {
        let mut grew = false;
        for share in &shares {
            if dealt.picks.len() >= n {
                break;
            }
            grew |= fill(&mut dealt, Some(share.group), 1) > 0;
        }
        if !grew || dealt.picks.len() >= n {
            break;
        }
    }
    // Then what is in no group, or in a group no longer listed.
    let listed: HashSet<i64> = shares.iter().map(|share| share.group).collect();
    for (at, candidate) in &eligible {
        if dealt.picks.len() >= n {
            break;
        }
        if candidate.group.is_none_or(|group| !listed.contains(&group)) {
            dealt.take(*at, candidate);
        }
    }

    dealt.picks.sort_by_key(|(at, _)| *at);
    let picks: Vec<&Candidate> = dealt
        .picks
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect();
    let short = if picks.len() >= n {
        None
    } else if picks.is_empty() {
        Some(SectionCondition::Empty)
    } else {
        Some(SectionCondition::Thin)
    };
    Selection { picks, short }
}

#[cfg(test)]
mod tests {
    use super::super::candidate::{CandidateKind, Link, Path, PathArtist, PathKind, PathSource};
    use super::super::rank::{AlbumGates, AlbumList, ArtistGates, RankedArtist, Walk, order};
    use super::*;
    use crate::discover::feedback::Feedback;
    use crate::discover::listening::{AlbumFacts, AlbumRow, RankedAlbum};

    const WEEK: i64 = 7 * 86_400_000;

    fn seed_path(seed: &str, strength: f64) -> Path {
        Path {
            kind: PathKind::Seed,
            source: PathSource::ListenbrainzLabs,
            fetched_at: None,
            artist: Some(PathArtist {
                mbid: seed.to_owned(),
                name: seed.to_owned(),
                plays: 1,
                weight: 1.0,
                albums_owned: 0,
            }),
            link: Some(Link {
                strength,
                rank: 1,
                of: 100,
                labs: true,
                lastfm: false,
                closest_new: false,
            }),
            known_credit: None,
            fact: None,
        }
    }

    fn candidate(
        album: &str,
        artist: &str,
        seed: &str,
        group: Option<i64>,
        support: f64,
    ) -> Candidate {
        Candidate {
            release_group_mbid: album.to_owned(),
            title: album.to_owned(),
            artist_mbid: artist.to_owned(),
            artist_name: artist.to_owned(),
            credited_artist_mbids: vec![artist.to_owned()],
            primary_type: Some("Album".to_owned()),
            first_release_date: None,
            album_listeners: Some(1_000),
            artist_listeners: Some(10_000),
            kind: CandidateKind::NewArtist,
            paths: vec![seed_path(seed, support)],
            support,
            group,
            group_support: support,
        }
    }

    /// `per_group` candidates in each of `groups`, every one by its own
    /// artist and seed.
    fn pool(groups: &[i64], per_group: usize) -> Vec<Candidate> {
        let mut pool: Vec<Candidate> = groups
            .iter()
            .flat_map(|group| {
                (0..per_group).map(move |n| {
                    candidate(
                        &format!("g{group}-album{n}"),
                        &format!("g{group}-artist{n}"),
                        &format!("g{group}-seed{n}"),
                        Some(*group),
                        1.0 / (n + 1) as f64,
                    )
                })
            })
            .collect();
        pool.sort_by(|a, b| {
            b.support
                .total_cmp(&a.support)
                .then(a.artist_mbid.cmp(&b.artist_mbid))
        });
        pool
    }

    fn stage_zero_shares() -> Vec<Share> {
        [0.35, 0.21, 0.21, 0.11, 0.05]
            .into_iter()
            .enumerate()
            .map(|(group, share)| Share {
                group: group as i64,
                share,
            })
            .collect()
    }

    fn per_group(selection: &Selection<'_>) -> Vec<usize> {
        (0..5)
            .map(|group| {
                selection
                    .picks
                    .iter()
                    .filter(|pick| pick.group == Some(group))
                    .count()
            })
            .collect()
    }

    #[test]
    fn select_shares_35_21_21_11_5_over_8_give_2_2_2_1_1() {
        let pool = pool(&[0, 1, 2, 3, 4], 10);
        let chosen = select(
            &pool,
            8,
            &stage_zero_shares(),
            &Exposure::default(),
            Floors::PROPOSED,
        );
        assert_eq!(per_group(&chosen), [2, 2, 2, 1, 1]);
        assert_eq!(chosen.short, None);
    }

    #[test]
    fn select_one_group_fills_all_8() {
        let pool = pool(&[0], 20);
        let shares = [Share {
            group: 0,
            share: 1.0,
        }];
        let chosen = select(&pool, 8, &shares, &Exposure::default(), Floors::PROPOSED);
        assert_eq!(chosen.picks.len(), 8);
        assert_eq!(chosen.short, None);
    }

    #[test]
    fn select_a_pool_of_5_answers_5_with_a_reason() {
        let pool = pool(&[0], 5);
        let shares = [Share {
            group: 0,
            share: 1.0,
        }];
        let chosen = select(&pool, 8, &shares, &Exposure::default(), Floors::PROPOSED);
        assert_eq!(chosen.picks.len(), 5, "never padded");
        assert_eq!(chosen.short, Some(SectionCondition::Thin));
        let none = select(&[], 8, &shares, &Exposure::default(), Floors::PROPOSED);
        assert_eq!(none.short, Some(SectionCondition::Empty));
    }

    #[test]
    fn select_one_album_an_artist() {
        let mut pool = pool(&[0], 3);
        pool.push(candidate(
            "second-album",
            "g0-artist0",
            "g0-seed9",
            Some(0),
            0.9,
        ));
        let shares = [Share {
            group: 0,
            share: 1.0,
        }];
        let chosen = select(&pool, 8, &shares, &Exposure::default(), Floors::PROPOSED);
        let artists: HashSet<&str> = chosen
            .picks
            .iter()
            .map(|pick| pick.artist_mbid.as_str())
            .collect();
        assert_eq!(artists.len(), chosen.picks.len());
        assert!(
            chosen
                .picks
                .iter()
                .all(|pick| pick.release_group_mbid != "second-album")
        );
    }

    #[test]
    fn select_a_seed_is_behind_at_most_two() {
        let pool: Vec<Candidate> = (0..6)
            .map(|n| {
                candidate(
                    &format!("album{n}"),
                    &format!("artist{n}"),
                    "one-seed",
                    Some(0),
                    1.0,
                )
            })
            .collect();
        let shares = [Share {
            group: 0,
            share: 1.0,
        }];
        let chosen = select(&pool, 8, &shares, &Exposure::default(), Floors::PROPOSED);
        assert_eq!(chosen.picks.len(), SEED_PICKS);
        assert_eq!(chosen.short, Some(SectionCondition::Thin));
    }

    #[test]
    fn select_a_pick_shown_and_passed_over_is_rested() {
        let pool = pool(&[0], 3);
        let shares = [Share {
            group: 0,
            share: 1.0,
        }];
        let now = 100 * WEEK;
        let shown = |sampled: bool| Shown {
            release_group: "g0-album0".to_owned(),
            artist: "g0-artist0".to_owned(),
            last_shown_at: now - WEEK,
            sampled,
            requested: false,
        };
        let passed = Exposure::from_shown(&[shown(false)], now);
        let chosen = select(&pool, 8, &shares, &passed, Floors::PROPOSED);
        assert!(
            chosen
                .picks
                .iter()
                .all(|pick| pick.release_group_mbid != "g0-album0")
        );
        let sampled = Exposure::from_shown(&[shown(true)], now);
        let chosen = select(&pool, 8, &shares, &sampled, Floors::PROPOSED);
        assert!(
            chosen
                .picks
                .iter()
                .any(|pick| pick.release_group_mbid == "g0-album0")
        );
        let long_ago = Exposure::from_shown(
            &[Shown {
                last_shown_at: now - proposed::REST_MS - 1,
                ..shown(false)
            }],
            now,
        );
        assert!(
            !long_ago.rests_group("g0-album0"),
            "rested twelve weeks, no longer"
        );
    }

    #[test]
    fn select_a_candidate_under_a_floor_waits_until_the_floor_is_halved() {
        let mut quiet = candidate("quiet", "quiet-artist", "seed", Some(0), 1.0);
        quiet.artist_listeners = Some(proposed::ARTIST_FLOOR * 3 / 4);
        let pool = [quiet];
        let shares = [Share {
            group: 0,
            share: 1.0,
        }];
        let chosen = select(&pool, 8, &shares, &Exposure::default(), Floors::PROPOSED);
        assert!(chosen.picks.is_empty());
        let halved = Floors {
            artist_listeners: proposed::ARTIST_FLOOR / 2,
            album_listeners: proposed::ALBUM_FLOOR / 2,
        };
        let chosen = select(&pool, 8, &shares, &Exposure::default(), halved);
        assert_eq!(chosen.picks.len(), 1);
    }

    /// Six weeks of a person answering their whole set and thirty extras
    /// each week, over a ranking of realistic size: every week's pool is
    /// built again through the walk, and every set is full, with nothing
    /// dealt twice.
    #[test]
    fn select_six_weeks_of_answering_everything_deal_six_full_sets_with_no_repeat() {
        let seeds = 23;
        let shares = stage_zero_shares();
        // 1,200 artists over five groups, each found by one of the seeds
        // of its group, with four albums each.
        let artists: Vec<RankedArtist> = (0..1_200)
            .map(|n: usize| {
                let group = n % 5;
                let seed = format!("seed{}", group + 5 * ((n / 5) % (seeds / 5 + 1)));
                RankedArtist {
                    mbid: format!("artist{n:04}"),
                    name: format!("Artist {n}"),
                    listeners: Some(5_000),
                    support: 1.0 / (n + 1) as f64,
                    group: Some(group as i64),
                    group_support: 1.0 / (n + 1) as f64,
                    paths: vec![seed_path(&seed, 1.0 / (n + 1) as f64)],
                }
            })
            .collect();
        let albums: HashMap<String, Vec<RankedAlbum>> = artists
            .iter()
            .map(|artist| {
                let rows = (0..4)
                    .map(|m| RankedAlbum {
                        album: AlbumRow {
                            release_group_mbid: format!("{}-album{m}", artist.mbid),
                            title: format!("Album {m}"),
                            artist_name: artist.name.clone(),
                            artist_mbid: Some(artist.mbid.clone()),
                            facts: AlbumFacts {
                                primary_type: Some("Album".to_owned()),
                                secondary_types: Some(Vec::new()),
                                first_release_date: Some("2020".to_owned()),
                                listeners: Some(1_000 - m),
                                credited_artist_mbids: Some(vec![artist.mbid.clone()]),
                            },
                        },
                        listen_count: 0,
                    })
                    .collect();
                (artist.mbid.clone(), rows)
            })
            .collect();
        let order_of = super::super::rank::walk_order(
            &artists,
            &shares
                .iter()
                .map(|share| (share.group, share.share))
                .collect::<Vec<_>>(),
        );
        let (known, owned, played, fetching) = (
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
            HashSet::new(),
        );
        let feedback = Feedback::default();
        let artist_gates = ArtistGates {
            known: &known,
            library: &owned,
            feedback: &feedback,
        };

        let mut shown: Vec<Shown> = Vec::new();
        let mut dealt: HashSet<String> = HashSet::new();
        for week in 1..=6 {
            let now = week * WEEK;
            let exposure = Exposure::from_shown(&shown, now);
            let gates = AlbumGates {
                owned: &owned,
                fetching: &fetching,
                played: &played,
                feedback: &feedback,
                exposure: &exposure,
                artists: &artist_gates,
            };
            let mut walk = Walk::new(proposed::WALK_ARTISTS);
            for at in &order_of {
                if walk.done() {
                    break;
                }
                let artist = &artists[*at];
                walk.step(artist, AlbumList::Found(&albums[&artist.mbid]), &gates);
            }
            let pool = order(walk.candidates, Depth::NextDoor);
            let set = select(&pool, 8, &shares, &exposure, Floors::PROPOSED);
            assert_eq!(set.picks.len(), 8, "week {week}'s set is full");
            let mut answered: Vec<&Candidate> = set.picks.clone();
            // The extras are dealt from what the set left.
            let after_set = Exposure::from_shown(
                &shown
                    .iter()
                    .cloned()
                    .chain(set.picks.iter().map(|pick| passed(pick, now)))
                    .collect::<Vec<_>>(),
                now,
            );
            let extras = select(&pool, 30, &shares, &after_set, Floors::PROPOSED);
            assert_eq!(extras.picks.len(), 30, "week {week}'s extras");
            answered.extend(extras.picks.iter().copied());
            for pick in answered {
                assert!(
                    dealt.insert(pick.release_group_mbid.clone()),
                    "{} dealt twice (week {week})",
                    pick.release_group_mbid
                );
                shown.push(passed(pick, now));
            }
        }
        assert_eq!(dealt.len(), 6 * 38);
    }

    fn passed(pick: &Candidate, now: i64) -> Shown {
        Shown {
            release_group: pick.release_group_mbid.clone(),
            artist: pick.artist_mbid.clone(),
            last_shown_at: now,
            sampled: false,
            requested: false,
        }
    }
}
