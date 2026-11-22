//! Seed artists: who a person's own shelves and deck grow from.
//!
//! Every input is that person's own: their plays, hearts and ratings in
//! the app, the artists they follow, their own playlists, the albums they
//! asked for, and the accounts they linked themselves (ListenBrainz,
//! Last.fm, their own Jellyfin login, never a shared one). Each statement
//! is filtered by their id, so nobody else's rows ever count. What the
//! library holds, which is shared, only tops up a list that came out too
//! short.
//!
//! Artists are keyed by MusicBrainz id alone. A row without one,
//! Various Artists and MusicBrainz's other special-purpose ids are dropped,
//! and a name never joins two inputs. The inputs add up to one weight per
//! artist; a seed needs [`MIN_WEIGHT`], and the heaviest [`MAX_SEEDS`] are
//! kept, ties by id.

use std::collections::{BTreeMap, HashSet};

use droppedneedle_library::identify::artist_links::placeholder;
use droppedneedle_providers::musicbrainz::is_valid_mbid;

use super::build::{BuildContext, normalize, or_empty};
use crate::discover::listening::{ArtistRow, JellyfinList, ListenSource, StatsRange, UserMusic};
use crate::discover::{read_all, sql};

// The weights, the minimum and the cap: proposed; tuned with taste groups.

/// Plays weigh `ln(1 + plays)`, times this much more for each calendar
/// month past the first that holds a play...
const PLAY_MONTH_BONUS: f64 = 0.25;
/// ...halved for every this many days since the artist's last play.
const PLAY_HALF_LIFE_DAYS: f64 = 180.0;
/// A heart on the artist.
const HEART_ARTIST: f64 = 2.0;
/// A heart on one of the artist's albums.
const HEART_ALBUM: f64 = 1.0;
/// A heart on one of the artist's tracks.
const HEART_TRACK: f64 = 0.5;
/// The most hearts add for one artist.
const HEARTS_MAX: f64 = 3.0;
/// A five-star rating on the artist, an album or a track of theirs.
const RATING_FIVE: f64 = 1.5;
/// A four-star rating.
const RATING_FOUR: f64 = 0.75;
/// A one or two-star rating.
const RATING_LOW: f64 = -1.0;
/// The most good ratings add for one artist.
const RATINGS_MAX: f64 = 3.0;
/// Following the artist.
const FOLLOW: f64 = 2.0;
/// Playlist entries weigh `ln(1 + entries)` times this.
const PLAYLIST_SCALE: f64 = 0.5;
/// One album of the artist asked for.
const REQUEST: f64 = 1.0;
/// The most requests add for one artist.
const REQUESTS_MAX: f64 = 3.0;
/// A linked account's listens weigh `ln(1 + listens)` times this.
const LINKED_SCALE: f64 = 0.5;
/// A favourite on the person's own Jellyfin, as a heart on the artist.
const JELLYFIN_FAVORITE: f64 = HEART_ARTIST;
/// The library's albums of the artist weigh `ln(1 + albums)` times this.
const LIBRARY_SCALE: f64 = 0.25;
/// The least weight a seed has: two recent plays, a heart on the artist
/// or an album, or a follow.
pub(in crate::discover) const MIN_WEIGHT: f64 = 1.0;
/// The most seeds one list holds.
pub(in crate::discover) const MAX_SEEDS: usize = 30;
/// The ListenBrainz range read for top artists...
const LINKED_RANGE: StatsRange = StatsRange::ThisYear;
/// ...and the one read as well when it gives too few seeds, as early in a
/// year.
const LINKED_FALLBACK: StatsRange = StatsRange::AllTime;
/// ListenBrainz top artists read: one page, all of it kept.
const LINKED_ROWS: u32 = 100;
/// Last.fm top artists read, the size of its one read.
const LASTFM_ROWS: u32 = 25;
/// Jellyfin artists read from each list.
const JELLYFIN_ROWS: u32 = 50;
/// Library artists read to top up a short list.
const LIBRARY_ROWS: i64 = 100;

const DAY_MS: f64 = 86_400_000.0;

/// What spoke for a seed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(in crate::discover) enum SeedInput {
    /// The person's plays in the app.
    Plays,
    /// Their hearts.
    Hearts,
    /// Their ratings.
    Ratings,
    /// Their follows.
    Follows,
    /// Their own playlists.
    Playlists,
    /// The albums they asked for.
    Requests,
    /// An account they linked: ListenBrainz, Last.fm or their Jellyfin.
    Linked,
    /// What the library holds.
    Library,
}

