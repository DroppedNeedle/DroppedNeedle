//! Which artist row a file's credits land on.
//!
//! An artist row is keyed by the tag's name and sort name, as in v2. A
//! credit whose file lists its artists with their MusicBrainz ids one for
//! one (`ARTISTS` with `MUSICBRAINZ_ARTISTID`, `ALBUMARTISTS` with
//! `MUSICBRAINZ_ALBUMARTISTID`) carries its id, and that id picks the row
//! already proven to be the artist, whatever name the tag now gives it.
//! Names never prove anything: a credit without an id lands where its name
//! keys, and an id never joins a row whose links or files say it is
//! another artist.
//!
//! A row is proven to be artist `x` when a person or a v2 import linked it
//! to `x`, or when it has no link and its credits hold `x` and no other id
//! ([`proven_rows`]). Two rows proven to be one artist are merged at the
//! end of the scan run (`corrections::artist_convergence`).

use std::collections::{BTreeSet, HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension as _, params};

use super::commit::{TRACK_GONE, UNKNOWN_ARTIST, UNKNOWN_ARTIST_ID, stable_id};
use crate::matching::model::VARIOUS_ARTISTS_MBID;
use crate::tags::AudioArtistCredit;
use droppedneedle_kernel::db::fold_text;

/// The catalog's "Various Artists" sentinel (seeded by the baseline).
const VARIOUS_ARTISTS_ID: &str = "00000000-0000-4000-8000-000000000001";

/// One artist credit as the catalog stores it.
pub(super) struct Credit {
    pub name: String,
    pub sort_name: Option<String>,
    pub credited_name: String,
    pub join_phrase: String,
    /// The credit's MusicBrainz artist id, lowercased, when its side of
    /// the file is aligned ([`credits`]).
    pub mbid: Option<String>,
}

/// The row a credit's name and sort name key to.
fn artist_id(credit: &Credit) -> String {
    if credit.name == UNKNOWN_ARTIST {
        return UNKNOWN_ARTIST_ID.to_owned();
    }
    stable_id(&format!(
        "artist:{}:{}",
        credit.name,
        credit.sort_name.as_deref().unwrap_or("")
    ))
}

/// The ids of one side of a file, lowercased, when they line up with its
/// names: the names came from the list key, there are as many of each,
/// and every id is a distinct hyphenated UUID.
fn aligned_ids(names: &[AudioArtistCredit], ids: &[String], listed: bool) -> Option<Vec<String>> {
    if !listed
        || names.is_empty()
        || names.len() != ids.len()
        || names.iter().any(|credit| credit.name.trim().is_empty())
    {
        return None;
    }
    let lowered: Vec<String> = ids.iter().map(|id| id.to_ascii_lowercase()).collect();
    let distinct: HashSet<&String> = lowered.iter().collect();
    let valid = lowered
        .iter()
        .all(|id| id.len() == 36 && uuid::Uuid::parse_str(id).is_ok());
    (valid && distinct.len() == lowered.len()).then_some(lowered)
}

/// One side's credits: from the tags, else one credit from the fallback
/// name. An aligned side gives each credit its id; Various Artists and
/// the "Unknown Artist" sentinel carry none.
pub(super) fn credits(
    from_tags: &[AudioArtistCredit],
    ids: &[String],
    listed: bool,
    fallback: &str,
    fallback_sort: Option<&str>,
) -> Vec<Credit> {
    let aligned = aligned_ids(from_tags, ids, listed);
    let mut out: Vec<Credit> = from_tags
        .iter()
        .enumerate()
        .filter(|(_, credit)| !credit.name.trim().is_empty())
        .map(|(position, credit)| {
            let name = credit.name.trim().to_owned();
            let mbid = aligned
                .as_ref()
                .and_then(|ids| ids.get(position))
                .filter(|id| id.as_str() != VARIOUS_ARTISTS_MBID && name != UNKNOWN_ARTIST)
                .cloned();
            Credit {
                sort_name: credit.sort_name.clone(),
                credited_name: credit.credited_name.clone().unwrap_or_else(|| name.clone()),
                join_phrase: credit.join_phrase.clone(),
                name,
                mbid,
            }
        })
        .collect();
    if out.is_empty() {
        out.push(Credit {
            name: fallback.to_owned(),
            sort_name: fallback_sort.map(str::to_owned),
            credited_name: fallback.to_owned(),
            join_phrase: String::new(),
            mbid: None,
        });
    }
    out
}

