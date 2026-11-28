//! The pure part of a mix: which tracks a taste's mix takes, its name and
//! the order they play in, from what the job read. Nothing here reads the
//! database, the clock or a provider, so one day's rows always make one
//! day's mixes.

use std::collections::{HashMap, HashSet};

use droppedneedle_library::sound::search::title_key;

use super::{
    ARC, MIN_YOURS, MIX_TRACKS, NEW_MOST, NEW_PER_ALBUM, NEW_PER_ARTIST, NEW_SHARE, PER_ARTIST,
    SOUND_MOST, WINDOW,
};
use crate::discover::models::{MixTrackKind, PickReason, PickReasonArtist};
use crate::discover::page::build::daily_shuffler;

/// One library track a mix may take.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct LibraryTrack {
    /// The library track.
    pub id: String,
    /// Its title.
    pub title: String,
    /// Its artist's name.
    pub artist_name: String,
    /// Its artist's MusicBrainz id, lowercased, when known.
    pub artist_mbid: Option<String>,
    /// Its album, the library's id.
    pub album_id: String,
    /// The album's title.
    pub album_title: Option<String>,
    /// The album's release group, lowercased, when known.
    pub release_group_mbid: Option<String>,
    /// Its recording, lowercased, when known.
    pub recording_mbid: Option<String>,
    /// How much the person's plays weigh for it, newer plays more.
    pub weight: f64,
    /// How loud it is, in LUFS, when known.
    pub loudness: Option<f64>,
    /// Which way its strongest sound tags lean: -1 calm, 1 lively, 0 none
    /// or not analysed.
    pub lean: i8,
}

/// A track from an album of the person's pool.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NewTrack {
    /// Its title.
    pub title: String,
    /// Its artist's name.
    pub artist_name: String,
    /// Its artist's MusicBrainz id, lowercased.
    pub artist_mbid: String,
    /// Its recording, when known.
    pub recording_mbid: Option<String>,
    /// Its album's release group, lowercased.
    pub release_group_mbid: String,
    /// The album's title.
    pub album_title: String,
    /// Why the album is offered.
    pub reason: PickReason,
    /// The person's artist the album was proposed from, lowercased, when it
    /// was proposed from one.
    pub seed: Option<String>,
}

/// What one taste's mix is made from.
#[derive(Debug, Clone, Default)]
pub(crate) struct TasteInputs {
    /// The taste group's anchor artist, lowercased.
    pub anchor: String,
    /// The group's artists, most played first.
    pub artists: Vec<PickReasonArtist>,
    /// The library tracks by the group's artists.
    pub own: Vec<LibraryTrack>,
    /// The library tracks by other artists that sound like the group's,
    /// nearest first; none while sound analysis is off.
    pub sound: Vec<LibraryTrack>,
    /// Tracks from the pool's albums for this group, in pool order; none
    /// while no source plays what the library lacks.
    pub new: Vec<NewTrack>,
}

/// One track of a built mix.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Picked {
    /// A library track, by one of the group's artists or found by sound.
    Library(MixTrackKind, LibraryTrack),
    /// A track from an album the library does not hold.
    New(NewTrack),
}

impl Picked {
    fn artist_key(&self) -> String {
        match self {
            Self::Library(_, track) => library_artist(track),
            Self::New(track) => track.artist_mbid.clone(),
        }
    }

    fn song_keys(&self) -> Vec<String> {
        match self {
            Self::Library(_, track) => song_keys(
                track.recording_mbid.as_deref(),
                &library_artist(track),
                &track.title,
            ),
            Self::New(track) => song_keys(
                track.recording_mbid.as_deref(),
                &track.artist_mbid,
                &track.title,
            ),
        }
    }
}

/// One built mix.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct BuiltMix {
    /// The group's anchor artist, lowercased.
    pub anchor: String,
    /// Its name.
    pub name: String,
    /// The artists its name names.
    pub named: Vec<PickReasonArtist>,
    /// Its tracks in the order they play.
    pub tracks: Vec<Picked>,
}

impl BuiltMix {
    /// How many of its tracks the library holds.
    pub fn yours_count(&self) -> usize {
        self.tracks
            .iter()
            .filter(|track| matches!(track, Picked::Library(..)))
            .count()
    }

    /// How many of its tracks are new.
    pub fn new_count(&self) -> usize {
        self.tracks.len() - self.yours_count()
    }
}

/// The artist the per-artist rules count a library track under.
fn library_artist(track: &LibraryTrack) -> String {
    match &track.artist_mbid {
        Some(mbid) if !mbid.is_empty() => mbid.clone(),
        _ => format!("name:{}", track.artist_name.to_lowercase()),
    }
}

