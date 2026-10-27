//! Player reads: the catalog as Subsonic and Jellyfin clients see it.
//!
//! The compat protocols need a few things the native read routes do not:
//! every tag a player shows (bit depth, channels, replay gain, sort names,
//! all genres, provider MBIDs), filters by year range and genre, exact
//! artist-name matches, batch lookups by id, and the caller's play history.
//! These are methods of [`LocalCatalog`], over the same `local_*` tables and
//! album columns as the library pages, one pooled read per statement; the
//! statements are in `sql.rs`. Every track read filters
//! `availability = 'indexed'`.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;

use droppedneedle_kernel::db::fold_text;
use droppedneedle_kernel::db::statement::{ListLen, Statement};
use droppedneedle_providers::musicbrainz::is_valid_mbid;
use rusqlite::Row;
use rusqlite::types::Value;

use super::sql;
use super::sqlite::TextSearch;
use super::store::{LocalCatalog, album_row, count, first, rows, scalar, unsigned};
use super::stores::{AlbumRecord, ArtistRecord, StoreError};

/// One streamable track with everything a player shows.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerTrack {
    /// Local track id.
    pub id: String,
    /// Title.
    pub title: String,
    /// Owning local album id.
    pub album_id: String,
    /// Album title.
    pub album_title: String,
    /// Track artist display name.
    pub artist_name: String,
    /// Lead track artist id, when credited.
    pub artist_id: Option<String>,
    /// Album artist id.
    pub album_artist_id: String,
    /// Album artist display name.
    pub album_artist_name: Option<String>,
    /// Disc number.
    pub disc_number: i64,
    /// Track number.
    pub track_number: i64,
    /// Year.
    pub year: Option<i64>,
    /// Genres in tag order.
    pub genres: Vec<String>,
    /// Duration, seconds.
    pub duration_seconds: Option<f64>,
    /// Container label, lowercased.
    pub format: String,
    /// Bit rate, kbps.
    pub bit_rate: Option<i64>,
    /// Sample rate, Hz.
    pub sample_rate: Option<i64>,
    /// Bit depth.
    pub bit_depth: Option<i64>,
    /// Channels.
    pub channels: Option<i64>,
    /// File size, bytes.
    pub file_size_bytes: i64,
    /// Path under the track's own library folder.
    pub relative_path: String,
    /// Import time, unix seconds.
    pub date_added: Option<f64>,
    /// Title sort tag.
    pub sort_name: Option<String>,
    /// Disc subtitle tag.
    pub disc_subtitle: Option<String>,
    /// Recording MBID: accepted identity, else the embedded tag.
    pub recording_mbid: Option<String>,
    /// Track artist MBID, when the artist is identified.
    pub artist_mbid: Option<String>,
    /// Album artist MBID, when identified.
    pub album_artist_mbid: Option<String>,
    /// Album release group, when identified.
    pub release_group_mbid: Option<String>,
    /// Release type tag.
    pub release_type: Option<String>,
    /// Replay gain: track gain dB.
    pub replaygain_track_gain: Option<f64>,
    /// Replay gain: album gain dB.
    pub replaygain_album_gain: Option<f64>,
    /// Replay gain: track peak.
    pub replaygain_track_peak: Option<f64>,
    /// Replay gain: album peak.
    pub replaygain_album_peak: Option<f64>,
    /// Whether the album has stored artwork.
    pub cover_available: bool,
    /// The version of the album's own art, when it has some.
    pub art_version: Option<i64>,
    /// Whether the track's file carries lyrics, as the lyrics sweep found.
    pub has_lyrics: bool,
}

/// Per artist id: the summed length in seconds and the number of the
/// streamable tracks crediting it (`artist_durations`).
pub type ArtistTracks = HashMap<String, (f64, u64)>;

/// One album with the extra facts players show.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerAlbum {
    /// The catalog row.
    pub record: AlbumRecord,
    /// Primary genre.
    pub genre: Option<String>,
    /// Album sort name.
    pub sort_name: Option<String>,
    /// Original release date, `YYYY[-MM[-DD]]`.
    pub original_release_date: Option<String>,
    /// Release date, `YYYY[-MM[-DD]]`: the one every indexed track's tags
    /// give, else none.
    pub release_date: Option<String>,
}

/// Track filters; every set field narrows.
#[derive(Debug, Clone, Default)]
pub struct TrackQuery {
    /// Title/artist/album search: every word, any column.
    pub q: Option<String>,
    /// Exact title, matched folded.
    pub title: Option<String>,
    /// One album.
    pub album_id: Option<String>,
    /// Credited (any position) artist ids.
    pub artist_ids: Vec<String>,
    /// Album artist ids.
    pub album_artist_ids: Vec<String>,
    /// Genre, matched folded.
    pub genre: Option<String>,
    /// Lowest year, inclusive.
    pub year_from: Option<i64>,
    /// Highest year, inclusive.
    pub year_to: Option<i64>,
    /// Exact track or album artist name, matched folded.
    pub artist_name: Option<String>,
    /// The user whose plays and marks `played` and the play-history orders
    /// read.
    pub listener: Option<String>,
    /// Only tracks that are played for the listener (`true`) or not
    /// (`false`), as [`PlayedValues`] says.
    pub played: Option<bool>,
}

/// A sort key the player protocols can ask for in either direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrderKey {
    /// Import time.
    Added,
    /// Title.
    Title,
    /// Year; unknown years sort first ascending.
    Year,
    /// The listener's last play; never played sorts first ascending.
    LastPlayed,
    /// The listener's play count.
    PlayCount,
}

/// Track orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackOrder {
    /// Album artist, album, disc, track.
    Album,
    /// Disc then track (one album's running order).
    Disc,
    /// Title.
    Title,
    /// Newest import first.
    Newest,
    /// A fresh random sample on every call; for one-shot lists
    /// (getRandomSongs, mixes), not for paging.
    Random,
    /// One fixed shuffle per seed: pages of the same seed never repeat or
    /// skip a track. Callers draw the seed with [`ShuffleSeeds`].
    Shuffle(u32),
    /// Insertion order (`search_key`): the full-library sync order. Pages
    /// walk the tracks in key order, and pages already read stay put while
    /// a scan appends tracks.
    Natural,
    /// One key, ascending or (`true`) descending; ids break ties in the
    /// same direction.
    By(OrderKey, bool),
}

/// Album filters; every set field narrows.
#[derive(Debug, Clone, Default)]
pub struct AlbumQuery {
    /// Title/artist substring.
    pub q: Option<String>,
    /// One album artist.
    pub artist_id: Option<String>,
    /// Any of these album artists.
    pub album_artist_ids: Vec<String>,
    /// Albums where one of these artists is credited on a track but is
    /// not the album artist ("appears on").
    pub appears_on: Vec<String>,
    /// Genre, matched folded on any track.
    pub genre: Option<String>,
    /// Lowest year, inclusive.
    pub year_from: Option<i64>,
    /// Highest year, inclusive.
    pub year_to: Option<i64>,
    /// The user whose plays and marks `played` and the play-history orders
    /// read.
    pub listener: Option<String>,
    /// Only albums that are played for the listener (`true`) or not
    /// (`false`), as [`PlayedValues`] says.
    pub played: Option<bool>,
}

/// Album orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumOrder {
    /// Newest import first.
    Newest,
    /// Title.
    Title,
    /// Album artist, then title.
    Artist,
    /// Year ascending, unknown last.
    YearAsc,
    /// Year descending, unknown last.
    YearDesc,
    /// One fixed shuffle per seed, for paging (see [`TrackOrder::Shuffle`]).
    Shuffle(u32),
    /// Insertion order (rowid).
    Natural,
    /// One key, ascending or (`true`) descending; ids break ties in the
    /// same direction.
    By(OrderKey, bool),
}