/// One seed artist.
#[derive(Debug, Clone, PartialEq)]
pub(in crate::discover) struct Seed {
    /// The artist: lowercased MusicBrainz id, name, and the person's own
    /// plays in the app, when they have some.
    pub artist: ArtistRow,
    /// How much the inputs add up to.
    pub weight: f64,
    /// Which inputs spoke, in [`SeedInput`]'s order.
    pub inputs: Vec<SeedInput>,
}

/// Whether the person linked a listening service of their own: the test
/// for a seed badge naming a service.
pub(in crate::discover) fn links_listening(user: &UserMusic) -> bool {
    user.listenbrainz.is_some() || user.jellyfin || (user.lastfm && user.lastfm_username.is_some())
}

/// A usable seed id: a MusicBrainz artist id, lowercased, not a
/// special-purpose one.
fn seed_mbid(value: Option<&str>) -> Option<String> {
    let mbid = normalize(value).filter(|mbid| is_valid_mbid(mbid))?;
    (!placeholder(&mbid)).then_some(mbid)
}

fn ln_1p(count: i64) -> f64 {
    (count.max(0) as f64).ln_1p()
}

/// The weight of `plays` in `months` calendar months, the last
/// `days_since` days ago.
fn plays_weight(plays: i64, months: i64, days_since: f64) -> f64 {
    ln_1p(plays)
        * (1.0 + PLAY_MONTH_BONUS * (months.max(1) - 1) as f64)
        * 0.5_f64.powf(days_since.max(0.0) / PLAY_HALF_LIFE_DAYS)
}

#[derive(Default)]
struct Tally {
    name: String,
    plays: i64,
    weight: f64,
    inputs: Vec<SeedInput>,
}

/// Every artist an input named, by MusicBrainz id.
#[derive(Default)]
struct Tallies(BTreeMap<String, Tally>);

impl Tallies {
    /// Count `weight` for the artist `mbid` from `input`; nothing for a
    /// row without a usable id. The first name an input gives is kept.
    fn add(
        &mut self,
        mbid: Option<&str>,
        name: &str,
        input: SeedInput,
        weight: f64,
    ) -> Option<&mut Tally> {
        let tally = self.0.entry(seed_mbid(mbid)?).or_default();
        if tally.name.is_empty() {
            tally.name = name.trim().to_owned();
        }
        if !tally.inputs.contains(&input) {
            tally.inputs.push(input);
        }
        tally.weight += weight;
        Some(tally)
    }

    fn seeds(&self) -> usize {
        self.0
            .values()
            .filter(|tally| tally.weight >= MIN_WEIGHT)
            .count()
    }

    /// The seeds, heaviest first, ties by id.
    fn into_seeds(self) -> Vec<Seed> {
        let mut seeds: Vec<Seed> = self
            .0
            .into_iter()
            .filter(|(_, tally)| tally.weight >= MIN_WEIGHT)
            .map(|(mbid, mut tally)| {
                tally.inputs.sort();
                Seed {
                    artist: ArtistRow {
                        name: tally.name,
                        mbid: Some(mbid),
                        listen_count: (tally.plays > 0).then_some(tally.plays),
                    },
                    weight: tally.weight,
                    inputs: tally.inputs,
                }
            })
            .collect();
        // The map is in id order and the sort is stable, so ties stay by id.
        seeds.sort_by(|a, b| b.weight.total_cmp(&a.weight));
        seeds.truncate(MAX_SEEDS);
        seeds
    }
}

/// Read one input's rows; a failure is logged, marks the build's ask, and
/// reads as nothing.
async fn rows<T, P, F>(
    ctx: &BuildContext<'_>,
    name: &'static str,
    statement: &'static str,
    params: P,
    map: F,
) -> Vec<T>
where
    T: Send + 'static,
    P: rusqlite::Params + Send + 'static,
    F: FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T> + Send + 'static,
{
    match read_all(ctx.db.db(), name, statement, params, map).await {
        Ok(rows) => rows,
        Err(cause) => {
            tracing::warn!(read = name, %cause, "seed read failed; leaving that input out");
            ctx.ask.note_failure();
            Vec::new()
        }
    }
}

