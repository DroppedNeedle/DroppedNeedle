//! Seed artists: who a person's own shelves and picks grow from.
//!
//! Every input is that person's own: their plays, hearts and ratings in
//! the app, the artists they follow, their own playlists, the albums they
//! asked for or put on their Later pile, and the accounts they linked
//! themselves (ListenBrainz, Last.fm, their own Jellyfin login, never a
//! shared one). Each statement is filtered by their id, so nobody else's
//! rows ever count. What the library holds, which is shared, only tops up a
//! list that came out too short.
//!
//! Artists are keyed by MusicBrainz id alone. A row without one,
//! Various Artists and MusicBrainz's other special-purpose ids are dropped,
//! and a name never joins two inputs. The inputs add up to one weight per
//! artist; a seed needs [`MIN_WEIGHT`], or [`SEED_PLAYS`] plays and listens
//! while its ratings do not count against it, and the heaviest
//! [`MAX_SEEDS`] are kept, ties by id.
//!
//! Plays are the person's plays in the app and the listens imported from
//! their ListenBrainz history; while that import is current, ListenBrainz's
//! top artists are not read, since the history already holds them. An
//! album they said they know already counts as a heart on it. Once their
//! taste is built the seeds are read from it instead
//! ([`crate::discover::engine`]): the heaviest, taking turns across their
//! groups.

use std::collections::{BTreeMap, HashSet};

use droppedneedle_library::identify::artist_links::placeholder;
use droppedneedle_providers::musicbrainz::is_valid_mbid;

use super::build::{BuildContext, normalize, or_empty};
use crate::discover::engine::history::History;
use crate::discover::engine::{sql as engine_sql, taste_store};
use crate::discover::feedback::{FeedbackKind, FeedbackTarget};
use crate::discover::listening::{ArtistRow, JellyfinList, ListenSource, StatsRange, UserMusic};
use crate::discover::{read_all, sql};

// The weights, the minimum and the cap: proposed. Taste groups weigh with them too.

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
/// An album of the artist on the Later pile, however many: what a follow
/// gives.
const LATER: f64 = FOLLOW;
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
/// The weight that makes an artist a seed by itself: a heart on
/// the artist or an album, a follow, or two plays in the last three weeks.
pub(in crate::discover) const MIN_WEIGHT: f64 = 1.0;
/// The plays in the app and imported listens, a scrobbled play counted
/// once, that make an artist a seed at any weight, unless its ratings count
/// against it.
pub(in crate::discover) const SEED_PLAYS: i64 = 2;
/// The most seeds one list holds.
pub(in crate::discover) const MAX_SEEDS: usize = 30;
/// The seeds a page build grows from. A list with fewer reads the person's
/// ListenBrainz artists of all time as well and is topped up from the
/// library.
pub(in crate::discover) const SEEDS: usize = 3;
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
pub(crate) enum SeedInput {
    /// The person's plays in the app and their imported listens.
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
    /// The albums on their Later pile.
    Later,
    /// An account they linked: ListenBrainz, Last.fm or their Jellyfin.
    Linked,
    /// What the library holds.
    Library,
}

/// One seed artist.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Seed {
    /// The artist: lowercased MusicBrainz id, name, and the person's own
    /// plays in the app, when they have some.
    pub artist: ArtistRow,
    /// How much the inputs add up to.
    pub weight: f64,
    /// Which inputs spoke, in [`SeedInput`]'s order; none named for a seed
    /// read from the person's taste build.
    pub inputs: Vec<SeedInput>,
}

/// Whether the person linked a listening service of their own: the test
/// for a seed badge naming a service.
pub(in crate::discover) fn links_listening(user: &UserMusic) -> bool {
    user.listenbrainz.is_some() || user.jellyfin || (user.lastfm && user.lastfm_username.is_some())
}