/// Play statistics for one item: count and last play (unix seconds).
pub type PlayStat = (u64, Option<f64>);

/// One listener's values for a track or an album in Jellyfin answers,
/// where a played mark and an unplayed mark weigh against the plays.
///
/// A track's counted plays are the listener's plays of it later than its
/// unplayed mark, in whole seconds, or all of them when it has none. Its
/// play count is that number, or 1 when it is marked played and has none;
/// its last play is the latest counted play, else the played mark; it is
/// played when its count is above 0.
///
/// An album's play count is the sum of its tracks' (every track on it,
/// streamable or not) plus the plays filed under it with no track, which a
/// mark never cuts off; its last play is the latest of theirs; it is
/// played when every streamable track on it is.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PlayedValues {
    /// Play count.
    pub plays: u64,
    /// Last play, unix seconds.
    pub last_played: Option<f64>,
    /// Whether it counts as played.
    pub played: bool,
}

/// The MusicBrainz ids player apps are sent for one album.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlbumIds {
    /// True when these are the accepted match's ids, false when they are
    /// the ones the album's files' tags agree on.
    pub accepted: bool,
    /// Release.
    pub release_mbid: Option<String>,
    /// Release group.
    pub release_group_mbid: Option<String>,
}

/// MusicBrainz ids for player answers, by local id. An album or artist
/// with no id is absent.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerIds {
    /// Per album.
    pub albums: HashMap<String, AlbumIds>,
    /// Per artist.
    pub artists: HashMap<String, String>,
}

droppedneedle_kernel::sql_choice! {
    /// How a player track list matches its text.
    pub(crate) enum TextMatch {
        /// No text filter.
        Off,
        /// The whole query as a substring of the title, artist or album.
        Like,
        /// The FTS index.
        Words,
        /// The FTS index, with one short word as a substring.
        OneShort,
        /// The FTS index, with two short words as substrings.
        TwoShort,
        /// The FTS index, with more short words as substrings.
        ManyShort,
    }
}

droppedneedle_kernel::sql_choice! {
    /// The played filter of a player list.
    pub(crate) enum PlayedFilter {
        /// Played or not.
        Any,
        /// Only the listener's played items.
        Played,
        /// Only the rest.
        Unplayed,
    }
}

droppedneedle_kernel::sql_choice! {
    /// The track filters that steer a statement's plan, so each set is its
    /// own statement. The others bind NULL when unset.
    pub(crate) struct TrackShape {
        /// How the text matches.
        pub text: TextMatch,
        /// Some album artists, which reads the album row.
        pub album_artists: bool,
        /// A lowest or highest year.
        pub years: bool,
        /// Any of the credited artists, genre or exact artist name.
        pub scoped: bool,
        /// The played filter.
        pub played: PlayedFilter,
    }
}

droppedneedle_kernel::sql_choice! {
    /// A player track page's order outside the play history.
    pub(crate) enum TrackSequence {
        /// Album artist, album, disc, track.
        Album,
        /// Disc then track.
        Disc,
        /// Title ascending.
        Title,
        /// Newest import first.
        Newest,
        /// A fresh random sample.
        Random,
        /// One fixed shuffle per seed.
        Shuffle,
        /// Insertion order.
        Natural,
        /// Import time ascending.
        AddedUp,
        /// Import time descending.
        AddedDown,
        /// Title descending.
        TitleDown,
        /// Year ascending.
        YearUp,
        /// Year descending.
        YearDown,
    }
}

droppedneedle_kernel::sql_choice! {
    /// The album filters that steer a statement's plan. The others bind
    /// NULL when unset.
    pub(crate) struct AlbumShape {
        /// One album artist.
        pub artist: bool,
        /// Any of some album artists.
        pub album_artists: bool,
        /// Albums some artists appear on without leading.
        pub appears: bool,
        /// A genre on any streamable track.
        pub genre: bool,
        /// The title or artist pattern, or years.
        pub scoped: bool,
        /// The played filter.
        pub played: PlayedFilter,
    }
}

droppedneedle_kernel::sql_choice! {
    /// A player album page's order outside the play history.
    pub(crate) enum AlbumSequence {
        /// Newest import first.
        Newest,
        /// Title ascending.
        Title,
        /// Album artist, then title.
        Artist,
        /// Year ascending, unknown last.
        Year,
        /// Year descending, unknown last.
        YearDescending,
        /// One fixed shuffle per seed.
        Shuffle,
        /// Insertion order.
        Natural,
        /// Import time ascending.
        AddedUp,
        /// Import time descending.
        AddedDown,
        /// Title descending.
        TitleDown,
        /// Year ascending, unknown first.
        YearUp,
        /// Year descending, unknown last.
        YearDown,
    }
}

droppedneedle_kernel::sql_choice! {
    /// The value a play-history order sorts by.
    pub(crate) enum HistoryKey {
        /// The last play.
        LastPlayed,
        /// The play count.
        PlayCount,
    }
}

droppedneedle_kernel::sql_choice! {
    /// What an id names.
    pub(crate) enum ItemKind {
        /// An artist.
        Artist,
        /// An album.
        Album,
        /// A track.
        Track,
    }
}

impl TrackShape {
    /// No filter but streamability.
    pub(crate) const OPEN: Self = Self {
        text: TextMatch::Off,
        album_artists: false,
        years: false,
        scoped: false,
        played: PlayedFilter::Any,
    };
}

droppedneedle_kernel::sql_choice! {
    /// The tracks a player track statement is narrowed to by an index: the
    /// whole library, one album, one exact title, or both.
    pub(crate) enum TrackKey {
        /// The whole library.
        Open,
        /// One album.
        Album,
        /// One exact title.
        Title,
        /// One exact title on one album.
        AlbumAndTitle,
    }
}

droppedneedle_kernel::sql_choice! {
    /// A [`TrackKey`] that names one album.
    pub(crate) enum AlbumKey {
        /// One album.
        Album,
        /// One exact title on one album.
        AlbumAndTitle,
    }
}

droppedneedle_kernel::sql_choice! {
    /// A [`TrackKey`] other than the whole library.
    pub(crate) enum KeyedBy {
        /// One album.
        Album,
        /// One exact title.
        Title,
        /// One exact title on one album.
        AlbumAndTitle,
    }
}

impl TrackKey {
    /// The key of a query with or without an album and an exact title.
    fn of(album: bool, title: bool) -> Self {
        match (album, title) {
            (false, false) => Self::Open,
            (true, false) => Self::Album,
            (false, true) => Self::Title,
            (true, true) => Self::AlbumAndTitle,
        }
    }

    /// Whether it names one album.
    pub(crate) fn album(self) -> bool {
        matches!(self, Self::Album | Self::AlbumAndTitle)
    }

    /// Whether it names an exact title.
    pub(crate) fn title(self) -> bool {
        matches!(self, Self::Title | Self::AlbumAndTitle)
    }

    /// The narrowing, or `None` for the whole library.
    fn keyed(self) -> Option<KeyedBy> {
        match self {
            Self::Open => None,
            Self::Album => Some(KeyedBy::Album),
            Self::Title => Some(KeyedBy::Title),
            Self::AlbumAndTitle => Some(KeyedBy::AlbumAndTitle),
        }
    }
}