/// The person's seed artists, heaviest first: at most [`MAX_SEEDS`], each
/// of weight [`MIN_WEIGHT`] or more. `source` is the service their
/// listening follows; with both linked, both feed while the server blends
/// them.
pub(in crate::discover) async fn weighted_seeds(
    ctx: &BuildContext<'_>,
    user_id: &str,
    user: &UserMusic,
    source: ListenSource,
) -> Vec<Seed> {
    let id = || (user_id.to_owned(),);
    let (plays, hearts, ratings, follows, playlists, requests, linked) = tokio::join!(
        rows(
            ctx,
            "discover.seeds.plays",
            sql::SEED_PLAYS.text(),
            id(),
            sql::SeedPlays::read
        ),
        rows(
            ctx,
            "discover.seeds.hearts",
            sql::SEED_HEARTS.text(),
            id(),
            sql::SeedHearts::read
        ),
        rows(
            ctx,
            "discover.seeds.ratings",
            sql::SEED_RATINGS.text(),
            id(),
            sql::SeedRatings::read
        ),
        rows(
            ctx,
            "discover.seeds.follows",
            sql::SEED_FOLLOWS.text(),
            id(),
            sql::SeedFollow::read
        ),
        rows(
            ctx,
            "discover.seeds.playlists",
            sql::SEED_PLAYLIST_ENTRIES.text(),
            id(),
            sql::SeedPlaylistEntry::read
        ),
        rows(
            ctx,
            "discover.seeds.requests",
            sql::SEED_REQUESTS.text(),
            id(),
            sql::SeedRequests::read
        ),
        linked_artists(ctx, user_id, user, source),
    );

    let mut tallies = Tallies::default();
    for row in plays {
        let days_since = (ctx.now_ms - row.last_played_at.0) as f64 / DAY_MS;
        let weight = plays_weight(row.plays, row.months, days_since);
        if let Some(tally) = tallies.add(Some(&row.mbid), &row.name, SeedInput::Plays, weight) {
            tally.plays += row.plays;
        }
    }
    for row in hearts {
        let weight = (HEART_ARTIST * row.artists as f64
            + HEART_ALBUM * row.albums as f64
            + HEART_TRACK * row.tracks as f64)
            .min(HEARTS_MAX);
        tallies.add(Some(&row.mbid), &row.name, SeedInput::Hearts, weight);
    }
    for row in ratings {
        let good =
            (RATING_FIVE * row.fives as f64 + RATING_FOUR * row.fours as f64).min(RATINGS_MAX);
        let weight = good + RATING_LOW * row.lows as f64;
        tallies.add(Some(&row.mbid), &row.name, SeedInput::Ratings, weight);
    }
    for row in follows {
        tallies.add(Some(&row.mbid), &row.name, SeedInput::Follows, FOLLOW);
    }
    let mut entries: BTreeMap<String, (String, i64)> = BTreeMap::new();
    for entry in playlists {
        // A library entry stores the library artist's own id, which stands
        // only for that artist's MusicBrainz id; an imported one may store
        // the id itself.
        let named = if entry.local_artist {
            entry.local_artist_mbid.as_deref()
        } else {
            entry.artist_id.as_deref()
        };
        let artist = seed_mbid(named).or_else(|| seed_mbid(entry.recording_artist_mbid.as_deref()));
        if let Some(artist) = artist {
            let counted = entries.entry(artist).or_insert((entry.artist_name, 0));
            counted.1 += 1;
        }
    }
    for (mbid, (name, count)) in entries {
        let weight = PLAYLIST_SCALE * ln_1p(count);
        tallies.add(Some(&mbid), &name, SeedInput::Playlists, weight);
    }
    for row in requests {
        let weight = (REQUEST * row.albums as f64).min(REQUESTS_MAX);
        tallies.add(Some(&row.mbid), &row.name, SeedInput::Requests, weight);
    }
    let named_this_year: HashSet<String> = linked
        .listenbrainz
        .iter()
        .filter_map(|(artist, _)| seed_mbid(artist.mbid.as_deref()))
        .collect();
    for (artist, weight) in linked.listenbrainz.into_iter().chain(linked.others) {
        tallies.add(
            artist.mbid.as_deref(),
            &artist.name,
            SeedInput::Linked,
            weight,
        );
    }
    let wanted = ctx.settings.seed_artists.max(1);
    // One more read, only when this year's listening gives too few seeds:
    // artists it already named keep this year's weight.
    if let Some(username) = linked.username
        && tallies.seeds() < wanted
    {
        let rows = or_empty(
            "listenbrainz top artists of all time",
            ctx.listening
                .user_artists(username, LINKED_FALLBACK, LINKED_ROWS, &ctx.ask)
                .await,
        );
        for (artist, weight) in rows.into_iter().map(listens) {
            let id = seed_mbid(artist.mbid.as_deref());
            if id.is_some_and(|id| !named_this_year.contains(&id)) {
                tallies.add(
                    artist.mbid.as_deref(),
                    &artist.name,
                    SeedInput::Linked,
                    weight,
                );
            }
        }
    }

    if tallies.seeds() < wanted {
        let library = rows(
            ctx,
            "discover.seeds.library",
            sql::TOP_ARTISTS.text(),
            (LIBRARY_ROWS,),
            sql::HomeArtistColumns::read,
        )
        .await;
        for row in library {
            let weight = LIBRARY_SCALE * ln_1p(row.album_count);
            tallies.add(
                row.mbid.as_deref(),
                &row.display_name,
                SeedInput::Library,
                weight,
            );
        }
    }
    tallies.into_seeds()
}

