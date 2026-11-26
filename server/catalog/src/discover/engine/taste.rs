//! A person's tastes, worked out from their seeds and the links between
//! them. Pure: the job hands it the seeds and the kept similar-artist
//! answers between them, and keeps what it answers.
//!
//! A link from one seed to another is the second's score in the first's
//! similar-artists list (co-listening with fame divided out, best 1.0), and
//! two seeds' strength is the larger of their two links. Seeds are walked
//! heaviest first: a seed in no group starts one, which takes in every
//! seed with a strength of [`LINK_MIN`] or more to one of its members,
//! until none joins. A group's share is its seeds' weight over all seed
//! weight. The [`MAX_GROUPS`] largest are kept, none under [`SHARE_FLOOR`];
//! the rest of the seeds are ungrouped. Nothing is split or padded to reach
//! a count. Each group is named after its anchor, its heaviest seed, and
//! nothing else.

use std::collections::{BTreeMap, HashMap, HashSet};

/// The least strength that joins two seeds in a group (proposed).
pub const LINK_MIN: f64 = 0.15;
/// The most groups kept.
pub const MAX_GROUPS: usize = 8;
/// The least share a kept group has (proposed).
pub const SHARE_FLOOR: f64 = 0.03;
/// How far back a group's recent share reads, in days.
pub const RECENT_DAYS: i64 = 90;
/// The fewest plays, all time and in the last [`RECENT_DAYS`], among the
/// seeds before a trend is read (proposed): with fewer every group is
/// steady.
pub const TREND_MIN_PLAYS: i64 = 10;
/// A recent play share this many times the group's play share of all time
/// or more is rising (proposed)...
pub const RISING: f64 = 1.25;
/// ...and this many times or less is fading (proposed).
pub const FADING: f64 = 0.75;

/// Whether a group is played more or less of late than it is all told.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Trend {
    /// Its share of the last [`RECENT_DAYS`] of plays is well above its
    /// share of all of them.
    Rising,
    /// About the same, or too few plays to tell.
    Steady,
    /// Well below.
    Fading,
}

droppedneedle_kernel::spelled!(Trend {
    Rising => "rising",
    Steady => "steady",
    Fading => "fading",
});

/// One seed, as grouping reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct TasteSeed {
    /// Its MusicBrainz id, lowercased.
    pub mbid: String,
    /// Its name.
    pub name: String,
    /// Its weight.
    pub weight: f64,
    /// The person's plays and imported listens of it.
    pub plays: i64,
    /// Those in the last [`RECENT_DAYS`].
    pub recent: i64,
}

/// The links between seeds: each seed's list, by the seed it points at.
#[derive(Debug, Default)]
pub struct Links(HashMap<String, HashMap<String, f64>>);

impl Links {
    /// `from`'s list puts `to` at `strength`.
    pub fn add(&mut self, from: &str, to: &str, strength: f64) {
        if from != to {
            self.0
                .entry(from.to_owned())
                .or_default()
                .insert(to.to_owned(), strength);
        }
    }

    fn link(&self, from: &str, to: &str) -> f64 {
        self.0
            .get(from)
            .and_then(|list| list.get(to))
            .copied()
            .unwrap_or(0.0)
    }

    /// Two seeds' strength: the larger of their two links.
    pub fn strength(&self, a: &str, b: &str) -> f64 {
        self.link(a, b).max(self.link(b, a))
    }
}

/// One kept group.
#[derive(Debug, Clone, PartialEq)]
pub struct Group {
    /// Its heaviest seed, which names it.
    pub anchor: String,
    /// The anchor's name: the group is shown as "Around {anchor_name}".
    pub anchor_name: String,
    /// Its seeds, heaviest first.
    pub members: Vec<String>,
    /// Its seeds' weight over all seed weight.
    pub share: f64,
    /// Its seeds' share of the seeds' plays in the last [`RECENT_DAYS`].
    pub recent_share: f64,
    /// That share against its share of all their plays.
    pub trend: Trend,
}

/// A group's recent play share against its play share of all time;
/// steady while `enough` says the seeds' plays are too few to tell.
fn trend(play_share: f64, recent_share: f64, enough: bool) -> Trend {
    if !enough || play_share <= 0.0 {
        return Trend::Steady;
    }
    let ratio = recent_share / play_share;
    if ratio >= RISING {
        Trend::Rising
    } else if ratio <= FADING {
        Trend::Fading
    } else {
        Trend::Steady
    }
}