impl From<AlbumKey> for TrackKey {
    fn from(key: AlbumKey) -> Self {
        match key {
            AlbumKey::Album => Self::Album,
            AlbumKey::AlbumAndTitle => Self::AlbumAndTitle,
        }
    }
}

impl From<KeyedBy> for TrackKey {
    fn from(key: KeyedBy) -> Self {
        match key {
            KeyedBy::Album => Self::Album,
            KeyedBy::Title => Self::Title,
            KeyedBy::AlbumAndTitle => Self::AlbumAndTitle,
        }
    }
}

/// The statements of one track query's shape and key, each from its
/// family: the whole library's, one album's or one exact title's.
struct TrackStatements {
    shape: TrackShape,
    key: TrackKey,
}

impl TrackStatements {
    fn page(&self, sequence: TrackSequence) -> String {
        match self.key {
            TrackKey::Open => sql::player_track_page(self.shape, sequence),
            TrackKey::Title => sql::player_title_track_page(self.shape, sequence),
            TrackKey::Album => sql::player_album_track_page(self.shape, AlbumKey::Album, sequence),
            TrackKey::AlbumAndTitle => {
                sql::player_album_track_page(self.shape, AlbumKey::AlbumAndTitle, sequence)
            }
        }
    }

    fn count(&self) -> String {
        match self.key.keyed() {
            None => sql::player_track_count(self.shape),
            Some(key) => sql::player_keyed_track_count(self.shape, key),
        }
    }

    fn unplayed(&self, descending: bool, listed: bool) -> String {
        match self.key.keyed() {
            None => sql::player_tracks_unplayed(self.shape, descending, listed),
            Some(key) => sql::player_keyed_tracks_unplayed(self.shape, key, descending, listed),
        }
    }

    fn unplayed_count(&self) -> String {
        match self.key.keyed() {
            None => sql::player_tracks_unplayed_count(self.shape),
            Some(key) => sql::player_keyed_tracks_unplayed_count(self.shape, key),
        }
    }
}

impl ItemKind {
    /// The kind a protocol names; `None` for anything else.
    fn named(kind: &str) -> Option<Self> {
        match kind {
            "artist" => Some(Self::Artist),
            "album" => Some(Self::Album),
            "track" => Some(Self::Track),
            _ => None,
        }
    }
}

/// How a page is read: one catalog order, or a play-history order.
enum Paging<S> {
    Catalog(S, Option<u32>),
    History(HistoryKey, bool),
}

impl TrackOrder {
    fn paging(self) -> Paging<TrackSequence> {
        let catalog = |sequence| Paging::Catalog(sequence, None);
        match self {
            Self::Album => catalog(TrackSequence::Album),
            Self::Disc => catalog(TrackSequence::Disc),
            Self::Title | Self::By(OrderKey::Title, false) => catalog(TrackSequence::Title),
            Self::Newest => catalog(TrackSequence::Newest),
            Self::Random => catalog(TrackSequence::Random),
            Self::Shuffle(seed) => Paging::Catalog(TrackSequence::Shuffle, Some(seed)),
            Self::Natural => catalog(TrackSequence::Natural),
            Self::By(OrderKey::Added, false) => catalog(TrackSequence::AddedUp),
            Self::By(OrderKey::Added, true) => catalog(TrackSequence::AddedDown),
            Self::By(OrderKey::Title, true) => catalog(TrackSequence::TitleDown),
            Self::By(OrderKey::Year, false) => catalog(TrackSequence::YearUp),
            Self::By(OrderKey::Year, true) => catalog(TrackSequence::YearDown),
            Self::By(OrderKey::LastPlayed, descending) => {
                Paging::History(HistoryKey::LastPlayed, descending)
            }
            Self::By(OrderKey::PlayCount, descending) => {
                Paging::History(HistoryKey::PlayCount, descending)
            }
        }
    }
}

impl AlbumOrder {
    fn paging(self) -> Paging<AlbumSequence> {
        let catalog = |sequence| Paging::Catalog(sequence, None);
        match self {
            Self::Newest => catalog(AlbumSequence::Newest),
            Self::Title | Self::By(OrderKey::Title, false) => catalog(AlbumSequence::Title),
            Self::Artist => catalog(AlbumSequence::Artist),
            Self::YearAsc => catalog(AlbumSequence::Year),
            Self::YearDesc => catalog(AlbumSequence::YearDescending),
            Self::Shuffle(seed) => Paging::Catalog(AlbumSequence::Shuffle, Some(seed)),
            Self::Natural => catalog(AlbumSequence::Natural),
            Self::By(OrderKey::Added, false) => catalog(AlbumSequence::AddedUp),
            Self::By(OrderKey::Added, true) => catalog(AlbumSequence::AddedDown),
            Self::By(OrderKey::Title, true) => catalog(AlbumSequence::TitleDown),
            Self::By(OrderKey::Year, false) => catalog(AlbumSequence::YearUp),
            Self::By(OrderKey::Year, true) => catalog(AlbumSequence::YearDown),
            Self::By(OrderKey::LastPlayed, descending) => {
                Paging::History(HistoryKey::LastPlayed, descending)
            }
            Self::By(OrderKey::PlayCount, descending) => {
                Paging::History(HistoryKey::PlayCount, descending)
            }
        }
    }
}

/// Tag ids per local id, kept only while every value agrees.
#[derive(Default)]
struct Agreement(HashMap<String, Option<String>>);

impl Agreement {
    /// Count one tag value for `id`. A value that is not one well-formed
    /// MBID is skipped.
    fn note(&mut self, id: &str, value: Option<&str>) {
        let Some(value) = value.map(str::trim).filter(|value| is_valid_mbid(value)) else {
            return;
        };
        let value = value.to_ascii_lowercase();
        match self.0.get_mut(id) {
            Some(seen) => {
                if seen.as_deref() != Some(value.as_str()) {
                    *seen = None;
                }
            }
            None => {
                self.0.insert(id.to_owned(), Some(value));
            }
        }
    }

    /// The agreed id for `id`, if any.
    fn agreed(&self, id: &str) -> Option<String> {
        self.0.get(id).cloned().flatten()
    }
}

/// Folded LIKE pattern for a substring filter.
fn like_pattern(raw: &str) -> String {
    let folded = fold_text(raw)
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_");
    format!("%{folded}%")
}

/// A text value.
fn text(value: &str) -> Value {
    Value::Text(value.to_owned())
}

/// A text value, NULL when absent.
fn maybe_text(value: Option<String>) -> Value {
    value.map_or(Value::Null, Value::Text)
}

/// A JSON list of ids, NULL when empty.
fn id_list(ids: &[String]) -> Result<Value, StoreError> {
    if ids.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::to_string(ids)
        .map(Value::Text)
        .map_err(|error| StoreError::Internal(format!("id list: {error}")))
}

/// Text values for some ids.
fn id_values(ids: &[String]) -> Vec<Value> {
    ids.iter().map(|id| text(id)).collect()
}

/// A track query's statements and its filters' binds in text order.
struct TrackFilters {
    statements: TrackStatements,
    values: Vec<Value>,
}

/// A statement's shape and its filters' binds in text order.
struct Filters<S> {
    shape: S,
    values: Vec<Value>,
}

