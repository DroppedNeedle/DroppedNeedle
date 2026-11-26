//! The kept facts' tables: what readers read, what fetches keep, what the
//! refresh job plans from and the background read meter.

use std::collections::HashMap;
use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::queue::JobError;
use rusqlite::{Connection, params};

use super::{DAY_MS, FactKind, FactSource, ListSource, MeteredSource, Outcome, WantOrder};
use crate::discover::listening::band_links::BandLink;
use crate::discover::listening::{AlbumFacts, AlbumRow, RankedAlbum};
use crate::discover::sql;

/// How long a found similar-artists list, album list or album count is
/// kept before it is read again: they move over weeks.
const LIST_AGE_MS: i64 = 14 * DAY_MS;
/// How long a found listener count is kept: it moves over months.
const COUNT_AGE_MS: i64 = 30 * DAY_MS;
/// How long a clean "nothing there" stands, an album list's apart.
const NONE_AGE_MS: i64 = DAY_MS;
/// How long an artist with no album to offer stands.
const NO_ALBUMS_AGE_MS: i64 = 14 * DAY_MS;
/// How long found band links are kept: a band's members change over years.
const LINKS_AGE_MS: i64 = 90 * DAY_MS;
/// How long an artist with no band links stands.
const NO_LINKS_AGE_MS: i64 = 30 * DAY_MS;
/// The first retry after a failure; each one in a row doubles it.
const RETRY_MS: i64 = 30 * 60_000;
/// The longest a failing fact waits for its next try.
const RETRY_CAP_MS: i64 = DAY_MS;
/// When a discography still being walked is asked again.
const NOT_YET_MS: i64 = 15 * 60_000;
/// A fact nothing has read for this long is dropped.
const UNREAD_DROP_MS: i64 = 180 * DAY_MS;
/// How long the read counter keeps a day.
const METER_DAYS: i64 = 90;

/// What is kept for one fact.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Kept<T> {
    /// Never answered: a reader gets "not yet".
    Missing,
    /// Answered with nothing there.
    Nothing,
    /// The kept answer.
    Found(T),
}

/// One fact as read, with what the reader's note needs.
#[derive(Debug, Clone)]
pub(crate) struct Looked<T> {
    /// What is kept.
    pub kept: Kept<T>,
    /// When it last answered, unix ms.
    pub fetched_at: Option<i64>,
    read_at: Option<i64>,
    wanted_at: Option<i64>,
}

impl<T> Looked<T> {
    pub(crate) fn missing() -> Self {
        Self {
            kept: Kept::Missing,
            fetched_at: None,
            read_at: None,
            wanted_at: None,
        }
    }

    /// Whether the fact's read stamp is a day old or more.
    fn stale(&self, now: i64) -> bool {
        self.read_at.is_some_and(|at| at <= now - DAY_MS)
    }

    /// Whether a want for it would be news: never wanted, or not for a day.
    fn want_is_news(&self, now: i64) -> bool {
        self.wanted_at.is_none_or(|at| at <= now - DAY_MS)
    }
}

/// One kept similar artist.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Neighbour {
    /// Its MusicBrainz id, lowercased.
    pub mbid: String,
    /// Its name, as the provider sent it.
    pub name: Option<String>,
    /// The provider's raw score.
    pub score: f64,
}

/// One artist's kept counts: `None` where ListenBrainz has none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Counts {
    /// Distinct listeners.
    pub listeners: Option<i64>,
    /// Listens.
    pub listens: Option<i64>,
}

/// The reads and wants a reader noted, written in one go.
#[derive(Debug, Default)]
pub(crate) struct Notes {
    read: Vec<(FactKind, FactSource, String)>,
    wanted: Vec<(FactKind, FactSource, WantOrder, String)>,
}