/// The keys two versions of one song share: the recording, and the
/// artist with the title folded as the sound search folds it.
fn song_keys(recording: Option<&str>, artist: &str, title: &str) -> Vec<String> {
    let mut keys = vec![format!("{artist}|{}", title_key(title))];
    if let Some(recording) = recording.map(str::trim).filter(|id| !id.is_empty()) {
        keys.push(recording.to_ascii_lowercase());
    }
    keys
}

/// The tracks a mix takes, kept under the per-artist and versions rules.
#[derive(Default)]
struct Taken {
    picked: Vec<Picked>,
    songs: HashSet<String>,
    artists: HashMap<String, usize>,
    albums: HashMap<String, usize>,
}

impl Taken {
    /// Take `track` when no version of it is taken and its artist has
    /// room; `per_artist` and `per_album` cap a kind's own share.
    fn take(&mut self, track: Picked, per_artist: usize, album: Option<(&str, usize)>) -> bool {
        let artist = track.artist_key();
        let keys = track.song_keys();
        if keys.iter().any(|key| self.songs.contains(key))
            || self.artists.get(&artist).copied().unwrap_or(0) >= per_artist
            || album
                .is_some_and(|(album, most)| self.albums.get(album).copied().unwrap_or(0) >= most)
        {
            return false;
        }
        self.songs.extend(keys);
        *self.artists.entry(artist).or_default() += 1;
        if let Some((album, _)) = album {
            *self.albums.entry(album.to_owned()).or_default() += 1;
        }
        self.picked.push(track);
        true
    }
}

/// A day's order over `items`: the first [`WINDOW`] times `want` of them,
/// shuffled for the day, then the rest in their order. The strongest stay
/// first and a mix still turns over from day to day.
fn for_the_day<T>(mut items: Vec<T>, want: usize, day: &str, parts: &[&str]) -> Vec<T> {
    let window = (want * WINDOW).min(items.len());
    daily_shuffler(day, parts).shuffle(&mut items[..window]);
    items
}

/// The mix for one taste on `day`, or `None` when the library holds too
/// few of its tracks. `user_id` only seeds the day's shuffle.
pub(crate) fn mix(user_id: &str, day: &str, inputs: TasteInputs) -> Option<BuiltMix> {
    let TasteInputs {
        anchor,
        artists,
        mut own,
        sound,
        new,
    } = inputs;
    let mut taken = Taken::default();

    // The person's own: the group's artists' tracks, most played first.
    own.sort_by(|a, b| b.weight.total_cmp(&a.weight).then_with(|| a.id.cmp(&b.id)));
    let yours_most = MIX_TRACKS - NEW_MOST.min(MIX_TRACKS);
    let sound_room = SOUND_MOST.min(sound.len()).min(yours_most);
    let own_want = yours_most - sound_room;
    let own = for_the_day(own, own_want, day, &[user_id, &anchor, "own"]);
    let sound = for_the_day(sound, sound_room, day, &[user_id, &anchor, "sound"]);
    let mut own_taken = 0;
    for track in own.iter().cloned() {
        if own_taken >= own_want {
            break;
        }
        if taken.take(
            Picked::Library(MixTrackKind::Artist, track),
            PER_ARTIST,
            None,
        ) {
            own_taken += 1;
        }
    }
    // Found by sound, then the group's own again when either falls short.
    let mut sound_taken = 0;
    for track in sound {
        if own_taken + sound_taken >= yours_most {
            break;
        }
        if taken.take(
            Picked::Library(MixTrackKind::Sound, track),
            PER_ARTIST,
            None,
        ) {
            sound_taken += 1;
        }
    }
    for track in own {
        if own_taken + sound_taken >= yours_most {
            break;
        }
        if taken.take(
            Picked::Library(MixTrackKind::Artist, track),
            PER_ARTIST,
            None,
        ) {
            own_taken += 1;
        }
    }
    let yours = own_taken + sound_taken;
    if yours < MIN_YOURS {
        return None;
    }

    // New tracks, about a third of what the person's own make.
    let new_most = NEW_MOST
        .min(MIX_TRACKS - yours)
        .min((yours as f64 * NEW_SHARE / (1.0 - NEW_SHARE)).round() as usize);
    let new = for_the_day(new, new_most, day, &[user_id, &anchor, "new"]);
    let mut new_taken = 0;
    for track in new {
        if new_taken >= new_most {
            break;
        }
        let album = track.release_group_mbid.clone();
        if taken.take(
            Picked::New(track),
            NEW_PER_ARTIST,
            Some((&album, NEW_PER_ALBUM)),
        ) {
            new_taken += 1;
        }
    }

    let (library, fresh): (Vec<Picked>, Vec<Picked>) = taken
        .picked
        .into_iter()
        .partition(|track| matches!(track, Picked::Library(..)));
    let ordered = by_energy(library);
    let tracks = no_three_in_a_row(beside_their_seeds(ordered, fresh));
    let (name, named) = name(&artists, &tracks);
    Some(BuiltMix {
        anchor,
        name,
        named,
        tracks,
    })
}