/// The shape and binds of a track query. `played` is the listener's played
/// tracks as a JSON list, read once per request, for the played filter.
fn track_filters(query: &TrackQuery, played: Option<&str>) -> Result<TrackFilters, StoreError> {
    let mut values = Vec::new();
    let text_match = match query.q.as_deref() {
        None => TextMatch::Off,
        // Words of 3+ characters search the FTS index, shorter words LIKE
        // over its candidates; a query with no long word keeps the LIKE
        // substring match.
        Some(q) => match TextSearch::parse(q) {
            Some(search) if search.short_words.len() > 2 => {
                values.push(Value::Text(search.fts));
                values.push(id_list(&search.short_words)?);
                TextMatch::ManyShort
            }
            Some(search) => {
                let matched = match search.short_words.len() {
                    0 => TextMatch::Words,
                    1 => TextMatch::OneShort,
                    _ => TextMatch::TwoShort,
                };
                values.extend(search.binds().into_iter().map(Value::Text));
                matched
            }
            None => {
                let pattern = like_pattern(q);
                values.extend(std::iter::repeat_n(text(&pattern), 3));
                TextMatch::Like
            }
        },
    };
    if let Some(title) = &query.title {
        values.push(Value::Text(fold_text(title.trim())));
    }
    if let Some(album) = &query.album_id {
        values.push(text(album));
    }
    if !query.album_artist_ids.is_empty() {
        values.push(id_list(&query.album_artist_ids)?);
    }
    // Either bound leaves out the tracks with no year, as one comparison
    // would; the other bound is then open.
    let years = query.year_from.is_some() || query.year_to.is_some();
    if years {
        values.extend([
            Value::Integer(query.year_from.unwrap_or(i64::MIN)),
            Value::Integer(query.year_to.unwrap_or(i64::MAX)),
        ]);
    }
    let scoped =
        !query.artist_ids.is_empty() || query.genre.is_some() || query.artist_name.is_some();
    if scoped {
        let artists = id_list(&query.artist_ids)?;
        values.extend([artists.clone(), artists]);
        let genre = maybe_text(query.genre.as_deref().map(|genre| fold_text(genre.trim())));
        values.extend(std::iter::repeat_n(genre, 3));
        let name = maybe_text(
            query
                .artist_name
                .as_deref()
                .map(|name| fold_text(name.trim())),
        );
        values.extend(std::iter::repeat_n(name, 3));
    }
    let played = played_filter(query.played, played, &mut values);
    Ok(TrackFilters {
        statements: TrackStatements {
            shape: TrackShape {
                text: text_match,
                album_artists: !query.album_artist_ids.is_empty(),
                years,
                scoped,
                played,
            },
            key: TrackKey::of(query.album_id.is_some(), query.title.is_some()),
        },
        values,
    })
}

/// The played filter, its list pushed onto `values` when it filters.
fn played_filter(
    wanted: Option<bool>,
    played: Option<&str>,
    values: &mut Vec<Value>,
) -> PlayedFilter {
    match (wanted, played) {
        (Some(wanted), Some(played)) => {
            values.push(text(played));
            if wanted {
                PlayedFilter::Played
            } else {
                PlayedFilter::Unplayed
            }
        }
        _ => PlayedFilter::Any,
    }
}

/// The shape and binds of an album query. `played` is the listener's
/// played albums as a JSON list, read once per request, for the played
/// filter.
fn album_filters(
    query: &AlbumQuery,
    played: Option<&str>,
) -> Result<Filters<AlbumShape>, StoreError> {
    let mut values = Vec::new();
    if let Some(artist) = &query.artist_id {
        values.push(text(artist));
    }
    if !query.album_artist_ids.is_empty() {
        values.push(id_list(&query.album_artist_ids)?);
    }
    let scoped = query.q.is_some() || query.year_from.is_some() || query.year_to.is_some();
    if scoped {
        let pattern = maybe_text(query.q.as_deref().map(like_pattern));
        values.extend(std::iter::repeat_n(pattern, 3));
        let from = query.year_from.map_or(Value::Null, Value::Integer);
        let to = query.year_to.map_or(Value::Null, Value::Integer);
        values.extend([from.clone(), from, to.clone(), to]);
    }
    if !query.appears_on.is_empty() {
        let appears = id_list(&query.appears_on)?;
        values.extend([appears.clone(), appears]);
    }
    let genre = query.genre.as_deref().map(|genre| fold_text(genre.trim()));
    if let Some(genre) = &genre {
        values.extend([text(genre), text(genre)]);
    }
    let played = played_filter(query.played, played, &mut values);
    Ok(Filters {
        shape: AlbumShape {
            artist: query.artist_id.is_some(),
            album_artists: !query.album_artist_ids.is_empty(),
            appears: !query.appears_on.is_empty(),
            genre: genre.is_some(),
            scoped,
            played,
        },
        values,
    })
}

/// No filter beyond streamability and the played filter: the total comes
/// from the maintained count and the listener's history.
fn track_query_is_open(query: &TrackQuery) -> bool {
    query.q.is_none()
        && query.title.is_none()
        && query.album_id.is_none()
        && query.artist_ids.is_empty()
        && query.album_artist_ids.is_empty()
        && query.genre.is_none()
        && query.year_from.is_none()
        && query.year_to.is_none()
        && query.artist_name.is_none()
}

/// No filter beyond "has a streamable track".
fn album_query_is_open(query: &AlbumQuery) -> bool {
    query.played.is_none()
        && query.q.is_none()
        && query.artist_id.is_none()
        && query.album_artist_ids.is_empty()
        && query.appears_on.is_empty()
        && query.genre.is_none()
        && query.year_from.is_none()
        && query.year_to.is_none()
}

/// `parts` joined into one bind list.
fn joined(parts: &[&[Value]]) -> Vec<Value> {
    parts.concat()
}

/// The statements of one play-history order and their binds.
struct HistoryRead {
    /// The played rows in order. Binds: `values`, the filters, the page.
    played: String,
    /// The unplayed rows by id. Binds: the filters, `played_ids`, the page.
    unplayed: String,
    /// The same with the played ids handed in. Binds: the filters, the
    /// list, the page.
    unplayed_listed: String,
    /// How many played rows pass. Binds: the list, the filters.
    played_count: String,
    /// How many unplayed rows pass. Binds: the filters, the list.
    unplayed_count: String,
    /// The ids of the rows the listener has values for.
    played_ids: Statement,
    /// The listener as the values bind it.
    values: Vec<Value>,
    /// The listener as the played ids bind it.
    ids: Vec<Value>,
    /// The filters' binds.
    filters: Vec<Value>,
}

/// The player track projection's row.
fn track_row(row: &Row<'_>) -> rusqlite::Result<PlayerTrack> {
    let genres: Option<String> = row.get("genres")?;
    let tag_genre: Option<String> = row.get("tag_genre")?;
    let genres = match genres.filter(|value| !value.is_empty()) {
        Some(list) => list.split('\u{1f}').map(str::to_owned).collect(),
        None => tag_genre
            .into_iter()
            .filter(|genre| !genre.trim().is_empty())
            .collect(),
    };
    Ok(PlayerTrack {
        id: row.get("id")?,
        title: row.get("title")?,
        album_id: row.get("album_id")?,
        album_title: row.get("album_title")?,
        artist_name: row.get("artist_name")?,
        artist_id: row.get("artist_id")?,
        album_artist_id: row.get("album_artist_id")?,
        album_artist_name: row.get("album_artist_name")?,
        disc_number: row.get("disc_number")?,
        track_number: row.get("track_number")?,
        year: row.get("year")?,
        genres,
        duration_seconds: row.get("duration_seconds")?,
        format: row.get("format")?,
        bit_rate: row.get("bit_rate")?,
        sample_rate: row.get("sample_rate")?,
        bit_depth: row.get("bit_depth")?,
        channels: row.get("channels")?,
        file_size_bytes: row.get("file_size_bytes")?,
        relative_path: row.get("relative_path")?,
        date_added: row.get("date_added")?,
        sort_name: row.get("sort_name")?,
        disc_subtitle: row.get("disc_subtitle")?,
        recording_mbid: row.get("recording_mbid")?,
        artist_mbid: row.get("artist_mbid")?,
        album_artist_mbid: row.get("album_artist_mbid")?,
        release_group_mbid: row.get("release_group_mbid")?,
        release_type: row.get("release_type")?,
        replaygain_track_gain: row.get("rg_track_gain")?,
        replaygain_album_gain: row.get("rg_album_gain")?,
        replaygain_track_peak: row.get("rg_track_peak")?,
        replaygain_album_peak: row.get("rg_album_peak")?,
        cover_available: row.get("cover_available")?,
        art_version: row.get("art_version")?,
        has_lyrics: row.get("has_lyrics")?,
    })
}

