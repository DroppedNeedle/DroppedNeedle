//! A station by sound: the library tracks that sound like its seed tracks,
//! drawn round the seeds in turn and walked ([`super::walk`]).
//!
//! The search itself is the library's and knows nothing of a person; what
//! is the person's is here. A candidate that shares none of its seed's five
//! strongest tags and whose artist is in none of the person's taste groups
//! the seed's artist is in sounds alike from outside the seed's genre: it is
//! pushed down by [`FENCE_PUSH`], not removed, unless the station asked for
//! sound alone. A track the person said "Not this" to never comes again,
//! and tracks that sound like it are pushed down by [`NOT_THIS_PUSH`] for
//! the station. A seed the person asked for more like takes
//! [`MORE_LIKE_WEIGHT`] turns a round.

use std::collections::{HashMap, HashSet};

use droppedneedle_library::sound::search::VERY_CLOSE_UNDER;
use droppedneedle_library::sound::{SoundIndex, SoundTrack};

use super::walk::{Step, walk};

/// Turns a round a seed the person asked for more like takes.
pub const MORE_LIKE_WEIGHT: usize = 2;
/// How far down a track from outside the seed's genre is pushed.
pub const FENCE_PUSH: f32 = 0.05;
/// How far down a track that sounds like one the person said "Not this"
/// to is pushed.
pub const NOT_THIS_PUSH: f32 = 0.10;
/// Tracks nearer than this to one the person said "Not this" to are
/// pushed down: those that sound very close to it.
pub const NOT_THIS_NEAR: f32 = VERY_CLOSE_UNDER;
/// Fewest tracks asked of the search for one seed.
const FEWEST_ASKED: usize = 10;
/// Candidates gathered for each track a chunk plays, so the walk has a
/// choice.
const CHOICE: usize = 2;
/// Most seeds one chunk searches: each search scans every vector.
pub const MOST_SEARCHED: usize = 40;

/// A track a station by sound grows from.
#[derive(Debug, Clone)]
pub struct Seed {
    /// The library track.
    pub track_id: String,
    /// Whether the person asked for more like it.
    pub more_like: bool,
}

/// What the person and the station's past say about this chunk.
pub struct Steering<'a> {
    /// Tracks never to take: the station's already, "Not this", rested.
    pub excluded: &'a HashSet<String>,
    /// Whether the person told Discover "Not this artist" of an artist, by
    /// MusicBrainz id; the station's own seed artists are left out of it.
    pub hidden_artist: &'a (dyn Fn(&str) -> bool + Sync),
    /// Tracks the person said "Not this" to.
    pub not_this: &'a [String],
    /// The library tracks the station has queued, in the order they play.
    pub played: &'a [String],
    /// Each artist's taste group, by MusicBrainz id lowercased.
    pub groups: &'a HashMap<String, i64>,
    /// Leave the genre fence off.
    pub sound_only: bool,
}

/// One track a station by sound plays.
#[derive(Debug, Clone)]
pub struct Found {
    /// The library track.
    pub track_id: String,
    /// The seed track that found it.
    pub seed: Seed,
    /// Its distance from that seed, as the search measured it.
    pub distance: f32,
}

/// Whether `candidate` sounds alike from outside `seed`'s genre: no tag
/// shared and no taste group shared.
fn fenced(
    index: &SoundIndex,
    seed: &str,
    candidate: &str,
    shared_tags: bool,
    groups: &HashMap<String, i64>,
) -> bool {
    if shared_tags {
        return false;
    }
    let group = |track: &str| {
        index
            .track(track)
            .and_then(|track| track.artist_mbid.as_deref())
            .and_then(|mbid| groups.get(mbid))
    };
    match (group(seed), group(candidate)) {
        (Some(seed), Some(candidate)) => seed != candidate,
        _ => true,
    }
}