/// A usable seed id: a MusicBrainz artist id, lowercased, not a
/// special-purpose one.
pub(in crate::discover) fn seed_mbid(value: Option<&str>) -> Option<String> {
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

/// What the inputs say about one artist.
#[derive(Default)]
pub(in crate::discover) struct Tally {
    /// The first name an input gave.
    pub name: String,
    /// The person's plays of it in the app.
    pub plays: i64,
    /// Their plays in the app and imported listens of it.
    history: i64,
    /// The inputs' weight added up.
    pub weight: f64,
    /// Whether their ratings of it add up to less than nothing.
    rated_down: bool,
    inputs: Vec<SeedInput>,
}

impl Tally {
    /// Whether the artist is a seed: weight [`MIN_WEIGHT`] or more, or
    /// [`SEED_PLAYS`] plays and listens with ratings that do not count
    /// against it.
    fn is_seed(&self) -> bool {
        self.weight >= MIN_WEIGHT
            || (self.history >= SEED_PLAYS && self.weight > 0.0 && !self.rated_down)
    }
}

/// Every artist an input named, by MusicBrainz id.
#[derive(Default)]
pub(in crate::discover) struct Tallies(BTreeMap<String, Tally>);

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
        self.0.values().filter(|tally| tally.is_seed()).count()
    }

    /// Every artist named, by id.
    pub(in crate::discover) fn artists(&self) -> &BTreeMap<String, Tally> {
        &self.0
    }

    /// The seeds' ids, heaviest first, ties by id: at most [`MAX_SEEDS`].
    pub(in crate::discover) fn seed_ids(&self) -> Vec<String> {
        let mut ids: Vec<(&String, f64)> = self
            .0
            .iter()
            .filter(|(_, tally)| tally.is_seed())
            .map(|(mbid, tally)| (mbid, tally.weight))
            .collect();
        // The map is in id order and the sort is stable, so ties stay by id.
        ids.sort_by(|a, b| b.1.total_cmp(&a.1));
        ids.truncate(MAX_SEEDS);
        ids.into_iter().map(|(mbid, _)| mbid.clone()).collect()
    }

    /// The seeds, heaviest first, ties by id.
    fn into_seeds(mut self) -> Vec<Seed> {
        self.seed_ids()
            .into_iter()
            .filter_map(|mbid| {
                let mut tally = self.0.remove(&mbid)?;
                tally.inputs.sort();
                Some(Seed {
                    artist: ArtistRow {
                        name: tally.name,
                        mbid: Some(mbid),
                        listen_count: (tally.plays > 0).then_some(tally.plays),
                    },
                    weight: tally.weight,
                    inputs: tally.inputs,
                })
            })
            .collect()
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
/// of weight [`MIN_WEIGHT`] or with [`SEED_PLAYS`]. Once their taste is
/// built they come from it; until then they are weighed here. `source` is
/// the service their listening follows; with both linked, both feed while
/// the server blends them. A list too short is topped up from the library.
pub(crate) async fn weighted_seeds(
    ctx: &BuildContext<'_>,
    user_id: &str,
    user: &UserMusic,
    source: ListenSource,
) -> Vec<Seed> {
    match taste_store::seeds(ctx.db.db(), user_id).await {
        Ok(Some(seeds)) => return topped_up(ctx, seeds).await,
        Ok(None) => {}
        Err(cause) => {
            tracing::warn!(%cause, "taste build unreadable; weighing seeds afresh");
            ctx.ask.note_failure();
        }
    }
    let history = match History::read(ctx.db.db(), user_id, ctx.now_ms).await {
        Ok(history) => history,
        Err(cause) => {
            tracing::warn!(%cause, "listening history unreadable; leaving plays out");
            ctx.ask.note_failure();
            History::default()
        }
    };
    let read_listenbrainz = !history.import_current;
    let mut tallies = weigh(ctx, user_id, user, source, &history, read_listenbrainz).await;
    if tallies.seeds() < SEEDS {
        top_up(ctx, &mut tallies).await;
    }
    tallies.into_seeds()
}

/// `seeds` from the taste build, with the library's artists after them
/// when there are too few.
async fn topped_up(ctx: &BuildContext<'_>, mut seeds: Vec<Seed>) -> Vec<Seed> {
    if seeds.len() >= SEEDS {
        return seeds;
    }
    let mut library = Tallies::default();
    top_up(ctx, &mut library).await;
    let have: HashSet<String> = seeds
        .iter()
        .filter_map(|seed| seed.artist.mbid.clone())
        .collect();
    library.0.retain(|mbid, _| !have.contains(mbid));
    seeds.extend(library.into_seeds());
    seeds.truncate(MAX_SEEDS);
    seeds
}

/// Add the library's artists to `tallies`, weighed by their albums.
async fn top_up(ctx: &BuildContext<'_>, tallies: &mut Tallies) {
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

/// Every artist the person's own rows and accounts name, weighed: their
/// plays and imported listens in `history`, hearts, ratings, follows,
/// playlists, requests, the albums they said they know already or put on
/// their Later pile, and the accounts they linked. ListenBrainz's top
/// artists are read only when `read_listenbrainz` says so. What the library
/// holds is not added.
pub(in crate::discover) async fn weigh(
    ctx: &BuildContext<'_>,
    user_id: &str,
    user: &UserMusic,
    source: ListenSource,
    history: &History,
    read_listenbrainz: bool,
) -> Tallies {
    let id = || (user_id.to_owned(),);
    let (hearts, known, ratings, follows, playlists, requests, later, linked) = tokio::join!(
        rows(
            ctx,
            "discover.seeds.hearts",
            sql::SEED_HEARTS.text(),
            id(),
            sql::SeedHearts::read
        ),
        rows(
            ctx,
            "discover.seeds.known_albums",
            engine_sql::SEED_KNOWN_ALBUMS.text(),
            (
                user_id.to_owned(),
                FeedbackTarget::Album,
                FeedbackKind::KnowIt
            ),
            engine_sql::SeedKnownAlbums::read
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
        rows(
            ctx,
            "discover.seeds.later",
            sql::SEED_LATER.text(),
            id(),
            sql::SeedLater::read
        ),
        linked_artists(ctx, user_id, user, source, read_listenbrainz),
    );

    let mut tallies = Tallies::default();
    for played in &history.artists {
        let days_since = (ctx.now_ms - played.last_at) as f64 / DAY_MS;
        let listens = played.plays + played.imported_listens;
        let weight = plays_weight(listens, played.months, days_since);
        if let Some(tally) = tallies.add(Some(&played.mbid), &played.name, SeedInput::Plays, weight)
        {
            tally.plays += played.plays;
            tally.history += listens;
        }
    }
    // An album they know already counts as a heart on it.
    let mut hearted: BTreeMap<String, (String, i64, i64, i64)> = BTreeMap::new();
    for row in hearts {
        hearted.insert(row.mbid, (row.name, row.artists, row.albums, row.tracks));
    }
    for row in known {
        hearted.entry(row.mbid).or_insert((row.name, 0, 0, 0)).2 += row.albums;
    }
    for (mbid, (name, artists, albums, tracks)) in hearted {
        let weight = (HEART_ARTIST * artists as f64
            + HEART_ALBUM * albums as f64
            + HEART_TRACK * tracks as f64)
            .min(HEARTS_MAX);
        tallies.add(Some(&mbid), &name, SeedInput::Hearts, weight);
    }
    for row in ratings {
        let good =
            (RATING_FIVE * row.fives as f64 + RATING_FOUR * row.fours as f64).min(RATINGS_MAX);
        let weight = good + RATING_LOW * row.lows as f64;
        if let Some(tally) = tallies.add(Some(&row.mbid), &row.name, SeedInput::Ratings, weight) {
            tally.rated_down |= weight < 0.0;
        }
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
    for row in later {
        tallies.add(Some(&row.mbid), &row.name, SeedInput::Later, LATER);
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
    // One more read, only when this year's listening gives too few seeds:
    // artists it already named keep this year's weight.
    if let Some(username) = linked.username
        && tallies.seeds() < SEEDS
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
    tallies
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
/// artists of [`LINKED_RANGE`] (only when `read_listenbrainz` says so) and
/// Last.fm's, the primary's alone unless the server blends both, and their
/// own Jellyfin's most played and favourites. Every read may fail: it is
/// logged and reads as nothing.
async fn linked_artists<'u>(
    ctx: &BuildContext<'_>,
    user_id: &str,
    user: &'u UserMusic,
    source: ListenSource,
    read_listenbrainz: bool,
) -> Linked<'u> {
    let blends = ctx
        .listening
        .policy_for(user)
        .blends(ctx.settings.blend_services);
    let username = user
        .listenbrainz
        .as_deref()
        .filter(|_| read_listenbrainz && (blends || source == ListenSource::Listenbrainz));
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