/// The person's groups, largest share first: at most [`MAX_GROUPS`], none
/// under [`SHARE_FLOOR`]. A seed in no group answered is ungrouped.
pub fn groups(seeds: &[TasteSeed], links: &Links) -> Vec<Group> {
    let mut order: Vec<&TasteSeed> = seeds.iter().collect();
    order.sort_by(|a, b| b.weight.total_cmp(&a.weight).then(a.mbid.cmp(&b.mbid)));
    let mut seen: HashSet<&str> = HashSet::new();
    let mut walked: Vec<Vec<&TasteSeed>> = Vec::new();
    for seed in &order {
        if !seen.insert(&seed.mbid) {
            continue;
        }
        let mut group = vec![*seed];
        let mut grew = true;
        while grew {
            grew = false;
            for other in &order {
                if seen.contains(other.mbid.as_str()) {
                    continue;
                }
                let joins = group
                    .iter()
                    .any(|member| links.strength(&other.mbid, &member.mbid) >= LINK_MIN);
                if joins {
                    seen.insert(&other.mbid);
                    group.push(*other);
                    grew = true;
                }
            }
        }
        walked.push(group);
    }

    let total: f64 = seeds.iter().map(|seed| seed.weight).sum();
    let plays_total: i64 = seeds.iter().map(|seed| seed.plays.max(0)).sum();
    let recent_total: i64 = seeds.iter().map(|seed| seed.recent.max(0)).sum();
    let enough = plays_total >= TREND_MIN_PLAYS && recent_total >= TREND_MIN_PLAYS;
    let part = |count: i64, of: i64| {
        if of > 0 {
            count as f64 / of as f64
        } else {
            0.0
        }
    };
    if total <= 0.0 {
        return Vec::new();
    }
    let mut groups: Vec<Group> = walked
        .into_iter()
        .map(|members| {
            let weight: f64 = members.iter().map(|seed| seed.weight).sum();
            let plays: i64 = members.iter().map(|seed| seed.plays.max(0)).sum();
            let recent: i64 = members.iter().map(|seed| seed.recent.max(0)).sum();
            let share = weight / total;
            let recent_share = part(recent, recent_total);
            Group {
                anchor: members[0].mbid.clone(),
                anchor_name: members[0].name.clone(),
                members: members.iter().map(|seed| seed.mbid.clone()).collect(),
                share,
                recent_share,
                trend: trend(part(plays, plays_total), recent_share, enough),
            }
        })
        .filter(|group| group.share >= SHARE_FLOOR)
        .collect();
    // Walked heaviest anchor first, and the sort is stable: equal shares
    // keep that order.
    groups.sort_by(|a, b| b.share.total_cmp(&a.share));
    groups.truncate(MAX_GROUPS);
    groups
}