/// Insert the row `id` with the credit's names unless it exists.
fn insert_artist(tx: &Connection, id: &str, credit: &Credit, now: f64) -> rusqlite::Result<()> {
    let folded = fold_text(&credit.name);
    tx.prepare_cached(
        "INSERT INTO local_artists (id, display_name, sort_name, folded_name, normalized_name, \
         kind, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4, 'group', ?5, ?5) \
         ON CONFLICT (id) DO NOTHING",
    )?
    .execute(params![id, credit.name, credit.sort_name, folded, now])?;
    Ok(())
}

/// The live row `id` names: itself, else the row its alias names, else
/// the row it was retired into (the order artist reads use).
fn live_artist(tx: &Connection, id: &str) -> rusqlite::Result<Option<String>> {
    tx.prepare_cached(
        "SELECT id FROM ( \
         SELECT id, 0 AS rank FROM local_artists WHERE id = ?1 AND retired_into_artist_id IS NULL \
         UNION ALL SELECT r.id, 1 FROM local_artist_aliases x \
          JOIN local_artists r ON r.id = x.local_artist_id \
          WHERE x.alias = ?1 AND r.retired_into_artist_id IS NULL \
         UNION ALL SELECT r.id, 2 FROM local_artists a \
          JOIN local_artists r ON r.id = a.retired_into_artist_id \
          WHERE a.id = ?1 AND r.retired_into_artist_id IS NULL) \
         ORDER BY rank LIMIT 1",
    )?
    .query_row(params![id], |row| row.get(0))
    .optional()
}

/// The row a credit's name keys to, and the live row that is now: the
/// row itself, or the row a merge retired it into.
struct NameRow {
    keyed: String,
    live: String,
}

fn name_row(tx: &Connection, credit: &Credit, now: f64) -> rusqlite::Result<NameRow> {
    let keyed = artist_id(credit);
    insert_artist(tx, &keyed, credit, now)?;
    let live = live_artist(tx, &keyed)?.unwrap_or_else(|| keyed.clone());
    Ok(NameRow { keyed, live })
}

/// The name row for a credit, as the album rebuild path takes it.
pub(super) fn upsert_artist(
    tx: &Connection,
    credit: &Credit,
    now: f64,
) -> rusqlite::Result<String> {
    Ok(name_row(tx, credit, now)?.live)
}

/// One live row proven to be an artist.
pub(crate) struct Proven {
    pub id: String,
    /// Proven by a person's or a v2 import's link, not by its files.
    pub linked: bool,
}