/// The mix's name: the two of the group's artists the person plays most
/// that are on it, and "and more" when anyone else is.
fn name(artists: &[PickReasonArtist], tracks: &[Picked]) -> (String, Vec<PickReasonArtist>) {
    let on_it: HashSet<String> = tracks.iter().map(Picked::artist_key).collect();
    let named: Vec<PickReasonArtist> = artists
        .iter()
        .filter(|artist| on_it.contains(&artist.artist_mbid))
        .take(2)
        .cloned()
        .collect();
    let more = on_it.len() > named.len();
    (title(&named, more), named)
}

/// A mix's name from the artists it names, with "and more" when anyone
/// else is on it.
pub(crate) fn title(named: &[PickReasonArtist], more: bool) -> String {
    let names: Vec<&str> = named.iter().map(|artist| artist.name.as_str()).collect();
    match (names.as_slice(), more) {
        ([], _) => "Your mix".to_owned(),
        ([one], false) => (*one).to_owned(),
        ([one], true) => format!("{one} and more"),
        ([one, two, ..], false) => format!("{one} and {two}"),
        ([one, two, ..], true) => format!("{one}, {two} and more"),
    }
}

/// A library track's energy, for the order: its loudness to the whole dB,
/// then which way its tags lean. A track with no loudness counts as the
/// middle of the others.
fn energy_order(tracks: &[Picked]) -> Vec<usize> {
    let loudness: Vec<Option<f64>> = tracks
        .iter()
        .map(|track| match track {
            Picked::Library(_, track) => track.loudness,
            Picked::New(_) => None,
        })
        .collect();
    let mut known: Vec<f64> = loudness.iter().flatten().copied().collect();
    known.sort_by(f64::total_cmp);
    let middle = known.get(known.len() / 2).copied().unwrap_or(0.0);
    let key = |at: usize| {
        let lean = match &tracks[at] {
            Picked::Library(_, track) => track.lean,
            Picked::New(_) => 0,
        };
        (loudness[at].unwrap_or(middle).round() as i64, lean)
    };
    let mut order: Vec<usize> = (0..tracks.len()).collect();
    order.sort_by(|a, b| {
        key(*a)
            .cmp(&key(*b))
            .then_with(|| id_of(&tracks[*a]).cmp(id_of(&tracks[*b])))
    });
    order
}

fn id_of(track: &Picked) -> &str {
    match track {
        Picked::Library(_, track) => &track.id,
        Picked::New(track) => &track.title,
    }
}

/// Where the arc wants the energy at `place` of `len`, from 0 (the
/// quietest) to 1 (the loudest): from the middle up to the top a third
/// from the end, then down to settle.
fn arc(place: usize, len: usize) -> f64 {
    if len < 2 {
        return ARC.start;
    }
    let at = place as f64 / (len - 1) as f64;
    if at <= ARC.peak_at {
        ARC.start + (ARC.peak - ARC.start) * at / ARC.peak_at
    } else {
        ARC.peak + (ARC.end - ARC.peak) * (at - ARC.peak_at) / (1.0 - ARC.peak_at)
    }
}

/// The library tracks in the arc's order: the places, ordered by the
/// energy the arc wants there, take the tracks from the quietest up, so
/// the mix's energy rises and falls as the arc does. A run of one artist
/// is broken up afterwards.
fn by_energy(tracks: Vec<Picked>) -> Vec<Picked> {
    let len = tracks.len();
    let order = energy_order(&tracks);
    let mut places: Vec<usize> = (0..len).collect();
    places.sort_by(|a, b| arc(*a, len).total_cmp(&arc(*b, len)).then(a.cmp(b)));
    let mut at_place: Vec<usize> = vec![0; len];
    for (place, track) in places.into_iter().zip(order) {
        at_place[place] = track;
    }
    let mut tracks: Vec<Option<Picked>> = tracks.into_iter().map(Some).collect();
    at_place
        .into_iter()
        .filter_map(|at| tracks[at].take())
        .collect()
}