/// The player album projection's row. Players are never shown a
/// contribution.
fn player_album_row(row: &Row<'_>) -> rusqlite::Result<PlayerAlbum> {
    Ok(PlayerAlbum {
        record: AlbumRecord {
            contribution_id: None,
            contribution_state: None,
            ..album_row(row)?
        },
        genre: row.get("primary_genre")?,
        sort_name: row.get("album_sort_name")?,
        original_release_date: row.get("original_release_date")?,
        release_date: row.get("release_date")?,
    })
}

/// The first column as text.
fn first_text(row: &Row<'_>) -> rusqlite::Result<String> {
    row.get(0)
}

/// Player reads.
impl LocalCatalog {
    /// The ids `text` answers over `values`, in one read named `name`.
    async fn ids(
        &self,
        name: &'static str,
        text: String,
        values: Vec<Value>,
    ) -> Result<Vec<String>, StoreError> {
        self.read(name, move |conn| rows(conn, &text, &values, first_text))
            .await
    }

    /// The count `text` answers over `values`, in one read named `name`.
    async fn counted(
        &self,
        name: &'static str,
        text: String,
        values: Vec<Value>,
    ) -> Result<u64, StoreError> {
        self.read(name, move |conn| scalar::<i64>(conn, &text, &values))
            .await
            .map(unsigned)
    }

    /// Tracks by id, in no particular order; unknown or unstreamable ids
    /// are absent.
    pub async fn tracks_by_ids(&self, ids: &[String]) -> Result<Vec<PlayerTrack>, StoreError> {
        Ok(self.tracks_in(ids).await?.into_values().collect())
    }

    async fn tracks_in(&self, ids: &[String]) -> Result<HashMap<String, PlayerTrack>, StoreError> {
        let mut out = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(500) {
            let values = id_values(chunk);
            let tracks = self
                .read("player.tracks_in", move |conn| {
                    rows(
                        conn,
                        &sql::player_tracks_in(ListLen::new(values.len())),
                        &values,
                        track_row,
                    )
                })
                .await?;
            out.extend(tracks.into_iter().map(|track| (track.id.clone(), track)));
        }
        Ok(out)
    }