impl Notes {
    /// Note that `looked` (fact `kind` of `artist` from `source`) was read,
    /// wanting it with `order` when it is not kept.
    pub(crate) fn saw<T>(
        &mut self,
        now: i64,
        (kind, source, order): (FactKind, FactSource, WantOrder),
        artist: &str,
        looked: &Looked<T>,
    ) {
        if matches!(looked.kept, Kept::Missing) {
            if looked.want_is_news(now) {
                self.wanted.push((kind, source, order, artist.to_owned()));
            }
        } else if looked.stale(now) {
            self.read.push((kind, source, artist.to_owned()));
        }
    }

    fn is_empty(&self) -> bool {
        self.read.is_empty() && self.wanted.is_empty()
    }
}

/// One fact the refresh job may read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FactRef {
    /// The artist, lowercased.
    pub artist: String,
    /// What is read about it.
    pub kind: FactKind,
    /// Where it is read from.
    pub source: FactSource,
}

/// The kept facts over the database, dated by the kernel clock.
#[derive(Clone)]
pub struct KeptFacts {
    db: Db,
    clock: Arc<dyn Clock>,
}

impl std::fmt::Debug for KeptFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeptFacts").finish_non_exhaustive()
    }
}

fn json(ids: &[String]) -> String {
    serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_owned())
}

/// The fetch rows of one kind and source for `artists`, by artist.
fn fetch_rows(
    conn: &Connection,
    kind: FactKind,
    source: FactSource,
    artists: &str,
) -> rusqlite::Result<HashMap<String, sql::FactFetchColumns>> {
    conn.prepare_cached(sql::FACT_FETCHES.text())?
        .query_map(params![kind, source, artists], sql::FactFetchColumns::read)?
        .map(|row| row.map(|row| (row.artist_mbid.clone(), row)))
        .collect()
}

/// `rows` as kept for `fetch`: found when there are rows, else nothing
/// there once it answered, else missing.
fn looked<T>(fetch: Option<&sql::FactFetchColumns>, rows: Option<T>) -> Looked<T> {
    let Some(fetch) = fetch else {
        return Looked::missing();
    };
    let kept = match (rows, &fetch.fetched_at) {
        (Some(rows), _) => Kept::Found(rows),
        (None, Some(_)) => Kept::Nothing,
        (None, None) => Kept::Missing,
    };
    Looked {
        kept,
        fetched_at: fetch.fetched_at.map(|at| at.0),
        read_at: Some(fetch.read_at.0),
        wanted_at: fetch.wanted_at.map(|at| at.0),
    }
}

fn answered_tx(
    tx: &rusqlite::Transaction,
    (kind, source): (FactKind, FactSource),
    artist: &str,
    outcome: Outcome,
    (now, age): (i64, i64),
) -> rusqlite::Result<()> {
    tx.prepare_cached(sql::FACT_ANSWERED.text())?
        .execute(params![
            artist,
            kind,
            source,
            outcome,
            Millis(now),
            Millis(now + age)
        ])?;
    Ok(())
}

fn secondary_types(facts: &AlbumFacts) -> String {
    serde_json::to_string(facts.secondary_types.as_deref().unwrap_or_default())
        .unwrap_or_else(|_| "[]".to_owned())
}

fn album_row(artist: &str, row: sql::KeptAlbumColumns) -> RankedAlbum {
    let list = |text: &str| serde_json::from_str::<Vec<String>>(text).ok();
    RankedAlbum {
        listen_count: row.listens.unwrap_or(0),
        album: AlbumRow {
            release_group_mbid: row.release_group_mbid,
            title: row.title,
            artist_name: row.artist_name,
            artist_mbid: Some(artist.to_owned()),
            facts: AlbumFacts {
                primary_type: row.primary_type,
                secondary_types: list(&row.secondary_types),
                first_release_date: row.first_release_date,
                listeners: row.listeners,
                credited_artist_mbids: row.credited_artist_mbids.as_deref().and_then(list),
            },
        },
    }
}

impl KeptFacts {
    /// The kept facts in `db`, dated by `clock`.
    pub fn new(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }

    /// Now, unix ms.
    pub(crate) fn now(&self) -> i64 {
        self.clock.now_ms()
    }

    /// The UTC day of `now_ms`.
    fn day(now_ms: i64) -> i64 {
        now_ms.div_euclid(DAY_MS)
    }

    /// The kept similar-artists lists from labs of `artists` (lowercased),
    /// by artist.
    pub(crate) async fn neighbours(
        &self,
        artists: &[String],
    ) -> Result<HashMap<String, Looked<Vec<Neighbour>>>, DbError> {
        let list = json(artists);
        let asked = artists.to_vec();
        self.db
            .read("discover.facts.neighbours", move |conn| {
                // The fetch rows and the lists are read in one snapshot, so a
                // list replaced between them never reads as missing.
                let fetches = fetch_rows(conn, FactKind::Neighbours, FactSource::Labs, &list)?;
                let mut lists: HashMap<String, Vec<Neighbour>> = HashMap::new();
                let rows = conn
                    .prepare_cached(sql::KEPT_NEIGHBOURS.text())?
                    .query_map(
                        params![ListSource::Labs, list],
                        sql::KeptNeighbourColumns::read,
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for row in rows {
                    lists.entry(row.artist_mbid).or_default().push(Neighbour {
                        mbid: row.neighbour_mbid,
                        name: row.neighbour_name,
                        score: row.score,
                    });
                }
                Ok(asked
                    .into_iter()
                    .map(|artist| {
                        let rows = lists.remove(&artist);
                        let found = looked(fetches.get(&artist), rows);
                        (artist, found)
                    })
                    .collect())
            })
            .await
    }

    /// The kept counts of `artists` (lowercased), by artist.
    pub(crate) async fn listeners(
        &self,
        artists: &[String],
    ) -> Result<HashMap<String, Looked<Counts>>, DbError> {
        let list = json(artists);
        let asked = artists.to_vec();
        self.db
            .read("discover.facts.listeners", move |conn| {
                // One snapshot, as for the lists.
                let fetches =
                    fetch_rows(conn, FactKind::Listeners, FactSource::ListenBrainz, &list)?;
                let mut counts: HashMap<String, Counts> = conn
                    .prepare_cached(sql::KEPT_LISTENERS.text())?
                    .query_map([&list], sql::KeptListenerColumns::read)?
                    .map(|row| {
                        row.map(|row| {
                            let counts = Counts {
                                listeners: row.listeners,
                                listens: row.listens,
                            };
                            (row.artist_mbid, counts)
                        })
                    })
                    .collect::<rusqlite::Result<_>>()?;
                Ok(asked
                    .into_iter()
                    .map(|artist| {
                        let rows = counts.remove(&artist);
                        let found = looked(fetches.get(&artist), rows);
                        (artist, found)
                    })
                    .collect())
            })
            .await
    }

    /// `artist`'s kept album list, and whether its groups' counts were
    /// read: found when they were, else missing.
    pub(crate) async fn albums(
        &self,
        artist: &str,
    ) -> Result<(Looked<Vec<RankedAlbum>>, Looked<()>), DbError> {
        let artist = artist.to_owned();
        self.db
            .read("discover.facts.albums", move |conn| {
                // One snapshot: the list, its fetch row and its counts' row.
                let list = json(std::slice::from_ref(&artist));
                let fetch = fetch_rows(conn, FactKind::Albums, FactSource::MusicBrainz, &list)?;
                let counted =
                    fetch_rows(conn, FactKind::AlbumCounts, FactSource::ListenBrainz, &list)?;
                let rows: Vec<RankedAlbum> = conn
                    .prepare_cached(sql::KEPT_ALBUMS.text())?
                    .query_map([&artist], sql::KeptAlbumColumns::read)?
                    .map(|row| row.map(|row| album_row(&artist, row)))
                    .collect::<rusqlite::Result<_>>()?;
                let albums = looked(fetch.get(&artist), (!rows.is_empty()).then_some(rows));
                let counts = counted.get(&artist);
                let counted = looked(
                    counts,
                    counts
                        .is_some_and(|row| row.wanted_at.is_none())
                        .then_some(()),
                );
                Ok((albums, counted))
            })
            .await
    }

    /// The kept band links of `artists` (lowercased), by artist.
    pub(crate) async fn links(
        &self,
        artists: &[String],
    ) -> Result<HashMap<String, Looked<Vec<BandLink>>>, DbError> {
        let list = json(artists);
        let asked = artists.to_vec();
        self.db
            .read("discover.facts.band_links", move |conn| {
                // One snapshot, as for the lists.
                let fetches =
                    fetch_rows(conn, FactKind::BandLinks, FactSource::MusicBrainz, &list)?;
                let mut links: HashMap<String, Vec<BandLink>> = HashMap::new();
                let rows = conn
                    .prepare_cached(sql::KEPT_BAND_LINKS.text())?
                    .query_map([&list], sql::KeptBandLinkColumns::read)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                for row in rows {
                    links.entry(row.artist_mbid).or_default().push(BandLink {
                        mbid: row.linked_mbid,
                        name: row.linked_name,
                        kind: row.kind,
                        direction: row.direction,
                        ended: row.ended,
                    });
                }
                Ok(asked
                    .into_iter()
                    .map(|artist| {
                        let rows = links.remove(&artist);
                        let found = looked(fetches.get(&artist), rows);
                        (artist, found)
                    })
                    .collect())
            })
            .await
    }

    /// The groups of `artists`' kept albums.
    pub(crate) async fn album_groups(&self, artists: &[String]) -> Result<Vec<String>, DbError> {
        let list = json(artists);
        self.db
            .read("discover.facts.album_groups", move |conn| {
                conn.prepare_cached(sql::KEPT_ALBUM_GROUPS.text())?
                    .query_map([list], |row| row.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()
                    .map_err(DbError::from)
            })
            .await
    }

    /// Write what a reader noted, in one write; nothing when it noted
    /// nothing new.
    pub(crate) async fn note(&self, notes: Notes) -> Result<(), DbError> {
        if notes.is_empty() {
            return Ok(());
        }
        let now = self.now();
        self.db
            .write(Lane::Background, "discover.facts.noted", move |tx| {
                let mut read: HashMap<(FactKind, FactSource), Vec<String>> = HashMap::new();
                for (kind, source, artist) in notes.read {
                    read.entry((kind, source)).or_default().push(artist);
                }
                for ((kind, source), artists) in read {
                    tx.prepare_cached(sql::FACTS_READ.text())?.execute(params![
                        kind,
                        source,
                        json(&artists),
                        Millis(now)
                    ])?;
                }
                let mut wanted: HashMap<(FactKind, FactSource, WantOrder), Vec<String>> =
                    HashMap::new();
                for (kind, source, order, artist) in notes.wanted {
                    wanted
                        .entry((kind, source, order))
                        .or_default()
                        .push(artist);
                }
                for ((kind, source, order), artists) in wanted {
                    tx.prepare_cached(sql::FACTS_WANTED.text())?
                        .execute(params![
                            kind,
                            source,
                            json(&artists),
                            Millis(now),
                            order as i64
                        ])?;
                }
                Ok::<_, DbError>(())
            })
            .await
    }

    /// Keep `artist`'s similar artists from labs, best first: they replace
    /// the list kept, in one write that also wants the counts of the
    /// neighbours not counted yet. An empty answer keeps a list already
    /// kept (a retired dataset may answer a known artist with nothing), and
    /// with none kept is "nothing there". Answers what is now kept.
    pub(crate) async fn keep_neighbours(
        &self,
        artist: &str,
        rows: Vec<Neighbour>,
    ) -> Result<Kept<Vec<Neighbour>>, DbError> {
        let now = self.now();
        let artist = artist.to_owned();
        if rows.is_empty() {
            let looked = self.neighbours(std::slice::from_ref(&artist)).await?;
            let kept = looked.get(&artist).map(|looked| looked.kept.clone());
            if let Some(Kept::Found(rows)) = kept {
                self.db
                    .write(
                        Lane::Background,
                        "discover.facts.neighbours_kept",
                        move |tx| {
                            let what = (FactKind::Neighbours, FactSource::Labs);
                            answered_tx(tx, what, &artist, Outcome::Found, (now, LIST_AGE_MS))?;
                            Ok::<_, DbError>(())
                        },
                    )
                    .await?;
                return Ok(Kept::Found(rows));
            }
            self.keep_none(FactKind::Neighbours, FactSource::Labs, &artist)
                .await?;
            return Ok(Kept::Nothing);
        }
        let kept = rows.clone();
        self.db
            .write(Lane::Background, "discover.facts.neighbours", move |tx| {
                tx.prepare_cached(sql::NEIGHBOURS_DROPPED.text())?
                    .execute(params![artist, ListSource::Labs])?;
                let mut add = tx.prepare_cached(sql::NEIGHBOUR_KEPT.text())?;
                for (rank, row) in rows.iter().enumerate() {
                    add.execute(params![
                        artist,
                        ListSource::Labs,
                        rank as i64,
                        row.mbid,
                        row.name,
                        row.score
                    ])?;
                }
                let what = (FactKind::Neighbours, FactSource::Labs);
                answered_tx(tx, what, &artist, Outcome::Found, (now, LIST_AGE_MS))?;
                let neighbours: Vec<String> = rows.iter().map(|row| row.mbid.clone()).collect();
                tx.prepare_cached(sql::FACTS_WANTED.text())?
                    .execute(params![
                        FactKind::Listeners,
                        FactSource::ListenBrainz,
                        json(&neighbours),
                        Millis(now),
                        WantOrder::Seed as i64
                    ])?;
                Ok::<_, DbError>(())
            })
            .await?;
        Ok(Kept::Found(kept))
    }

    /// Keep that `artist` has nothing of `kind` from `source`.
    pub(crate) async fn keep_none(
        &self,
        kind: FactKind,
        source: FactSource,
        artist: &str,
    ) -> Result<(), DbError> {
        let now = self.now();
        let artist = artist.to_owned();
        let age = match kind {
            FactKind::Albums => NO_ALBUMS_AGE_MS,
            FactKind::BandLinks => NO_LINKS_AGE_MS,
            _ => NONE_AGE_MS,
        };
        self.db
            .write(Lane::Background, "discover.facts.none", move |tx| {
                match kind {
                    FactKind::Albums => {
                        tx.prepare_cached(sql::ALBUMS_DROPPED.text())?
                            .execute(params![artist, "[]"])?;
                    }
                    FactKind::BandLinks => {
                        tx.prepare_cached(sql::BAND_LINKS_DROPPED.text())?
                            .execute([&artist])?;
                    }
                    _ => {}
                }
                answered_tx(tx, (kind, source), &artist, Outcome::None, (now, age))?;
                Ok::<_, DbError>(())
            })
            .await
    }

    /// Record that reading `kind` of `artists` from `source` failed: the
    /// rows they had stay, the next try backs off, and the first failure's
    /// time is kept. A fact with no row yet is wanted with `order`.
    pub(crate) async fn keep_failed(
        &self,
        (kind, source): (FactKind, FactSource),
        artists: &[String],
        order: WantOrder,
    ) -> Result<(), DbError> {
        let now = self.now();
        let artists = artists.to_vec();
        self.db
            .write(Lane::Background, "discover.facts.failed", move |tx| {
                let mut failed = tx.prepare_cached(sql::FACT_FAILED.text())?;
                for artist in &artists {
                    failed.execute(params![
                        artist,
                        kind,
                        source,
                        Millis(now),
                        RETRY_MS,
                        RETRY_CAP_MS,
                        order as i64
                    ])?;
                }
                Ok::<_, DbError>(())
            })
            .await
    }

    /// Record that `artist`'s `kind` could not answer yet: it is asked again
    /// soon, and a fact with no row yet is wanted with `order`.
    pub(crate) async fn keep_not_yet(
        &self,
        (kind, source): (FactKind, FactSource),
        artist: &str,
        order: WantOrder,
    ) -> Result<(), DbError> {
        let now = self.now();
        let artist = artist.to_owned();
        self.db
            .write(Lane::Background, "discover.facts.not_yet", move |tx| {
                tx.prepare_cached(sql::FACT_NOT_YET.text())?
                    .execute(params![
                        artist,
                        kind,
                        source,
                        Millis(now),
                        Millis(now + NOT_YET_MS),
                        order as i64
                    ])?;
                Ok::<_, DbError>(())
            })
            .await
    }

    /// Keep the counts of `asked` (lowercased) as read in one answer:
    /// `found` holds those ListenBrainz counted, and the rest have none.
    pub(crate) async fn keep_listeners(
        &self,
        asked: &[String],
        found: HashMap<String, Counts>,
    ) -> Result<(), DbError> {
        let now = self.now();
        let asked = asked.to_vec();
        self.db
            .write(Lane::Background, "discover.facts.listeners", move |tx| {
                let mut keep = tx.prepare_cached(sql::LISTENERS_KEPT.text())?;
                let what = (FactKind::Listeners, FactSource::ListenBrainz);
                for artist in &asked {
                    match found.get(artist) {
                        Some(counts) => {
                            keep.execute(params![artist, counts.listeners, counts.listens])?;
                            answered_tx(tx, what, artist, Outcome::Found, (now, COUNT_AGE_MS))?;
                        }
                        None => answered_tx(tx, what, artist, Outcome::None, (now, NONE_AGE_MS))?,
                    }
                }
                Ok::<_, DbError>(())
            })
            .await
    }

    /// Keep `artist`'s albums that may be offered, in discography order:
    /// they replace the list kept, in one write, a group kept before
    /// keeping its counts, and their counts are wanted again. None is
    /// "nothing to offer".
    pub(crate) async fn keep_albums(
        &self,
        artist: &str,
        rows: Vec<AlbumRow>,
    ) -> Result<(), DbError> {
        if rows.is_empty() {
            return self
                .keep_none(FactKind::Albums, FactSource::MusicBrainz, artist)
                .await;
        }
        let now = self.now();
        let artist = artist.to_owned();
        self.db
            .write(Lane::Background, "discover.facts.albums", move |tx| {
                let groups: Vec<String> = rows
                    .iter()
                    .map(|row| row.release_group_mbid.clone())
                    .collect();
                tx.prepare_cached(sql::ALBUMS_DROPPED.text())?
                    .execute(params![artist, json(&groups)])?;
                let mut keep = tx.prepare_cached(sql::ALBUM_KEPT.text())?;
                for (position, row) in rows.iter().enumerate() {
                    let credited = row
                        .facts
                        .credited_artist_mbids
                        .as_ref()
                        .and_then(|ids| serde_json::to_string(ids).ok());
                    keep.execute(params![
                        artist,
                        row.release_group_mbid,
                        position as i64,
                        row.title,
                        row.artist_name,
                        row.facts.primary_type,
                        secondary_types(&row.facts),
                        row.facts.first_release_date,
                        credited
                    ])?;
                }
                let what = (FactKind::Albums, FactSource::MusicBrainz);
                answered_tx(tx, what, &artist, Outcome::Found, (now, LIST_AGE_MS))?;
                tx.prepare_cached(sql::FACTS_WANTED_AGAIN.text())?
                    .execute(params![
                        FactKind::AlbumCounts,
                        FactSource::ListenBrainz,
                        json(std::slice::from_ref(&artist)),
                        Millis(now),
                        WantOrder::Pool as i64
                    ])?;
                Ok::<_, DbError>(())
            })
            .await
    }

    /// Keep `artist`'s band links, in answer order: they replace the links
    /// kept, in one write. None is "nothing there".
    pub(crate) async fn keep_band_links(
        &self,
        artist: &str,
        links: Vec<BandLink>,
    ) -> Result<(), DbError> {
        if links.is_empty() {
            return self
                .keep_none(FactKind::BandLinks, FactSource::MusicBrainz, artist)
                .await;
        }
        let now = self.now();
        let artist = artist.to_owned();
        self.db
            .write(Lane::Background, "discover.facts.band_links", move |tx| {
                tx.prepare_cached(sql::BAND_LINKS_DROPPED.text())?
                    .execute([&artist])?;
                let mut keep = tx.prepare_cached(sql::BAND_LINK_KEPT.text())?;
                for (position, link) in links.iter().enumerate() {
                    keep.execute(params![
                        artist,
                        position as i64,
                        link.kind,
                        link.direction,
                        link.mbid,
                        link.name,
                        link.ended
                    ])?;
                }
                let what = (FactKind::BandLinks, FactSource::MusicBrainz);
                answered_tx(tx, what, &artist, Outcome::Found, (now, LINKS_AGE_MS))?;
                Ok::<_, DbError>(())
            })
            .await
    }

    /// Keep the counts of `artists`' albums as read in one answer: `found`
    /// holds each counted group's listeners and listens.
    pub(crate) async fn keep_album_counts(
        &self,
        artists: &[String],
        found: HashMap<String, Counts>,
    ) -> Result<(), DbError> {
        let now = self.now();
        let artists = artists.to_vec();
        self.db
            .write(Lane::Background, "discover.facts.album_counts", move |tx| {
                let mut keep = tx.prepare_cached(sql::ALBUM_COUNTS_KEPT.text())?;
                for (group, counts) in &found {
                    keep.execute(params![group, counts.listeners, counts.listens])?;
                }
                let what = (FactKind::AlbumCounts, FactSource::ListenBrainz);
                for artist in &artists {
                    answered_tx(tx, what, artist, Outcome::Found, (now, LIST_AGE_MS))?;
                }
                Ok::<_, DbError>(())
            })
            .await
    }

    /// The facts of `kinds` the refresh job may read now, `limit` at most,
    /// in the order it reads them: wanted in the last seven days first,
    /// then the oldest due that someone read in the last seven days.
    pub(crate) async fn to_read(
        &self,
        kinds: &[FactKind],
        limit: usize,
    ) -> Result<Vec<FactRef>, DbError> {
        let now = self.now();
        let kinds = kinds_json(kinds);
        self.db
            .read("discover.facts.to_read", move |conn| {
                conn.prepare_cached(sql::FACTS_TO_READ.text())?
                    .query_map(
                        params![
                            Millis(now),
                            kinds,
                            Millis(now - super::plan::SEEN_WINDOW_MS),
                            limit as i64
                        ],
                        sql::FactToReadColumns::read,
                    )?
                    .map(|row| {
                        row.map(|row| FactRef {
                            artist: row.artist_mbid,
                            kind: row.kind,
                            source: row.source,
                        })
                    })
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(DbError::from)
            })
            .await
    }

    /// Whether the refresh job has a fact of `kinds` to read now.
    pub(crate) async fn waiting(&self, kinds: &[FactKind]) -> Result<bool, DbError> {
        let now = self.now();
        let kinds = kinds_json(kinds);
        self.db
            .read("discover.facts.waiting", move |conn| {
                conn.query_row(
                    sql::FACTS_WAITING.text(),
                    params![
                        Millis(now),
                        kinds,
                        Millis(now - super::plan::SEEN_WINDOW_MS)
                    ],
                    |row| row.get(0),
                )
                .map_err(DbError::from)
            })
            .await
    }

    /// Drop the facts nothing has read for 180 days and the read counts
    /// older than 90 days.
    pub(crate) async fn drop_unread(&self) -> Result<(), DbError> {
        let now = self.now();
        self.db
            .write(
                Lane::Background,
                "discover.facts.unread_dropped",
                move |tx| {
                    let before = Millis(now - UNREAD_DROP_MS);
                    for statement in [
                        sql::UNREAD_NEIGHBOURS_DROPPED,
                        sql::UNREAD_LISTENERS_DROPPED,
                        sql::UNREAD_ALBUMS_DROPPED,
                        sql::UNREAD_BAND_LINKS_DROPPED,
                        sql::UNREAD_FETCHES_DROPPED,
                    ] {
                        tx.prepare_cached(statement.text())?.execute([&before])?;
                    }
                    tx.prepare_cached(sql::READ_DAYS_DROPPED.text())?
                        .execute([Self::day(now) - METER_DAYS])?;
                    Ok::<_, DbError>(())
                },
            )
            .await
    }

    /// Count `reads` background reads to `source` today. A failed write is
    /// logged: the count is a meter, and the read it counts was made.
    pub(crate) async fn count_read(&self, source: MeteredSource, reads: u64) {
        if reads == 0 {
            return;
        }
        let day = Self::day(self.now());
        let counted = self
            .db
            .write(Lane::Background, "discover.facts.read_counted", move |tx| {
                tx.prepare_cached(sql::READS_COUNTED.text())?
                    .execute(params![day, source, reads as i64])?;
                Ok::<_, DbError>(())
            })
            .await;
        if let Err(error) = counted {
            tracing::warn!(%error, source = source.as_str(), "a background read went uncounted");
        }
    }

    /// The background reads counted today, every source together.
    pub(crate) async fn reads_today(&self) -> Result<u64, DbError> {
        let day = Self::day(self.now());
        self.db
            .read("discover.facts.reads_today", move |conn| {
                conn.query_row(sql::READS_ON_DAY.text(), [day], |row| row.get::<_, i64>(0))
                    .map_err(DbError::from)
            })
            .await
            .map(|reads| u64::try_from(reads).unwrap_or(0))
    }

    /// Finish a refresh job inside its own write: `done` completes it.
    pub(crate) async fn finish(
        &self,
        done: impl FnOnce(&rusqlite::Transaction) -> Result<(), JobError> + Send + 'static,
    ) -> Result<(), JobError> {
        self.db
            .write(Lane::Background, "discover.facts.done", done)
            .await
    }

    /// Who was seen in the last seven days.
    pub(crate) async fn seen(&self) -> Result<usize, DbError> {
        let since = self.now() - super::plan::SEEN_WINDOW_MS;
        Ok(droppedneedle_auth::sqlite::seen_since(&self.db, since)
            .await?
            .len())
    }
}

fn kinds_json(kinds: &[FactKind]) -> String {
    let names: Vec<String> = kinds.iter().map(|kind| kind.as_str().to_owned()).collect();
    json(&names)
}

/// Empty the fact tables and their fetch rows, in one write.
pub(super) async fn clear(db: &Db) -> Result<u64, DbError> {
    db.write(Lane::Foreground, "discover.facts.cleared", |tx| {
        let mut cleared = 0;
        for statement in [
            sql::ALL_NEIGHBOURS_DROPPED,
            sql::ALL_LISTENERS_DROPPED,
            sql::ALL_ALBUMS_DROPPED,
            sql::ALL_BAND_LINKS_DROPPED,
        ] {
            cleared += tx.execute(statement.text(), [])? as u64;
        }
        tx.execute(sql::ALL_FETCHES_DROPPED.text(), [])?;
        Ok::<_, DbError>(cleared)
    })
    .await
}

/// Every kept fact row.
pub(super) async fn kept_rows(db: &Db) -> Result<u64, DbError> {
    db.read("discover.facts.kept_rows", |conn| {
        conn.query_row(sql::KEPT_FACT_ROWS.text(), [], |row| row.get::<_, i64>(0))
            .map_err(DbError::from)
    })
    .await
    .map(|rows| u64::try_from(rows).unwrap_or(0))
}
