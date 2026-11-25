//! Working out a year from its plays, in Rust: the keys each play is
//! counted under, the totals, the ranked lists and the dates, each play's
//! local date taken in the year's time zone.

use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap};

use droppedneedle_library::scan::naming::{UNKNOWN_ARTIST, VARIOUS_ARTISTS, artist_name_key};
use jiff::Timestamp;
use jiff::civil::Date;
use jiff::tz::TimeZone;

use super::models::YearReviewState;
use super::sql::YearPlay;

/// Under this many plays a year has too few to review.
pub const MIN_PLAYS: u64 = 30;
/// Under this many distinct artists a year has too few to review.
pub const MIN_ARTISTS: u64 = 5;
/// The longest top album, artist and song lists.
pub const TOP: usize = 10;
/// The most new artists listed.
pub const NEW_TOP: usize = 8;
/// The plays an album needs in the busiest month to be named for it.
pub const MONTH_ALBUM_PLAYS: u64 = 5;

/// What a play counts for in the artist lists: the library artist, else
/// the name key of the name the play carries. A placeholder artist row and
/// a blank name count for none.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ArtistKey {
    /// A library artist, by id.
    Id(String),
    /// A play no artist row holds, by [`artist_name_key`] of its name.
    Name(String),
}

/// The artist key of a play with artist row `id` (a placeholder when
/// `placeholder`) and artist name `name`. A play no row holds whose name is
/// a placeholder ("Unknown Artist", "Various Artists", MusicBrainz's
/// "[unknown]") counts for none, as a placeholder row does.
pub fn artist_key(id: Option<&str>, placeholder: bool, name: &str) -> Option<ArtistKey> {
    match id {
        Some(_) if placeholder => None,
        Some(id) => Some(ArtistKey::Id(id.to_owned())),
        None => {
            let key = artist_name_key(name);
            let named_nobody = [UNKNOWN_ARTIST, VARIOUS_ARTISTS, "[unknown]"]
                .iter()
                .any(|placeholder| artist_name_key(placeholder) == key);
            (!key.is_empty() && !named_nobody).then_some(ArtistKey::Name(key))
        }
    }
}

/// A year's state from its plays and distinct artists.
pub fn state(plays: u64, artists: u64, current: bool) -> YearReviewState {
    if plays < MIN_PLAYS || artists < MIN_ARTISTS {
        YearReviewState::TooFewPlays
    } else if current {
        YearReviewState::InProgress
    } else {
        YearReviewState::Complete
    }
}

/// The local date of `ms` in `zone`. Every play read lies inside a year's
/// bounds, which are valid dates, so the time is always in range.
pub fn local_date(zone: &TimeZone, ms: i64) -> Date {
    zone.to_datetime(Timestamp::from_millisecond(ms).unwrap_or_default())
        .date()
}

/// One entry of a ranked list: how often and how early it was played, and
/// the name it shows, which breaks the last ties.
#[derive(Debug, Clone)]
pub struct Tally<K> {
    /// What it is counted under.
    pub key: K,
    /// Plays.
    pub plays: u64,
    /// Its earliest play counted, unix milliseconds.
    pub first_at: i64,
    /// The name it shows.
    pub name: String,
}

impl<K> Tally<K> {
    fn new(key: K, at: i64, name: String) -> Self {
        Self {
            key,
            plays: 0,
            first_at: at,
            name,
        }
    }
}

/// The list order: most played first, ties by the earlier first play,
/// then the name, then the key.
pub fn order<K: Ord>(a: &Tally<K>, b: &Tally<K>) -> Ordering {
    b.plays
        .cmp(&a.plays)
        .then(a.first_at.cmp(&b.first_at))
        .then_with(|| a.name.cmp(&b.name))
        .then_with(|| a.key.cmp(&b.key))
}

/// `tallies` in [`order`], cut to `top`.
pub fn ranked<K: Ord>(mut tallies: Vec<Tally<K>>, top: usize) -> Vec<Tally<K>> {
    tallies.sort_by(order);
    tallies.truncate(top);
    tallies
}

/// What the totals say, whatever the state.
#[derive(Debug, Clone, Default)]
pub struct Totals {
    /// Plays.
    pub plays: u64,
    /// Distinct artists.
    pub artists: u64,
    /// Days with a play.
    pub days_played: u64,
    /// Summed lengths.
    pub listened_ms: u64,
    /// Plays with no length.
    pub plays_without_length: u64,
    /// Plays per month, January first.
    pub months: [u64; 12],
}

/// An artist the year played, with what the new-artist check and its
/// picture need.
#[derive(Debug, Clone)]
pub struct PlayedArtist {
    /// Its tally in the year.
    pub tally: Tally<ArtistKey>,
    /// Its MusicBrainz id, lowercased, when it has one.
    pub mbid: Option<String>,
}

/// A song the year played: its tally, keyed by the song, and the track
/// that shows it.
#[derive(Debug, Clone)]
pub struct PlayedSong {
    /// Its tally, named by the track that shows it.
    pub tally: Tally<String>,
    /// That track; none when no play of it had one.
    pub track_id: Option<String>,
    /// That track's album.
    pub album_id: Option<String>,
    /// That album's title; none when there is no album.
    pub album_name: Option<String>,
    /// Its artist.
    pub artist_name: String,
}