/// The new tracks placed among the library ones: each just after a track
/// by the person's artist its album was proposed from, one each in turn,
/// and the rest spread evenly.
fn beside_their_seeds(library: Vec<Picked>, fresh: Vec<Picked>) -> Vec<Picked> {
    let len = library.len();
    let mut after: Vec<Vec<Picked>> = vec![Vec::new(); len];
    let mut used: HashMap<usize, usize> = HashMap::new();
    let mut unplaced = Vec::new();
    for track in fresh {
        let seed = match &track {
            Picked::New(track) => track.seed.clone(),
            Picked::Library(..) => None,
        };
        let places: Vec<usize> = seed
            .map(|seed| {
                library
                    .iter()
                    .enumerate()
                    .filter(|(_, own)| own.artist_key() == seed)
                    .map(|(at, _)| at)
                    .collect()
            })
            .unwrap_or_default();
        let place = places
            .iter()
            .copied()
            .min_by_key(|at| (used.get(at).copied().unwrap_or(0), *at));
        match place {
            Some(at) => {
                *used.entry(at).or_default() += 1;
                after[at].push(track);
            }
            None => unplaced.push(track),
        }
    }
    let count = unplaced.len();
    for (n, track) in unplaced.into_iter().enumerate() {
        if len == 0 {
            break;
        }
        let at = ((n + 1) * len / (count + 1)).min(len - 1);
        after[at].push(track);
    }
    library
        .into_iter()
        .zip(after)
        .flat_map(|(own, new)| std::iter::once(own).chain(new))
        .collect()
}

/// Whether three tracks in a row in `tracks` are by one artist.
fn has_run(tracks: &[Picked]) -> bool {
    let artists: Vec<String> = tracks.iter().map(Picked::artist_key).collect();
    artists
        .windows(3)
        .any(|run| run[0] == run[1] && run[1] == run[2])
}

/// `tracks` with any third in a row by one artist broken up: the next
/// track by another artist is brought in before it, or, with none after
/// it, the third goes to the first place before the run where it makes no
/// run of its own.
fn no_three_in_a_row(mut tracks: Vec<Picked>) -> Vec<Picked> {
    let mut at = 2;
    while at < tracks.len() {
        let artist = tracks[at].artist_key();
        let run = tracks[at - 1].artist_key() == artist && tracks[at - 2].artist_key() == artist;
        if run {
            if let Some(swap) =
                (at + 1..tracks.len()).find(|later| tracks[*later].artist_key() != artist)
            {
                let moved = tracks.remove(swap);
                tracks.insert(at, moved);
            } else {
                let fits = (0..at - 1).find(|place| {
                    let mut trial = tracks[..=at].to_vec();
                    let moved = trial.remove(at);
                    trial.insert(*place, moved);
                    !has_run(&trial)
                });
                if let Some(place) = fits {
                    let moved = tracks.remove(at);
                    tracks.insert(place, moved);
                }
            }
        }
        at += 1;
    }
    tracks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, artist: &str, loudness: f64) -> LibraryTrack {
        LibraryTrack {
            id: id.to_owned(),
            title: id.to_owned(),
            artist_name: artist.to_owned(),
            artist_mbid: Some(artist.to_owned()),
            album_id: format!("album-{id}"),
            album_title: None,
            release_group_mbid: None,
            recording_mbid: None,
            weight: 1.0,
            loudness: Some(loudness),
            lean: 0,
        }
    }

    fn ids(tracks: &[Picked]) -> Vec<&str> {
        tracks.iter().map(id_of).collect()
    }

    /// Seven tracks by seven artists, t0 the quietest and t6 the loudest.
    /// Worked out by hand: the arc wants 0.5, 0.625, 0.75, 0.875 and 1.0
    /// (two thirds of the way), then 0.6 and 0.2, so the last place takes
    /// the quietest, the first the next, the sixth the next, and the fifth
    /// the loudest.
    #[test]
    fn the_order_follows_the_arc() {
        let tracks: Vec<Picked> = (0..7)
            .map(|n| {
                Picked::Library(
                    MixTrackKind::Artist,
                    track(&format!("t{n}"), &format!("a{n}"), -20.0 + n as f64),
                )
            })
            .collect();
        let ordered = by_energy(tracks);
        assert_eq!(ids(&ordered), ["t1", "t3", "t4", "t5", "t6", "t2", "t0"]);
    }

    /// Three tracks by one artist never play in a row while another would
    /// do, whether the other artist's track comes after the run or only
    /// before it.
    #[test]
    fn never_three_in_a_row_by_one_artist() {
        for order in [["x", "x", "x", "y"], ["y", "x", "x", "x"]] {
            let tracks: Vec<Picked> = order
                .iter()
                .enumerate()
                .map(|(n, artist)| {
                    Picked::Library(
                        MixTrackKind::Artist,
                        track(&format!("t{n}"), artist, -20.0 + n as f64),
                    )
                })
                .collect();
            let tracks = no_three_in_a_row(tracks);
            let artists: Vec<String> = tracks.iter().map(Picked::artist_key).collect();
            assert_eq!(artists.len(), 4);
            assert!(
                artists
                    .windows(3)
                    .all(|run| !(run[0] == run[1] && run[1] == run[2])),
                "{order:?} gave {artists:?}"
            );
        }
    }
}
