//! Genre artwork: up to four covers from the user's own collection stand
//! for each genre tile, else a plain gradient.
//!
//! Only art the album artwork route can serve takes part (a folder image
//! or an embedded picture the scan recorded), so a collage never shows a
//! broken image, and choosing one never calls a provider. The albums with
//! the most tracks in the genre go first; inside a tier the order is a
//! stable shuffle keyed by genre and album, so a tile does not change from
//! visit to visit. Different artists and different images are preferred.
//!
//! A choice is remembered for a day under the genre's revision, which the
//! database bumps whenever an album's art or tags in that genre change. A
//! remembered choice is checked again before use: when one of its albums
//! lost its art or its file is gone, the genre is chosen again.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::library::{ArtCandidate, GenreCandidates, HomeLibrary};
use crate::reads::discover::adapters::page::library::fold;
use crate::reads::discover::models::{GenreArtwork, GenreArtworkAlbum};
use crate::reads::platform::artwork::cache::sha256_hex;
use crate::reads::platform::artwork::local::{LocalArt, read_local_art};

/// The selection rules' version, part of every artwork version.
pub const ALGORITHM_VERSION: &str = "v2";
/// Covers in one collage.
const COLLAGE_SIZE: usize = 4;
/// Genres per batch (the tiles a page shows).
const BATCH_MAX: usize = 20;
/// How long a choice is remembered.
const REMEMBER: Duration = Duration::from_secs(86_400);

/// Picks and remembers genre artwork.
#[derive(Debug)]
pub struct GenreArt {
    library: HomeLibrary,
    memo: Mutex<HashMap<(String, i64), (GenreArtwork, Instant)>>,
}

impl GenreArt {
    /// Choose from `library`.
    pub fn new(library: HomeLibrary) -> Self {
        Self {
            library,
            memo: Mutex::new(HashMap::new()),
        }
    }

    /// Artwork for each of the first 20 genres, keyed by the name asked
    /// for. A failed read leaves the tiles as gradients, with a log line.
    pub async fn batch(&self, genres: &[String]) -> HashMap<String, GenreArtwork> {
        let mut names: Vec<(String, String)> = Vec::new();
        for genre in genres {
            let folded = fold(genre);
            if folded.is_empty() || names.iter().any(|(_, seen)| *seen == folded) {
                continue;
            }
            names.push((genre.clone(), folded));
            if names.len() == BATCH_MAX {
                break;
            }
        }
        if names.is_empty() {
            return HashMap::new();
        }
        let asked: Vec<String> = names.iter().map(|(_, folded)| folded.clone()).collect();
        let rows = match self.library.art_candidates(&asked).await {
            Ok(rows) => rows,
            Err(cause) => {
                tracing::warn!(%cause, "genre artwork unreadable; showing gradients");
                return HashMap::new();
            }
        };
        let by_genre: HashMap<&str, &GenreCandidates> =
            rows.iter().map(|row| (row.folded.as_str(), row)).collect();
        let mut out = HashMap::new();
        for (name, folded) in &names {
            let Some(genre) = by_genre.get(folded.as_str()) else {
                continue;
            };
            out.insert(name.clone(), self.artwork(genre).await);
        }
        out
    }

    /// Artwork for one genre: a gradient when the genre has no art.
    pub async fn one(&self, genre: &str) -> GenreArtwork {
        let mut found = self.batch(&[genre.to_owned()]).await;
        found
            .remove(genre)
            .unwrap_or_else(|| artwork_of(0, Vec::new()))
    }

    async fn artwork(&self, genre: &GenreCandidates) -> GenreArtwork {
        let key = (genre.folded.clone(), genre.revision);
        let remembered = self.memo.lock().ok().and_then(|memo| {
            memo.get(&key)
                .filter(|(_, at)| at.elapsed() < REMEMBER)
                .map(|(artwork, _)| artwork.clone())
        });
        if let Some(artwork) = remembered
            && still_current(&artwork, &genre.candidates).await
        {
            return artwork;
        }
        let artwork = select(genre).await;
        if let Ok(mut memo) = self.memo.lock() {
            memo.retain(|_, (_, at)| at.elapsed() < REMEMBER);
            memo.insert(key, (artwork.clone(), Instant::now()));
        }
        artwork
    }
}

