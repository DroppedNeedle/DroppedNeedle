//! Pure deck assembly: shuffling, round-robin selection across candidate
//! pools, and wildcard placement.

use std::collections::{HashMap, HashSet};

use crate::discover::models::QueueItemLight;

/// Most cards one artist may hold in the personalised part of a deck.
pub const MAX_PER_ARTIST: usize = 2;
/// Where wildcards land in the deck (third and eighth card), so the
/// out-of-profile picks are spread instead of bunched at the end.
pub const WILDCARD_POSITIONS: [usize; 2] = [2, 7];

/// A small xorshift generator. Decks only need to vary between builds, so
/// a seed from a fresh UUID is plenty; tests pass a fixed seed.
#[derive(Debug, Clone)]
pub struct Shuffler {
    state: u64,
}

impl Shuffler {
    /// A generator seeded from a fresh random UUID.
    pub fn random() -> Self {
        let bits = uuid::Uuid::new_v4().as_u128();
        Self::seeded((bits as u64) ^ ((bits >> 64) as u64))
    }

    /// A generator with a fixed seed (zero is replaced, xorshift needs a
    /// set bit).
    pub fn seeded(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    fn next(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    /// Fisher-Yates shuffle in place.
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for index in (1..items.len()).rev() {
            let pick = (self.next() % (index as u64 + 1)) as usize;
            items.swap(index, pick);
        }
    }
}

/// Take `count` cards round-robin across shuffled pools, one per pool per
/// pass, skipping release groups already taken and artists already at
/// `max_per_artist`.
pub fn round_robin(
    mut pools: Vec<Vec<QueueItemLight>>,
    count: usize,
    max_per_artist: usize,
    shuffler: &mut Shuffler,
) -> Vec<QueueItemLight> {
    for pool in &mut pools {
        shuffler.shuffle(pool);
    }
    let mut cursors = vec![0usize; pools.len()];
    let mut picked: Vec<QueueItemLight> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut per_artist: HashMap<String, usize> = HashMap::new();
    for _ in 0..count.saturating_mul(3) {
        if picked.len() >= count {
            break;
        }
        for (pool, cursor) in pools.iter().zip(cursors.iter_mut()) {
            if picked.len() >= count {
                break;
            }
            while let Some(item) = pool.get(*cursor) {
                *cursor += 1;
                let key = item.musicbrainz_id.to_ascii_lowercase();
                let artist = item.artist_id.to_ascii_lowercase();
                if seen.contains(&key) {
                    continue;
                }
                if !artist.is_empty()
                    && per_artist.get(&artist).copied().unwrap_or(0) >= max_per_artist
                {
                    continue;
                }
                seen.insert(key);
                if !artist.is_empty() {
                    *per_artist.entry(artist).or_insert(0) += 1;
                }
                picked.push(item.clone());
                break;
            }
        }
    }
    picked
}

/// Insert `wildcards` into `base` at `positions`, each position counted in
/// the growing deck; extras go to the end.
pub fn interleave(
    base: Vec<QueueItemLight>,
    wildcards: Vec<QueueItemLight>,
    positions: &[usize],
) -> Vec<QueueItemLight> {
    let mut deck = base;
    for (index, wildcard) in wildcards.into_iter().enumerate() {
        let at = positions
            .get(index)
            .copied()
            .unwrap_or(deck.len())
            .min(deck.len());
        deck.insert(at, wildcard);
    }
    deck
}