    /// The rich rows for `ids`, in `ids` order.
    async fn hydrate_tracks(&self, ids: &[String]) -> Result<Vec<PlayerTrack>, StoreError> {
        let mut by_id = self.tracks_in(ids).await?;
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// One page of tracks plus the match total.
    pub async fn tracks(
        &self,
        query: &TrackQuery,
        order: TrackOrder,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<PlayerTrack>, u64), StoreError> {
        let played = self.played_tracks(query).await?;
        let played = played.as_deref();
        let Some(ids) = self.track_ids(query, played, order, limit, offset).await? else {
            return Ok((Vec::new(), 0));
        };
        let tracks = self.hydrate_tracks(&ids).await?;
        Ok((tracks, self.track_total(query, played).await?))
    }

    /// One page of tracks without the total, for callers that never show
    /// it (counting a text search costs more than the page).
    pub async fn track_page(
        &self,
        query: &TrackQuery,
        order: TrackOrder,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<PlayerTrack>, StoreError> {
        let played = self.played_tracks(query).await?;
        match self
            .track_ids(query, played.as_deref(), order, limit, offset)
            .await?
        {
            Some(ids) => self.hydrate_tracks(&ids).await,
            None => Ok(Vec::new()),
        }
    }

    /// The listener's played tracks as a JSON list, when `query` filters by
    /// them: one read per request, for the page and its total.
    async fn played_tracks(&self, query: &TrackQuery) -> Result<Option<String>, StoreError> {
        if query.played.is_none() {
            return Ok(None);
        }
        let listener = text(query.listener.as_deref().unwrap_or_default());
        let ids = self
            .ids(
                "player.tracks.played",
                sql::PLAYER_PLAYED_TRACKS.text().to_owned(),
                vec![listener; 3],
            )
            .await?;
        serde_json::to_string(&ids)
            .map(Some)
            .map_err(|error| StoreError::Internal(format!("played tracks: {error}")))
    }

    /// One page of track ids in order, or `None` when the text search
    /// matches nothing at all (the miss oracle).
    async fn track_ids(
        &self,
        query: &TrackQuery,
        played: Option<&str>,
        order: TrackOrder,
        limit: u64,
        offset: u64,
    ) -> Result<Option<Vec<String>>, StoreError> {
        // Miss oracle: a query no track text contains matches nothing,
        // whatever the other filters say.
        if let Some(search) = query.q.as_deref().and_then(TextSearch::parse) {
            let values = vec![Value::Text(search.fts)];
            let hit: bool = self
                .read("player.tracks.search", move |conn| {
                    scalar(conn, sql::TRACK_MISS.text(), &values)
                })
                .await?;
            if !hit {
                return Ok(None);
            }
        }
        let filters = track_filters(query, played)?;
        let page = [count(limit), count(offset)];
        match order.paging() {
            Paging::History(key, descending) => {
                let statements = &filters.statements;
                let (shape, track) = (statements.shape, statements.key);
                let listener = text(query.listener.as_deref().unwrap_or_default());
                let read = HistoryRead {
                    played: sql::player_tracks_played(shape, track, key, descending),
                    unplayed: statements.unplayed(descending, false),
                    unplayed_listed: statements.unplayed(descending, true),
                    played_count: sql::player_tracks_played_count(shape, track),
                    unplayed_count: statements.unplayed_count(),
                    played_ids: sql::PLAYER_PLAYED_TRACKS,
                    values: vec![listener.clone(); 2],
                    ids: vec![listener; 3],
                    filters: filters.values,
                };
                self.history_page(read, descending, limit, offset)
                    .await
                    .map(Some)
            }
            Paging::Catalog(sequence, seed) => {
                let seed: Vec<Value> = seed
                    .map(|seed| sql::shuffle_values(seed).to_vec())
                    .unwrap_or_default();
                self.ids(
                    "player.tracks",
                    filters.statements.page(sequence),
                    joined(&[&seed, &filters.values, &page]),
                )
                .await
                .map(Some)
            }
        }
    }

    /// How many tracks match `query`.
    async fn track_total(
        &self,
        query: &TrackQuery,
        played: Option<&str>,
    ) -> Result<u64, StoreError> {
        if !track_query_is_open(query) {
            let filters = track_filters(query, played)?;
            let statements = &filters.statements;
            let text = statements.count();
            // Plays do not move the catalog revision, so a played filter
            // is counted every time.
            if played.is_some() {
                return self
                    .counted("player.tracks.count", text, filters.values)
                    .await;
            }
            let key = format!(
                "player.tracks {:?} {:?} {:?}",
                statements.shape, statements.key, filters.values
            );
            return self
                .remembered("player.tracks.count", key, text, filters.values)
                .await;
        }
        // Maintained exact count of indexed tracks (migration 0005).
        let all = self
            .counted(
                "player.tracks.count",
                sql::TRACK_TOTAL.text().to_owned(),
                Vec::new(),
            )
            .await?;
        match (query.played, played) {
            // Counted from the listener's played tracks, not across the
            // library: one probe per played track.
            (Some(wanted), Some(played)) => {
                let played = self
                    .counted(
                        "player.tracks.count",
                        sql::PLAYER_LISTED_TRACK_COUNT.text().to_owned(),
                        vec![text(played)],
                    )
                    .await?;
                Ok(if wanted {
                    played
                } else {
                    all.saturating_sub(played)
                })
            }
            _ => Ok(all),
        }
    }

    /// The played ids of a play-history order as a JSON list.
    async fn played_list(&self, read: &HistoryRead) -> Result<Value, StoreError> {
        let ids = self
            .ids(
                "player.history_page.played",
                read.played_ids.text().to_owned(),
                read.ids.clone(),
            )
            .await?;
        serde_json::to_string(&ids)
            .map(Value::Text)
            .map_err(|error| StoreError::Internal(format!("played ids: {error}")))
    }

    /// One page of ids in a play-history order, in two reads and never one
    /// sort of the whole library: what the listener played, in the order
    /// of their plays, and everything else by id, read only when the page
    /// reaches it. Descending puts the played rows first, ascending last,
    /// as one sort with the never played at zero would. The page past the
    /// first half needs that half's size: known from a short page, counted
    /// only when the page starts beyond it, from the played ids read once
    /// for the count and the half after it.
    async fn history_page(
        &self,
        read: HistoryRead,
        descending: bool,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<String>, StoreError> {
        const NAME: &str = "player.history_page";
        const COUNT: &str = "player.history_page.count";
        let filters = &read.filters;
        let page = |limit: u64, offset: u64| [count(limit), count(offset)];
        if descending {
            let mut ids = self
                .ids(
                    NAME,
                    read.played.clone(),
                    joined(&[&read.values, filters, &page(limit, offset)]),
                )
                .await?;
            let got = ids.len() as u64;
            if got >= limit {
                return Ok(ids);
            }
            let rest = if got > 0 || offset == 0 {
                self.ids(
                    NAME,
                    read.unplayed.clone(),
                    joined(&[filters, &read.ids, &page(limit - got, 0)]),
                )
                .await?
            } else {
                let list = self.played_list(&read).await?;
                let played = self
                    .counted(
                        COUNT,
                        read.played_count.clone(),
                        joined(&[std::slice::from_ref(&list), filters]),
                    )
                    .await?;
                self.ids(
                    NAME,
                    read.unplayed_listed.clone(),
                    joined(&[
                        filters,
                        &[list],
                        &page(limit, offset.saturating_sub(played)),
                    ]),
                )
                .await?
            };
            ids.extend(rest);
            Ok(ids)
        } else {
            let mut ids = self
                .ids(
                    NAME,
                    read.unplayed.clone(),
                    joined(&[filters, &read.ids, &page(limit, offset)]),
                )
                .await?;
            let got = ids.len() as u64;
            if got >= limit {
                return Ok(ids);
            }
            let unplayed = if got > 0 || offset == 0 {
                offset + got
            } else {
                let list = self.played_list(&read).await?;
                self.counted(
                    COUNT,
                    read.unplayed_count.clone(),
                    joined(&[filters, &[list]]),
                )
                .await?
            };
            let rest = self
                .ids(
                    NAME,
                    read.played.clone(),
                    joined(&[
                        &read.values,
                        filters,
                        &page(limit - got, offset.saturating_sub(unplayed)),
                    ]),
                )
                .await?;
            ids.extend(rest);
            Ok(ids)
        }
    }

    /// Albums by id, in no particular order.
    pub async fn albums_by_ids(&self, ids: &[String]) -> Result<Vec<PlayerAlbum>, StoreError> {
        Ok(self.albums_in(ids).await?.into_values().collect())
    }

    async fn albums_in(&self, ids: &[String]) -> Result<HashMap<String, PlayerAlbum>, StoreError> {
        let mut out = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(500) {
            let values = id_values(chunk);
            let albums = self
                .read("player.albums", move |conn| {
                    rows(
                        conn,
                        &sql::player_albums_in(ListLen::new(values.len())),
                        &values,
                        player_album_row,
                    )
                })
                .await?;
            out.extend(
                albums
                    .into_iter()
                    .map(|album| (album.record.id.clone(), album)),
            );
        }
        Ok(out)
    }

    /// The listener's played albums as a JSON list, and how many there
    /// are, when `query` filters by them: one read per request, for the
    /// page and its total.
    async fn played_albums(&self, query: &AlbumQuery) -> Result<Option<(String, u64)>, StoreError> {
        if query.played.is_none() {
            return Ok(None);
        }
        let listener = text(query.listener.as_deref().unwrap_or_default());
        let ids = self
            .ids(
                "player.albums.played",
                sql::PLAYER_PLAYED_ALBUMS.text().to_owned(),
                vec![listener],
            )
            .await?;
        let json = serde_json::to_string(&ids)
            .map_err(|error| StoreError::Internal(format!("played albums: {error}")))?;
        Ok(Some((json, ids.len() as u64)))
    }

    /// One page of albums, `played` being what [`Self::played_albums`]
    /// read.
    async fn album_page_in(
        &self,
        query: &AlbumQuery,
        played: Option<&str>,
        order: AlbumOrder,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<PlayerAlbum>, StoreError> {
        let filters = album_filters(query, played)?;
        let ids = match order.paging() {
            Paging::History(key, descending) => {
                let shape = filters.shape;
                let listener = text(query.listener.as_deref().unwrap_or_default());
                let read = HistoryRead {
                    played: sql::player_albums_played(shape, key, descending),
                    unplayed: sql::player_albums_unplayed(shape, descending, false),
                    unplayed_listed: sql::player_albums_unplayed(shape, descending, true),
                    played_count: sql::player_albums_played_count(shape),
                    unplayed_count: sql::player_albums_unplayed_count(shape),
                    played_ids: sql::PLAYER_ALBUM_PLAYED_IDS,
                    values: vec![listener.clone()],
                    ids: vec![listener],
                    filters: filters.values,
                };
                self.history_page(read, descending, limit, offset).await?
            }
            Paging::Catalog(sequence, seed) => {
                let seed: Vec<Value> = seed
                    .map(|seed| sql::shuffle_values(seed).to_vec())
                    .unwrap_or_default();
                self.ids(
                    "player.albums.page",
                    sql::player_album_page(filters.shape, sequence),
                    joined(&[&seed, &filters.values, &[count(limit), count(offset)]]),
                )
                .await?
            }
        };
        let mut by_id = self.albums_in(&ids).await?;
        Ok(ids.iter().filter_map(|id| by_id.remove(id)).collect())
    }

    /// The browseable album total, remembered at one catalog revision:
    /// counting it probes every album, and the open album list asks for it
    /// on every page.
    async fn open_album_total(&self) -> Result<u64, StoreError> {
        let filters = album_filters(&AlbumQuery::default(), None)?;
        self.remembered(
            "player.albums.count",
            "player.albums.open".to_owned(),
            sql::player_album_count(filters.shape),
            filters.values,
        )
        .await
    }

    /// The count `text` answers over `values`, remembered under `key` at
    /// one catalog revision, so a list's later pages reuse its first
    /// page's count. The store keeps at most 256 totals per revision.
    async fn remembered(
        &self,
        name: &'static str,
        key: String,
        text: String,
        values: Vec<Value>,
    ) -> Result<u64, StoreError> {
        let counted = self
            .totals(key, name, move |conn| {
                Ok(vec![unsigned(scalar(conn, &text, &values)?)])
            })
            .await?;
        Ok(counted.first().copied().unwrap_or(0))
    }

    /// One page of albums holding at least one streamable track, plus the
    /// match total.
    pub async fn albums(
        &self,
        query: &AlbumQuery,
        order: AlbumOrder,
        limit: u64,
        offset: u64,
    ) -> Result<(Vec<PlayerAlbum>, u64), StoreError> {
        let played = self.played_albums(query).await?;
        let json = played.as_ref().map(|(json, _)| json.as_str());
        let albums = self
            .album_page_in(query, json, order, limit, offset)
            .await?;
        let unfiltered = AlbumQuery {
            played: None,
            ..query.clone()
        };
        let total = if album_query_is_open(query) {
            self.open_album_total().await?
        } else if let (Some(wanted), Some((_, count))) = (query.played, &played)
            && album_query_is_open(&unfiltered)
        {
            // Every played album has a streamable track, so the rest of
            // the open total is unplayed.
            if wanted {
                *count
            } else {
                self.open_album_total().await?.saturating_sub(*count)
            }
        } else if json.is_some() {
            // Plays do not move the catalog revision, so a played filter
            // is counted every time.
            let filters = album_filters(query, json)?;
            self.counted(
                "player.albums.count",
                sql::player_album_count(filters.shape),
                filters.values,
            )
            .await?
        } else {
            let filters = album_filters(query, None)?;
            let key = format!("player.albums {:?} {:?}", filters.shape, filters.values);
            self.remembered(
                "player.albums.count",
                key,
                sql::player_album_count(filters.shape),
                filters.values,
            )
            .await?
        };
        Ok((albums, total))
    }

    /// One page of albums without the total.
    pub async fn album_page(
        &self,
        query: &AlbumQuery,
        order: AlbumOrder,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<PlayerAlbum>, StoreError> {
        let played = self.played_albums(query).await?;
        let json = played.as_ref().map(|(json, _)| json.as_str());
        self.album_page_in(query, json, order, limit, offset).await
    }

    /// One page of album artists without the total: name order, or
    /// insertion order when `natural` (the search3 empty-query sync, stable
    /// while a scan adds artists). `q` narrows by folded name substring.
    pub async fn album_artist_page(
        &self,
        q: Option<&str>,
        natural: bool,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<ArtistRecord>, StoreError> {
        let pattern = maybe_text(q.map(like_pattern));
        let ids = self
            .ids(
                "player.album_artists",
                sql::player_album_artist_page(natural),
                vec![pattern.clone(), pattern, count(limit), count(offset)],
            )
            .await?;
        self.hydrate_artists(&ids).await
    }

    /// Which of `ids` exist, for one kind (`artist`, `album`, `track`).
    pub async fn existing(
        &self,
        kind: &str,
        ids: &[String],
    ) -> Result<HashSet<String>, StoreError> {
        let Some(kind) = ItemKind::named(kind) else {
            return Ok(HashSet::new());
        };
        let mut out = HashSet::new();
        for chunk in ids.chunks(500) {
            out.extend(
                self.ids(
                    "player.existing",
                    sql::player_existing(kind, ListLen::new(chunk.len())),
                    id_values(chunk),
                )
                .await?,
            );
        }
        Ok(out)
    }

    /// Catalog revision, bumped by every catalog write.
    pub async fn revision(&self) -> Result<i64, StoreError> {
        self.read("player.revision", |conn| {
            scalar(conn, sql::CATALOG_REVISION.text(), &[])
        })
        .await
    }

    /// Album ids from the user's play history: most recent first, or most
    /// played first when `frequent`. An album with no streamable track left
    /// is not listed.
    pub async fn history_albums(
        &self,
        user_id: &str,
        frequent: bool,
        limit: u64,
        offset: u64,
    ) -> Result<Vec<String>, StoreError> {
        self.ids(
            "player.history_albums",
            sql::player_history_albums(frequent),
            vec![text(user_id), text(user_id), count(limit), count(offset)],
        )
        .await
    }

    /// The listener's [`PlayedValues`] for some ids of one kind (`album`,
    /// `track`). An item with a play count of 0 is absent.
    pub async fn played_values(
        &self,
        user_id: &str,
        kind: &str,
        ids: &[String],
    ) -> Result<HashMap<String, PlayedValues>, StoreError> {
        let album = match ItemKind::named(kind) {
            Some(ItemKind::Track) => false,
            Some(ItemKind::Album) => true,
            _ => return Ok(HashMap::new()),
        };
        let mut out = HashMap::new();
        for chunk in ids.chunks(250) {
            let len = ListLen::new(chunk.len());
            let half = joined(&[&[text(user_id)], &id_values(chunk)]);
            let (statement, values) = if album {
                (sql::player_album_values(len), half)
            } else {
                (sql::player_track_values(len), joined(&[&half, &half]))
            };
            let values = self
                .read("player.played_values", move |conn| {
                    rows(conn, &statement, &values, |row| {
                        let plays: i64 = row.get("plays")?;
                        let last: Option<i64> = row.get("played_at")?;
                        let played = if album { row.get("played")? } else { plays > 0 };
                        Ok((
                            row.get::<_, String>("id")?,
                            PlayedValues {
                                plays: unsigned(plays),
                                last_played: last.map(|at| at as f64),
                                played,
                            },
                        ))
                    })
                })
                .await?;
            out.extend(values);
        }
        Ok(out)
    }

    /// The user's play counts and last plays for some ids of one kind
    /// (`artist`, `album`, `track`).
    pub async fn play_stats(
        &self,
        user_id: &str,
        kind: &str,
        ids: &[String],
    ) -> Result<HashMap<String, PlayStat>, StoreError> {
        let Some(kind) = ItemKind::named(kind) else {
            return Ok(HashMap::new());
        };
        let mut out = HashMap::new();
        for chunk in ids.chunks(500) {
            let statement = sql::player_play_stats(kind, ListLen::new(chunk.len()));
            // An album's plays are read in two parts, each with its own
            // listener and ids.
            let half = joined(&[&[text(user_id)], &id_values(chunk)]);
            let values = if kind == ItemKind::Album {
                joined(&[&half, &half])
            } else {
                half
            };
            let stats = self
                .read("player.play_stats", move |conn| {
                    rows(conn, &statement, &values, |row| {
                        let plays: i64 = row.get("plays")?;
                        Ok((
                            row.get::<_, String>("id")?,
                            (unsigned(plays), row.get::<_, Option<f64>>("last_played")?),
                        ))
                    })
                })
                .await?;
            out.extend(stats);
        }
        Ok(out)
    }

    /// The MusicBrainz ids player apps are sent for some albums and
    /// artists: the accepted match's, else the one their files' tags agree
    /// on. An album's tags are its indexed tracks'; an artist's are the ids
    /// its track and album credits took from their files, which the scan
    /// keeps only where a file pairs artist names and ids one for one, so a
    /// tag holding several ids for one artist row gives none. A value that
    /// is not one well-formed MBID is skipped. Tag ids are for player
    /// answers only, never identity or matching.
    pub async fn player_ids(
        &self,
        album_ids: &[String],
        artist_ids: &[String],
    ) -> Result<PlayerIds, StoreError> {
        type AlbumIdRow = (
            String,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        );
        let mut out = PlayerIds::default();
        for chunk in album_ids.chunks(500) {
            let values = id_values(chunk);
            let found: Vec<AlbumIdRow> = self
                .read("player.player_ids", move |conn| {
                    rows(
                        conn,
                        &sql::player_album_ids(ListLen::new(values.len())),
                        &values,
                        |row| {
                            Ok((
                                row.get(0)?,
                                row.get(1)?,
                                row.get(2)?,
                                row.get(3)?,
                                row.get(4)?,
                            ))
                        },
                    )
                })
                .await?;
            let (mut releases, mut groups) = (Agreement::default(), Agreement::default());
            for (id, release, group, tag_release, tag_group) in found {
                if group.is_some() {
                    out.albums.insert(
                        id,
                        AlbumIds {
                            accepted: true,
                            release_mbid: release,
                            release_group_mbid: group,
                        },
                    );
                } else {
                    releases.note(&id, tag_release.as_deref());
                    groups.note(&id, tag_group.as_deref());
                }
            }
            for id in chunk {
                let (release, group) = (releases.agreed(id), groups.agreed(id));
                if release.is_some() || group.is_some() {
                    out.albums.insert(
                        id.clone(),
                        AlbumIds {
                            accepted: false,
                            release_mbid: release,
                            release_group_mbid: group,
                        },
                    );
                }
            }
        }
        for chunk in artist_ids.chunks(500) {
            let values = id_values(chunk);
            let accepted: Vec<(String, String)> = self
                .read("player.player_ids", move |conn| {
                    rows(
                        conn,
                        &sql::player_artist_ids(ListLen::new(values.len())),
                        &values,
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                })
                .await?;
            out.artists.extend(accepted);
            let unmatched: Vec<String> = chunk
                .iter()
                .filter(|id| !out.artists.contains_key(*id))
                .cloned()
                .collect();
            if unmatched.is_empty() {
                continue;
            }
            let values = joined(&[&id_values(&unmatched), &id_values(&unmatched)]);
            let len = ListLen::new(unmatched.len());
            let tagged: Vec<(String, String)> = self
                .read("player.player_ids", move |conn| {
                    rows(conn, &sql::artist_tag_ids(len), &values, |row| {
                        Ok((row.get(0)?, row.get(1)?))
                    })
                })
                .await?;
            let mut tags = Agreement::default();
            for (id, value) in &tagged {
                tags.note(id, Some(value));
            }
            for id in unmatched {
                if let Some(mbid) = tags.agreed(&id) {
                    out.artists.insert(id, mbid);
                }
            }
        }
        Ok(out)
    }

    /// The id an album, artist or track (`kind`) has now: `id` itself while
    /// it is live, else the live one a merge or an alias of any kind points
    /// it at. `None` for an album or artist nothing live holds; a track
    /// answers its own id then, so reads test its availability as usual.
    /// One indexed read.
    pub async fn current_id(&self, kind: &str, id: &str) -> Result<Option<String>, StoreError> {
        match ItemKind::named(kind) {
            Some(ItemKind::Album) => self.current_album(id).await,
            Some(ItemKind::Artist) => self.current_artist(id).await,
            Some(ItemKind::Track) => {
                let values = vec![text(id)];
                self.read("library.lookups.direct", move |conn| {
                    first(conn, sql::CURRENT_TRACK_ID.text(), &values, first_text)
                })
                .await
            }
            None => Ok(None),
        }
    }

    /// Every year at least one album with a streamable track has,
    /// ascending.
    pub async fn album_years(&self) -> Result<Vec<i64>, StoreError> {
        self.read("player.album_years", |conn| {
            rows(conn, sql::PLAYER_ALBUM_YEARS.text(), &[], |row| row.get(0))
        })
        .await
    }

    /// For some artist ids, the summed length in seconds and the number of
    /// the streamable tracks crediting each one as track artist or album
    /// artist, each track once. An artist with no such track is absent.
    pub async fn artist_durations(&self, ids: &[String]) -> Result<ArtistTracks, StoreError> {
        let mut out = HashMap::with_capacity(ids.len());
        for chunk in ids.chunks(500) {
            let len = ListLen::new(chunk.len());
            let values = joined(&[&id_values(chunk), &id_values(chunk)]);
            let found: Vec<(String, f64, i64)> = self
                .read("player.artist_durations", move |conn| {
                    rows(conn, &sql::player_artist_durations(len), &values, |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?))
                    })
                })
                .await?;
            out.extend(
                found
                    .into_iter()
                    .map(|(id, seconds, count)| (id, (seconds, unsigned(count)))),
            );
        }
        Ok(out)
    }
}

/// Seeds for paged random lists, one per caller, kind and filter. The first
/// page (offset 0) draws a new seed and later pages reuse it, so a client
/// paging through "random" sees one shuffle with no repeats or gaps, and a
/// random track list never reshuffles a random album list. Seeds idle for
/// an hour are dropped once the map grows.
#[derive(Debug, Default)]
pub struct ShuffleSeeds {
    seeds: Mutex<HashMap<SeedKey, (u32, std::time::Instant)>>,
}

/// Caller, list kind (`track`/`album`) and a hash of the filter.
type SeedKey = (String, &'static str, u64);

impl ShuffleSeeds {
    /// Most lists remembered; past this the idle ones go, then the oldest.
    const CAP: usize = 1024;
    /// Idle time after which a seed may be pruned.
    const IDLE: std::time::Duration = std::time::Duration::from_secs(3600);

    /// The seed for a random track list at `offset`.
    pub fn tracks(&self, caller: &str, query: &TrackQuery, offset: u64) -> u32 {
        self.seed(caller, "track", &format!("{query:?}"), offset)
    }

    /// The seed for a random album list at `offset`.
    pub fn albums(&self, caller: &str, query: &AlbumQuery, offset: u64) -> u32 {
        self.seed(caller, "album", &format!("{query:?}"), offset)
    }

    fn seed(&self, caller: &str, kind: &'static str, filter: &str, offset: u64) -> u32 {
        use std::hash::{Hash as _, Hasher as _};

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        filter.hash(&mut hasher);
        let key = (caller.to_owned(), kind, hasher.finish());
        let now = std::time::Instant::now();
        let mut seeds = self
            .seeds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !seeds.contains_key(&key) && seeds.len() >= Self::CAP {
            seeds.retain(|_, (_, used)| now.duration_since(*used) < Self::IDLE);
            while seeds.len() >= Self::CAP {
                let oldest = seeds
                    .iter()
                    .min_by_key(|(_, (_, used))| *used)
                    .map(|(key, _)| key.clone());
                match oldest {
                    Some(oldest) => seeds.remove(&oldest),
                    None => break,
                };
            }
        }
        let fresh = || uuid::Uuid::new_v4().as_u128() as u32;
        let entry = seeds.entry(key).or_insert_with(|| (fresh(), now));
        if offset == 0 {
            entry.0 = fresh();
        }
        entry.1 = now;
        entry.0
    }
}

#[cfg(test)]
mod shuffle_seed_tests {
    use super::*;

    #[test]
    fn seed_map_stays_capped() {
        let seeds = ShuffleSeeds::default();
        for caller in 0..ShuffleSeeds::CAP + 10 {
            seeds.tracks(&caller.to_string(), &TrackQuery::default(), 0);
        }
        let held = seeds
            .seeds
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .len();
        assert_eq!(held, ShuffleSeeds::CAP);
    }
}