/// True when every album of a remembered choice still has the same art
/// and its file is still there.
async fn still_current(artwork: &GenreArtwork, candidates: &[ArtCandidate]) -> bool {
    let by_id: HashMap<&str, &ArtCandidate> = candidates
        .iter()
        .map(|candidate| (candidate.album_id.as_str(), candidate))
        .collect();
    for album in &artwork.albums {
        let Some(candidate) = by_id.get(album.album_id.as_str()) else {
            return false;
        };
        if candidate.cover_version != album.cover_version || !file_present(candidate).await {
            return false;
        }
    }
    true
}

async fn file_present(candidate: &ArtCandidate) -> bool {
    tokio::fs::metadata(&candidate.path)
        .await
        .is_ok_and(|meta| meta.is_file() && meta.len() > 0)
}

/// What identifies the image: the hash the scan recorded, else the hash
/// of the bytes read now. `None` when the art cannot be served.
async fn image_identity(candidate: &ArtCandidate) -> Option<String> {
    if let Some(hash) = &candidate.content_hash {
        return file_present(candidate).await.then(|| hash.clone());
    }
    let art = LocalArt {
        album_id: candidate.album_id.clone(),
        source: candidate.source.clone(),
        path: candidate.path.clone().into(),
        content_hash: None,
        version: candidate.cover_version,
    };
    read_local_art(&art)
        .await
        .map(|(bytes, _)| sha256_hex(&bytes))
}

fn stable_key(folded: &str, album_id: &str) -> String {
    sha256_hex(format!("{folded}:{album_id}").as_bytes())
}

fn artist_key(candidate: &ArtCandidate) -> String {
    candidate
        .album_artist_id
        .clone()
        .or_else(|| candidate.album_artist_name.clone())
        .unwrap_or_default()
}

/// Choose up to four covers for one genre (v2 `GenreArtworkService`).
async fn select(genre: &GenreCandidates) -> GenreArtwork {
    let mut tiers: BTreeMap<i64, Vec<&ArtCandidate>> = BTreeMap::new();
    for candidate in &genre.candidates {
        tiers
            .entry(candidate.match_count)
            .or_default()
            .push(candidate);
    }
    let mut chosen: Vec<GenreArtworkAlbum> = Vec::new();
    let mut artists: HashSet<String> = HashSet::new();
    let mut images: HashSet<String> = HashSet::new();
    for (_, mut remaining) in tiers.into_iter().rev() {
        remaining.sort_by_cached_key(|candidate| stable_key(&genre.folded, &candidate.album_id));
        while !remaining.is_empty() && chosen.len() < COLLAGE_SIZE {
            let index = remaining
                .iter()
                .position(|candidate| !artists.contains(&artist_key(candidate)))
                .unwrap_or(0);
            let candidate = remaining.remove(index);
            let Some(image) = image_identity(candidate).await else {
                continue;
            };
            if !images.insert(image) {
                continue;
            }
            artists.insert(artist_key(candidate));
            chosen.push(GenreArtworkAlbum {
                album_id: candidate.album_id.clone(),
                album_title: candidate.album_title.clone(),
                cover_version: candidate.cover_version,
                album_artist_name: candidate.album_artist_name.clone(),
            });
        }
        if chosen.len() == COLLAGE_SIZE {
            break;
        }
    }
    artwork_of(genre.revision, chosen)
}

/// The artwork for a choice: its version names the rules, the genre's
/// revision and the chosen covers, so clients refetch only on a change.
fn artwork_of(revision: i64, albums: Vec<GenreArtworkAlbum>) -> GenreArtwork {
    let joined = albums
        .iter()
        .map(|album| format!("{}:{}", album.album_id, album.cover_version))
        .collect::<Vec<_>>()
        .join("|");
    let digest = sha256_hex(joined.as_bytes());
    GenreArtwork {
        kind: if albums.is_empty() {
            "gradient"
        } else {
            "collage"
        }
        .to_owned(),
        version: format!("{ALGORITHM_VERSION}:{revision}:{}", &digest[..12]),
        albums,
    }
}