/// `items` (heaviest first) in turns across their groups by weight, as
/// seats are shared out by votes (Sainte-Laguë): each turn goes to the
/// group whose weight over twice its items so far, plus one, is largest,
/// earlier groups first on a tie. An item in no group is a group of its
/// own. So the first few of a long list speak for every taste, larger ones
/// more often.
pub fn spread<T>(
    items: Vec<T>,
    weight: impl Fn(&T) -> f64,
    group: impl Fn(&T) -> Option<i64>,
) -> Vec<T> {
    let mut buckets: Vec<(f64, std::collections::VecDeque<T>)> = Vec::new();
    let mut by_group: BTreeMap<i64, usize> = BTreeMap::new();
    for item in items {
        let at = match group(&item) {
            Some(no) => *by_group.entry(no).or_insert_with(|| {
                buckets.push((0.0, std::collections::VecDeque::new()));
                buckets.len() - 1
            }),
            None => {
                buckets.push((0.0, std::collections::VecDeque::new()));
                buckets.len() - 1
            }
        };
        buckets[at].0 += weight(&item).max(0.0);
        buckets[at].1.push_back(item);
    }
    let mut taken = vec![0usize; buckets.len()];
    let mut spread = Vec::new();
    loop {
        let mut best: Option<(usize, f64)> = None;
        for (at, (total, queue)) in buckets.iter().enumerate() {
            if queue.is_empty() {
                continue;
            }
            let turn = total / (2 * taken[at] + 1) as f64;
            if best.is_none_or(|(_, best)| turn > best) {
                best = Some((at, turn));
            }
        }
        let Some((at, _)) = best else {
            break;
        };
        taken[at] += 1;
        if let Some(item) = buckets[at].1.pop_front() {
            spread.push(item);
        }
    }
    spread
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed(mbid: &str, weight: f64, plays: i64, recent: i64) -> TasteSeed {
        TasteSeed {
            mbid: mbid.to_owned(),
            name: mbid.to_uppercase(),
            weight,
            plays,
            recent,
        }
    }

    /// Two circles of artists who share listeners, joined only by a weak
    /// link, and one artist nobody shares.
    fn history() -> (Vec<TasteSeed>, Links) {
        let seeds = vec![
            seed("prog-a", 5.0, 30, 2),
            seed("prog-b", 3.0, 20, 1),
            seed("prog-c", 2.0, 10, 0),
            seed("noise-a", 4.0, 15, 10),
            seed("noise-b", 2.0, 10, 6),
            seed("loner", 1.0, 5, 0),
        ];
        let mut links = Links::default();
        links.add("prog-a", "prog-b", 0.9);
        // One way is enough: the larger of the two links counts.
        links.add("prog-c", "prog-b", 0.4);
        links.add("noise-b", "noise-a", 0.6);
        // Under the threshold: the circles stay apart.
        links.add("prog-a", "noise-a", 0.14);
        links.add("noise-a", "prog-a", 0.1);
        (seeds, links)
    }

    #[test]
    fn taste_two_circles_make_two_groups_named_after_their_heaviest() {
        let (seeds, links) = history();
        let groups = groups(&seeds, &links);
        let members: Vec<(&str, Vec<&str>)> = groups
            .iter()
            .map(|group| {
                (
                    group.anchor_name.as_str(),
                    group.members.iter().map(String::as_str).collect(),
                )
            })
            .collect();
        assert_eq!(
            members,
            [
                ("PROG-A", vec!["prog-a", "prog-b", "prog-c"]),
                ("NOISE-A", vec!["noise-a", "noise-b"]),
                ("LONER", vec!["loner"]),
            ]
        );
    }

    #[test]
    fn taste_shares_sum_to_the_grouped_weight() {
        let (seeds, links) = history();
        let groups = groups(&seeds, &links);
        let shares: f64 = groups.iter().map(|group| group.share).sum();
        assert!((shares - 1.0).abs() < 1e-9, "{shares}");
        assert!((groups[0].share - 10.0 / 17.0).abs() < 1e-9);
        assert!((groups[1].share - 6.0 / 17.0).abs() < 1e-9);
    }

    #[test]
    fn taste_trend_compares_recent_plays_with_all_plays() {
        let (mut seeds, links) = history();
        // Most of the last 90 days went to the second circle.
        let trends: Vec<Trend> = groups(&seeds, &links).iter().map(|g| g.trend).collect();
        assert_eq!(trends, [Trend::Fading, Trend::Rising, Trend::Fading]);
        // A followed artist hardly played reads steady, not fading.
        seeds.push(seed("followed", 6.0, 0, 0));
        let followed = groups(&seeds, &links)
            .into_iter()
            .find(|group| group.anchor == "followed")
            .expect("a group of its own");
        assert_eq!(followed.trend, Trend::Steady);
        // Too few recent plays to tell: every group is steady.
        for seed in &mut seeds {
            seed.recent = seed.recent.min(1);
        }
        let quiet = groups(&seeds, &links);
        assert!(quiet.iter().all(|group| group.trend == Trend::Steady));
    }

    #[test]
    fn taste_a_link_under_the_threshold_joins_nothing() {
        let (seeds, mut links) = history();
        assert_eq!(groups(&seeds, &links).len(), 3);
        links.add("prog-a", "noise-a", LINK_MIN);
        let joined = groups(&seeds, &links);
        assert_eq!(joined.len(), 2, "at the threshold the circles join");
        assert_eq!(joined[0].members.len(), 5);
    }

    #[test]
    fn taste_a_group_under_the_floor_stays_ungrouped() {
        let (mut seeds, links) = history();
        // The loner's share drops to 1 in 40, under 3%.
        seeds.push(seed("filler", 23.0, 0, 0));
        let groups = groups(&seeds, &links);
        assert!(
            groups.iter().all(|group| group.anchor != "loner"),
            "{groups:?}"
        );
        assert!(groups.iter().all(|group| group.share >= SHARE_FLOOR));
        let grouped: f64 = groups.iter().map(|group| group.share).sum();
        assert!((grouped - 39.0 / 40.0).abs() < 1e-9, "{grouped}");
    }

    #[test]
    fn taste_seeds_take_turns_across_groups_by_share() {
        let items = vec![
            ("a1", 4.0, Some(0)),
            ("a2", 3.0, Some(0)),
            ("a3", 3.0, Some(0)),
            ("b1", 3.5, Some(1)),
            ("c1", 2.0, None),
        ];
        let order: Vec<&str> = spread(items, |item| item.1, |item| item.2)
            .into_iter()
            .map(|item| item.0)
            .collect();
        assert_eq!(order, ["a1", "b1", "a2", "a3", "c1"]);
    }
}