/// One seed's candidates, nearest after its push downs first.
fn candidates(
    index: &SoundIndex,
    seed: &Seed,
    seed_ids: &HashSet<&str>,
    steering: &Steering<'_>,
    asked: usize,
) -> Vec<(Step, Found)> {
    // Tracks never to take are left out before the search counts, so a
    // long station keeps finding new ones.
    let skip = |track: &SoundTrack| {
        steering.excluded.contains(&track.track_id)
            || seed_ids.contains(track.track_id.as_str())
            || track
                .artist_mbid
                .as_deref()
                .is_some_and(|artist| (steering.hidden_artist)(artist))
    };
    let Some(found) = index.similar(&seed.track_id, asked, &skip) else {
        return Vec::new();
    };
    let mut list: Vec<(Step, Found)> = found
        .into_iter()
        .filter_map(|near| {
            let artist = index.track(&near.track_id)?.artist_key().to_owned();
            let mut to_seed = near.distance;
            if !steering.sound_only
                && fenced(
                    index,
                    &seed.track_id,
                    &near.track_id,
                    !near.shared_tags.is_empty(),
                    steering.groups,
                )
            {
                to_seed += FENCE_PUSH;
            }
            if steering.not_this.iter().any(|said| {
                index
                    .distance(said, &near.track_id)
                    .is_some_and(|apart| apart < NOT_THIS_NEAR)
            }) {
                to_seed += NOT_THIS_PUSH;
            }
            Some((
                Step {
                    track_id: near.track_id.clone(),
                    artist,
                    to_seed,
                },
                Found {
                    track_id: near.track_id,
                    seed: seed.clone(),
                    distance: near.distance,
                },
            ))
        })
        .collect();
    list.sort_by(|a, b| a.0.to_seed.total_cmp(&b.0.to_seed));
    list
}

/// Up to `count` tracks for the next chunk of a station grown from
/// `seeds`, in the order they play. Seeds the index does not hold are
/// passed over. A seed is searched only when its turn comes, so a chunk
/// searches about as many seeds as it needs candidates, and at most
/// [`MOST_SEARCHED`]; the station's own seeds start where the last chunk's
/// turns left off.
pub fn station(
    index: &SoundIndex,
    seeds: &[Seed],
    steering: &Steering<'_>,
    count: usize,
) -> Vec<Found> {
    let asked = count.max(FEWEST_ASKED);
    let seed_ids: HashSet<&str> = seeds.iter().map(|seed| seed.track_id.as_str()).collect();
    let (more_like, own): (Vec<&Seed>, Vec<&Seed>) = seeds
        .iter()
        .filter(|seed| index.track(&seed.track_id).is_some())
        .partition(|seed| seed.more_like);
    let turn = if own.is_empty() {
        0
    } else {
        steering.played.len() % own.len()
    };
    // More-like seeds take at most half the searches, so the station's own
    // seeds always have room.
    let more_like_room = MOST_SEARCHED.saturating_sub(own.len().min(MOST_SEARCHED / 2));
    let order: Vec<&Seed> = more_like
        .into_iter()
        .rev()
        .take(more_like_room)
        .chain(own[turn..].iter().copied())
        .chain(own[..turn].iter().copied())
        .take(MOST_SEARCHED)
        .collect();
    // Round the seeds in turn until there is a choice for every track.
    let wanted = count * CHOICE;
    let mut lists: Vec<Option<Vec<(Step, Found)>>> = vec![None; order.len()];
    let mut cursors = vec![0usize; order.len()];
    let mut taken: HashMap<String, Found> = HashMap::new();
    let mut steps: Vec<Step> = Vec::new();
    'rounds: loop {
        let mut moved = false;
        for (at, seed) in order.iter().enumerate() {
            let turns = if seed.more_like { MORE_LIKE_WEIGHT } else { 1 };
            let list = lists[at]
                .get_or_insert_with(|| candidates(index, seed, &seed_ids, steering, asked));
            for _ in 0..turns {
                while let Some((step, found)) = list.get(cursors[at]) {
                    cursors[at] += 1;
                    if taken.contains_key(&step.track_id) {
                        continue;
                    }
                    taken.insert(step.track_id.clone(), found.clone());
                    steps.push(step.clone());
                    moved = true;
                    break;
                }
            }
            if steps.len() >= wanted {
                break 'rounds;
            }
        }
        if !moved {
            break;
        }
    }
    let start = steering
        .played
        .iter()
        .rev()
        .find(|id| index.track(id).is_some())
        .or_else(|| own.first().or(order.first()).map(|seed| &seed.track_id))
        .map(String::as_str);
    let recent: Vec<String> = steering
        .played
        .iter()
        .filter_map(|id| Some(index.track(id)?.artist_key().to_owned()))
        .collect();
    walk(steps, start, &recent, count, |a, b| index.distance(a, b))
        .into_iter()
        .filter_map(|step| taken.remove(&step.track_id))
        .collect()
}