/// What the person's own linked accounts name.
struct Linked<'u> {
    /// The ListenBrainz account read, when it was.
    username: Option<&'u str>,
    /// Its top artists of [`LINKED_RANGE`], each with its weight.
    listenbrainz: Vec<(ArtistRow, f64)>,
    /// Last.fm's and their own Jellyfin's artists, each with its weight.
    others: Vec<(ArtistRow, f64)>,
}

/// A linked account's row with the weight of its listens.
fn listens(row: ArtistRow) -> (ArtistRow, f64) {
    let weight = LINKED_SCALE * ln_1p(row.listen_count.unwrap_or(0));
    (row, weight)
}

/// The artists the person's own linked accounts name: ListenBrainz's top
/// artists of [`LINKED_RANGE`] and Last.fm's, the primary's alone unless
/// the server blends both, and their own Jellyfin's most played and
/// favourites. Every read may fail: it is logged and reads as nothing.
async fn linked_artists<'u>(
    ctx: &BuildContext<'_>,
    user_id: &str,
    user: &'u UserMusic,
    source: ListenSource,
) -> Linked<'u> {
    let blends = ctx
        .listening
        .policy_for(user)
        .blends(ctx.settings.blend_services);
    let username = user
        .listenbrainz
        .as_deref()
        .filter(|_| blends || source == ListenSource::Listenbrainz);
    let listenbrainz = async {
        let Some(username) = username else {
            return Vec::new();
        };
        or_empty(
            "listenbrainz top artists",
            ctx.listening
                .user_artists(username, LINKED_RANGE, LINKED_ROWS, &ctx.ask)
                .await,
        )
    };
    let lastfm = async {
        let Some(username) = user
            .lastfm_username
            .as_deref()
            .filter(|_| user.lastfm && (blends || source == ListenSource::Lastfm))
        else {
            return Vec::new();
        };
        or_empty(
            "lastfm top artists",
            ctx.listening
                .lastfm_user_artists(user_id, username, LASTFM_ROWS, &ctx.ask)
                .await,
        )
    };
    let jellyfin_plays = async {
        if !user.jellyfin {
            return Vec::new();
        }
        or_empty(
            "jellyfin most played",
            ctx.listening
                .jellyfin_plays(user_id, JELLYFIN_ROWS, &ctx.ask)
                .await,
        )
    };
    let jellyfin_favorites = async {
        if !user.jellyfin {
            return Vec::new();
        }
        or_empty(
            "jellyfin favorites",
            ctx.listening
                .jellyfin_artists(user_id, JellyfinList::Favorites, JELLYFIN_ROWS, &ctx.ask)
                .await,
        )
    };
    let (listenbrainz, lastfm, played, favorites) =
        tokio::join!(listenbrainz, lastfm, jellyfin_plays, jellyfin_favorites);
    let mut others: Vec<(ArtistRow, f64)> = lastfm.into_iter().map(listens).collect();
    others.extend(played.into_iter().map(|row| {
        listens(ArtistRow {
            name: row.name,
            mbid: row.mbid,
            listen_count: Some(row.play_count),
        })
    }));
    others.extend(favorites.into_iter().map(|row| (row, JELLYFIN_FAVORITE)));
    Linked {
        username,
        listenbrainz: listenbrainz.into_iter().map(listens).collect(),
        others,
    }
}