/// The live rows proven to be the MusicBrainz artist `mbid`, the survivor
/// first: linked rows (oldest first), then rows proven by their files
/// (oldest first). A link counts when a person or a v2 import made it; a
/// row with any other link is proven by neither. Sentinels and Various
/// Artists rows are never proven. Every test is a bounded index probe.
pub(crate) fn proven_rows(tx: &Connection, mbid: &str) -> rusqlite::Result<Vec<Proven>> {
    let mut out: Vec<Proven> = tx
        .prepare_cached(
            "SELECT a.id FROM local_artist_external_identities i \
             JOIN local_artists a ON a.id = i.local_artist_id \
             WHERE lower(i.provider_artist_id) = ?1 AND i.provider = 'musicbrainz' \
             AND i.decision_source IN ('manual', 'legacy_import') \
             AND a.retired_into_artist_id IS NULL AND a.kind <> 'various_artists' \
             AND a.id NOT IN (?2, ?3) ORDER BY a.created_at, a.id",
        )?
        .query_map(
            params![mbid, VARIOUS_ARTISTS_ID, UNKNOWN_ARTIST_ID],
            |row| {
                Ok(Proven {
                    id: row.get(0)?,
                    linked: true,
                })
            },
        )?
        .collect::<rusqlite::Result<_>>()?;
    let mut by_files: Vec<(f64, String)> = Vec::new();
    for holder in holders(tx, mbid)? {
        if out.iter().any(|row| row.id == holder) {
            continue;
        }
        let created: Option<f64> = tx
            .prepare_cached(
                "SELECT created_at FROM local_artists WHERE id = ?1 \
                 AND retired_into_artist_id IS NULL AND kind <> 'various_artists' \
                 AND id NOT IN (?2, ?3) AND NOT EXISTS (SELECT 1 FROM \
                 local_artist_external_identities WHERE local_artist_id = ?1)",
            )?
            .query_row(
                params![holder, VARIOUS_ARTISTS_ID, UNKNOWN_ARTIST_ID],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(created) = created
            && !holds_other_id(tx, &holder, mbid, None)?
        {
            by_files.push((created, holder));
        }
    }
    by_files.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    out.extend(
        by_files
            .into_iter()
            .map(|(_, id)| Proven { id, linked: false }),
    );
    Ok(out)
}

/// The rows any credit holding `mbid` points at, one index step each.
fn holders(tx: &Connection, mbid: &str) -> rusqlite::Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for table in ["local_track_artists", "local_album_artists"] {
        let mut after = String::new();
        loop {
            let next: Option<String> = tx
                .prepare_cached(&format!(
                    "SELECT local_artist_id FROM {table} WHERE artist_mbid = ?1 \
                     AND local_artist_id > ?2 ORDER BY local_artist_id LIMIT 1"
                ))?
                .query_row(params![mbid, after], |row| row.get(0))
                .optional()?;
            let Some(next) = next else { break };
            out.insert(next.clone());
            after = next;
        }
    }
    Ok(out)
}

/// The track and album whose own credits a test leaves out: the file
/// being read rewrites them.
#[derive(Clone, Copy)]
pub(super) struct Own<'a> {
    /// The scan run reading the file, if any: tracks gone in it no
    /// longer count.
    pub run_id: Option<&'a str>,
    pub track_id: Option<&'a str>,
    pub album_id: &'a str,
}