/// The lists, worked out only for a year with enough plays.
#[derive(Debug, Clone)]
pub struct Lists {
    /// Top owned albums, by id.
    pub albums: Vec<Tally<String>>,
    /// Top artists.
    pub artists: Vec<PlayedArtist>,
    /// Top songs.
    pub songs: Vec<PlayedSong>,
    /// The busiest month (1 to 12), its plays, and its album when one has
    /// [`MONTH_ALBUM_PLAYS`].
    pub busiest_month: (u8, u64, Option<Tally<String>>),
    /// The first play: its time, song and artist.
    pub first_play: (i64, String, String),
    /// The busiest day and its plays.
    pub busiest_day: (Date, u64),
    /// The longest run of days: its length, first and last day.
    pub streak: (u64, Date, Date),
    /// Every library artist played, by id, for the new-artist check.
    pub library_artists: HashMap<String, PlayedArtist>,
}

/// A year worked out from its plays.
#[derive(Debug, Clone)]
pub struct Worked {
    /// The totals.
    pub totals: Totals,
    /// The lists; none under too few plays.
    pub lists: Option<Lists>,
}

/// The name a play's artist shows: its library artist's, else the play's.
fn artist_shown(play: &YearPlay) -> String {
    play.artist_display
        .clone()
        .unwrap_or_else(|| play.artist_name.clone())
}

/// A song is its recording, else its track, else the name keys of its
/// artist and title, so one song on an album and a best-of counts once.
fn song_key(play: &YearPlay) -> String {
    if let Some(recording) = play.recording.as_deref().filter(|id| !id.is_empty()) {
        return format!("r:{recording}");
    }
    if let Some(track) = play.track_id.as_deref() {
        return format!("t:{track}");
    }
    format!(
        "n:{}\u{1f}{}",
        artist_name_key(&play.artist_name),
        artist_name_key(&play.track_name)
    )
}

/// The most days in a row among `days` (sorted), the earliest on a tie.
fn longest_streak(days: &BTreeMap<Date, u64>) -> Option<(u64, Date, Date)> {
    let mut best: Option<(u64, Date, Date)> = None;
    let mut run: Option<(u64, Date, Date)> = None;
    for &day in days.keys() {
        run = match run {
            Some((length, first, last)) if last.tomorrow().ok() == Some(day) => {
                Some((length + 1, first, day))
            }
            _ => Some((1, day, day)),
        };
        if let Some(current) = run
            && best.is_none_or(|(length, _, _)| current.0 > length)
        {
            best = Some(current);
        }
    }
    best
}

/// The year's plays worked out. `plays` are oldest first; `current` says
/// whether the year is the one in progress.
pub fn work_out(plays: &[YearPlay], zone: &TimeZone, current: bool) -> Worked {
    let mut totals = Totals::default();
    let mut artists: HashMap<ArtistKey, PlayedArtist> = HashMap::new();
    let mut days: BTreeMap<Date, u64> = BTreeMap::new();
    let mut dates = Vec::with_capacity(plays.len());
    for play in plays {
        let at = play.played_at.0;
        let date = local_date(zone, at);
        dates.push(date);
        totals.plays += 1;
        match play.length_ms.and_then(|ms| u64::try_from(ms).ok()) {
            Some(ms) if ms > 0 => totals.listened_ms += ms,
            _ => totals.plays_without_length += 1,
        }
        let month = usize::try_from(date.month() - 1).unwrap_or_default();
        if let Some(count) = totals.months.get_mut(month) {
            *count += 1;
        }
        *days.entry(date).or_default() += 1;
        if let Some(key) = artist_key(
            play.artist_id.as_deref(),
            play.artist_placeholder,
            &play.artist_name,
        ) {
            let artist = artists.entry(key.clone()).or_insert_with(|| PlayedArtist {
                tally: Tally::new(key, at, artist_shown(play)),
                mbid: play
                    .artist_mbid
                    .as_deref()
                    .map(|id| id.trim().to_ascii_lowercase())
                    .filter(|id| !id.is_empty()),
            });
            artist.tally.plays += 1;
        }
    }
    totals.artists = artists.len() as u64;
    totals.days_played = days.len() as u64;
    if state(totals.plays, totals.artists, current) == YearReviewState::TooFewPlays {
        return Worked {
            totals,
            lists: None,
        };
    }

    let mut albums: HashMap<&str, Tally<String>> = HashMap::new();
    let mut songs: HashMap<String, SongTracks> = HashMap::new();
    for play in plays {
        let at = play.played_at.0;
        if let (true, Some(id)) = (play.album_listed, play.album_id.as_deref()) {
            albums
                .entry(id)
                .or_insert_with(|| {
                    Tally::new(
                        id.to_owned(),
                        at,
                        play.album_title.clone().unwrap_or_default(),
                    )
                })
                .plays += 1;
        }
        songs
            .entry(song_key(play))
            .or_insert_with(|| SongTracks::new(play))
            .add(play);
    }

    let (month_index, month_plays) =
        totals
            .months
            .iter()
            .enumerate()
            .fold(
                (0, 0),
                |best, (index, &count)| {
                    if count > best.1 { (index, count) } else { best }
                },
            );
    let mut month_albums: HashMap<&str, Tally<String>> = HashMap::new();
    for (play, date) in plays.iter().zip(&dates) {
        if usize::try_from(date.month() - 1).ok() != Some(month_index) {
            continue;
        }
        if let (true, Some(id)) = (play.album_listed, play.album_id.as_deref()) {
            month_albums
                .entry(id)
                .or_insert_with(|| {
                    Tally::new(
                        id.to_owned(),
                        play.played_at.0,
                        play.album_title.clone().unwrap_or_default(),
                    )
                })
                .plays += 1;
        }
    }
    let month_album = ranked(month_albums.into_values().collect(), 1)
        .into_iter()
        .next()
        .filter(|album| album.plays >= MONTH_ALBUM_PLAYS);

    let first_play = plays.first().map_or_else(
        || (0, String::new(), String::new()),
        |play| {
            (
                play.played_at.0,
                play.track_title
                    .clone()
                    .unwrap_or_else(|| play.track_name.clone()),
                artist_shown(play),
            )
        },
    );
    let busiest_day = days.iter().fold(
        None,
        |best: Option<(Date, u64)>, (&day, &count)| match best {
            Some((_, most)) if most >= count => best,
            _ => Some((day, count)),
        },
    );
    let Some(busiest_day) = busiest_day else {
        return Worked {
            totals,
            lists: None,
        };
    };
    let streak = longest_streak(&days).unwrap_or((1, busiest_day.0, busiest_day.0));

    let library_artists: HashMap<String, PlayedArtist> = artists
        .iter()
        .filter_map(|(key, artist)| match key {
            ArtistKey::Id(id) => Some((id.clone(), artist.clone())),
            ArtistKey::Name(_) => None,
        })
        .collect();
    let mut top_artists: Vec<PlayedArtist> = artists.into_values().collect();
    top_artists.sort_by(|a, b| order(&a.tally, &b.tally));
    top_artists.truncate(TOP);
    let mut top_songs: Vec<PlayedSong> = songs
        .into_iter()
        .map(|(key, tracks)| tracks.shown(key))
        .collect();
    top_songs.sort_by(|a, b| order(&a.tally, &b.tally));
    top_songs.truncate(TOP);
    Worked {
        totals,
        lists: Some(Lists {
            albums: ranked(albums.into_values().collect(), TOP),
            artists: top_artists,
            songs: top_songs,
            busiest_month: (
                u8::try_from(month_index + 1).unwrap_or(1),
                month_plays,
                month_album,
            ),
            first_play,
            busiest_day,
            streak,
            library_artists,
        }),
    }
}

/// A track's plays, first play, title, album (id and title) and artist shown.
type TrackTally = (u64, i64, String, Option<(String, String)>, String);

/// One song's plays, per track that played it.
struct SongTracks {
    plays: u64,
    first_at: i64,
    /// The first play's names, for a song no track played.
    name: String,
    artist_name: String,
    /// Each track that played it, by track id.
    tracks: HashMap<String, TrackTally>,
}

impl SongTracks {
    fn new(play: &YearPlay) -> Self {
        Self {
            plays: 0,
            first_at: play.played_at.0,
            name: play.track_name.clone(),
            artist_name: artist_shown(play),
            tracks: HashMap::new(),
        }
    }

    fn add(&mut self, play: &YearPlay) {
        self.plays += 1;
        if let Some(track) = play.track_id.as_deref() {
            self.tracks
                .entry(track.to_owned())
                .or_insert_with(|| {
                    (
                        0,
                        play.played_at.0,
                        play.track_title
                            .clone()
                            .unwrap_or_else(|| play.track_name.clone()),
                        play.album_id
                            .clone()
                            .map(|id| (id, play.album_title.clone().unwrap_or_default())),
                        artist_shown(play),
                    )
                })
                .0 += 1;
        }
    }

    /// The song shown by its track with most plays, the earlier first play
    /// on a tie.
    fn shown(self, key: String) -> PlayedSong {
        let track = self.tracks.into_iter().min_by(|(a_id, a), (b_id, b)| {
            b.0.cmp(&a.0)
                .then(a.1.cmp(&b.1))
                .then_with(|| a_id.cmp(b_id))
        });
        match track {
            Some((id, (_, _, title, album, artist_name))) => {
                let (album_id, album_name) = album.unzip();
                PlayedSong {
                    tally: Tally {
                        key,
                        plays: self.plays,
                        first_at: self.first_at,
                        name: title,
                    },
                    track_id: Some(id),
                    album_id,
                    album_name,
                    artist_name,
                }
            }
            None => PlayedSong {
                tally: Tally {
                    key,
                    plays: self.plays,
                    first_at: self.first_at,
                    name: self.name,
                },
                track_id: None,
                album_id: None,
                album_name: None,
                artist_name: self.artist_name,
            },
        }
    }
}