/// Whether a credit of `artist` holds an id other than `mbid`, leaving
/// out `own`'s credits. Two probes per table, each stopping at the first
/// row found.
fn holds_other_id(
    tx: &Connection,
    artist: &str,
    mbid: &str,
    own: Option<Own<'_>>,
) -> rusqlite::Result<bool> {
    let track = own.and_then(|own| own.track_id);
    let album = own.map(|own| own.album_id);
    for (sql, skip) in [
        (
            "SELECT EXISTS(SELECT 1 FROM local_track_artists WHERE local_artist_id = ?1 \
             AND artist_mbid < ?2 AND local_track_id IS NOT ?3)",
            track,
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM local_track_artists WHERE local_artist_id = ?1 \
             AND artist_mbid > ?2 AND local_track_id IS NOT ?3)",
            track,
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM local_album_artists WHERE local_artist_id = ?1 \
             AND artist_mbid < ?2 AND local_album_id IS NOT ?3)",
            album,
        ),
        (
            "SELECT EXISTS(SELECT 1 FROM local_album_artists WHERE local_artist_id = ?1 \
             AND artist_mbid > ?2 AND local_album_id IS NOT ?3)",
            album,
        ),
    ] {
        let found: bool = tx
            .prepare_cached(sql)?
            .query_row(params![artist, mbid, skip], |row| row.get(0))?;
        if found {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Picks the rows for one file's credits.
struct Resolver<'a> {
    tx: &'a Connection,
    own: Own<'a>,
    now: f64,
    /// MusicBrainz id to the row it took in this file, and whether that
    /// row was proven to be the artist.
    by_id: HashMap<String, (String, bool)>,
    /// The ids this file's credits put on each row so far.
    placed: HashMap<String, Vec<String>>,
}

impl Resolver<'_> {
    /// Rule for a credit with id `mbid`: the row proven to be the artist,
    /// else the name row (or the shared album credit's name row) unless
    /// that row says it is another artist, else the row keyed by the id.
    fn with_id(
        &mut self,
        credit: &Credit,
        mbid: &str,
        shared: Option<&Credit>,
    ) -> rusqlite::Result<String> {
        let row = if let Some((row, proven)) = self.by_id.get(mbid).cloned() {
            if proven {
                self.follow_names(&row, credit)?;
            }
            row
        } else if let Some(survivor) = proven_rows(self.tx, mbid)?.into_iter().next() {
            self.follow_names(&survivor.id, credit)?;
            self.by_id
                .insert(mbid.to_owned(), (survivor.id.clone(), true));
            survivor.id
        } else {
            let base = name_row(self.tx, shared.unwrap_or(credit), self.now)?;
            let fits = if base.keyed == base.live {
                !self.says_other(&base.live, mbid)?
            } else {
                // Reached through a merge: a person's merge stands; the
                // app's own merge holds only for the id that proved it.
                let converged: Option<String> = self
                    .tx
                    .prepare_cached("SELECT converged_mbid FROM local_artists WHERE id = ?1")?
                    .query_row(params![base.keyed], |row| row.get(0))
                    .optional()?
                    .flatten();
                converged.is_none_or(|converged| converged == mbid)
            };
            let row = if fits {
                base.live
            } else {
                let keyed = stable_id(&format!("artist-mbid:{mbid}"));
                insert_artist(self.tx, &keyed, credit, self.now)?;
                live_artist(self.tx, &keyed)?.unwrap_or(keyed)
            };
            self.by_id.insert(mbid.to_owned(), (row.clone(), false));
            row
        };
        self.placed
            .entry(row.clone())
            .or_default()
            .push(mbid.to_owned());
        Ok(row)
    }

    /// Whether `artist` is another artist than `mbid` by what is known:
    /// a sentinel or Various Artists row, a person's or v2's link to
    /// another id, another id this file already put on it, or a credit
    /// of another file holding another id.
    fn says_other(&self, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
        if self
            .placed
            .get(artist)
            .is_some_and(|ids| ids.iter().any(|id| id != mbid))
        {
            return Ok(true);
        }
        let marked: bool = self
            .tx
            .prepare_cached(
                "SELECT EXISTS(SELECT 1 FROM local_artists WHERE id = ?1 \
                 AND (kind = 'various_artists' OR id IN (?3, ?4))) \
                 OR EXISTS(SELECT 1 FROM local_artist_external_identities \
                 WHERE local_artist_id = ?1 AND provider = 'musicbrainz' \
                 AND decision_source IN ('manual', 'legacy_import') \
                 AND lower(provider_artist_id) <> ?2)",
            )?
            .query_row(
                params![artist, mbid, VARIOUS_ARTISTS_ID, UNKNOWN_ARTIST_ID],
                |row| row.get(0),
            )?;
        Ok(marked || holds_other_id(self.tx, artist, mbid, Some(self.own))?)
    }

    /// The proven row takes the credit's name once no other file still
    /// credits it under its current name (only indexed tracks, and live
    /// albums with an indexed track, count), and the credit's sort name
    /// when the row is then called by the credit's name.
    fn follow_names(&self, artist: &str, credit: &Credit) -> rusqlite::Result<()> {
        let (display, sort): (String, Option<String>) = self
            .tx
            .prepare_cached("SELECT display_name, sort_name FROM local_artists WHERE id = ?1")?
            .query_row(params![artist], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let renamed = display != credit.name && !self.still_named(artist, &display)?;
        let name = if renamed { &credit.name } else { &display };
        let new_sort = match &credit.sort_name {
            Some(wanted) if *name == credit.name && sort.as_ref() != Some(wanted) => Some(wanted),
            _ => None,
        };
        if !renamed && new_sort.is_none() {
            return Ok(());
        }
        let folded = fold_text(name);
        self.tx
            .prepare_cached(
                "UPDATE local_artists SET display_name = ?2, folded_name = ?3, \
                 normalized_name = ?3, sort_name = COALESCE(?4, sort_name), updated_at = ?5, \
                 row_revision = row_revision + 1 WHERE id = ?1",
            )?
            .execute(params![artist, name, folded, new_sort, self.now])?;
        Ok(())
    }

    /// Whether a credit of another file still names `artist` as `display`.
    /// Only indexed tracks count, less those gone in this run (a tagger
    /// that renames also moves files, and the old paths are marked missing
    /// only once the run ends). An album's credit counts while a file
    /// other than this one is indexed on it.
    fn still_named(&self, artist: &str, display: &str) -> rusqlite::Result<bool> {
        self.tx
            .prepare_cached(&format!(
                "SELECT EXISTS(SELECT 1 FROM local_track_artists c \
                 JOIN local_tracks t ON t.id = c.local_track_id \
                 WHERE c.local_artist_id = ?2 AND c.credited_name = ?3 \
                 AND c.local_track_id IS NOT ?4 AND t.availability = 'indexed' \
                 AND NOT {TRACK_GONE}) \
                 OR EXISTS(SELECT 1 FROM local_album_artists c \
                 JOIN local_albums a ON a.id = c.local_album_id \
                 WHERE c.local_artist_id = ?2 AND c.credited_name = ?3 \
                 AND c.local_album_id <> ?5 AND a.retired_into_album_id IS NULL \
                 AND EXISTS(SELECT 1 FROM local_tracks t WHERE t.local_album_id = a.id \
                 AND t.availability = 'indexed' AND t.id IS NOT ?4 AND NOT {TRACK_GONE}))"
            ))?
            .query_row(
                params![
                    self.own.run_id,
                    artist,
                    display,
                    self.own.track_id,
                    self.own.album_id
                ],
                |row| row.get(0),
            )
    }
}

/// The rows for one file's album and track credits, in order.
///
/// First every credit with an id, album side then track side. A track
/// credit with no proven row shares the row of an album credit with the
/// same folded name that has no id or the same id, as a credit without an
/// id always has. Then the credits without an id: an album credit takes
/// the row of a credit with an id and the same folded name, a track
/// credit that of an album credit, then of a track credit with an id,
/// each by folded name; else the name row.
pub(super) fn resolve(
    tx: &Connection,
    album: &[Credit],
    track: &[Credit],
    own: Own<'_>,
    now: f64,
) -> rusqlite::Result<(Vec<String>, Vec<String>)> {
    let mut resolver = Resolver {
        tx,
        own,
        now,
        by_id: HashMap::new(),
        placed: HashMap::new(),
    };
    let mut album_rows: Vec<Option<String>> = vec![None; album.len()];
    let mut track_rows: Vec<Option<String>> = vec![None; track.len()];
    for (index, credit) in album.iter().enumerate() {
        if let Some(mbid) = &credit.mbid {
            album_rows[index] = Some(resolver.with_id(credit, mbid, None)?);
        }
    }
    for (index, credit) in track.iter().enumerate() {
        if let Some(mbid) = &credit.mbid {
            let folded = fold_text(&credit.name);
            let shared = album.iter().find(|other| {
                fold_text(&other.name) == folded && other.mbid.as_ref().is_none_or(|id| id == mbid)
            });
            track_rows[index] = Some(resolver.with_id(credit, mbid, shared)?);
        }
    }
    let with_id: Vec<(String, String)> = album
        .iter()
        .zip(&album_rows)
        .chain(track.iter().zip(&track_rows))
        .filter_map(|(credit, row)| Some((fold_text(&credit.name), row.clone()?)))
        .collect();
    for (index, credit) in album.iter().enumerate() {
        if album_rows[index].is_none() {
            let folded = fold_text(&credit.name);
            let row = match with_id.iter().find(|(name, _)| *name == folded) {
                Some((_, row)) => row.clone(),
                None => name_row(tx, credit, now)?.live,
            };
            album_rows[index] = Some(row);
        }
    }
    let album_rows: Vec<String> = album_rows.into_iter().flatten().collect();
    for (index, credit) in track.iter().enumerate() {
        if track_rows[index].is_none() {
            let folded = fold_text(&credit.name);
            let shared = album
                .iter()
                .zip(&album_rows)
                .find(|(other, _)| fold_text(&other.name) == folded)
                .map(|(_, row)| row.clone())
                .or_else(|| {
                    track
                        .iter()
                        .zip(&track_rows)
                        .find(|(other, row)| {
                            other.mbid.is_some()
                                && row.is_some()
                                && fold_text(&other.name) == folded
                        })
                        .and_then(|(_, row)| row.clone())
                });
            let row = match shared {
                Some(row) => row,
                None => name_row(tx, credit, now)?.live,
            };
            track_rows[index] = Some(row);
        }
    }
    Ok((album_rows, track_rows.into_iter().flatten().collect()))
}
