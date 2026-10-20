//! Artist ids in v3's form for the carried catalog.
//!
//! v2 keyed an artist by its MusicBrainz id when the file had one, else by
//! the name and sort name it read for that position. v3 finds the row a
//! credit lands on by the credit's name and the file's sort name, taken
//! only when its side lists a single artist ([`artist_key`]). A carried row
//! left under v2's id would lose every credit to a new row on the first
//! scan, and its plays, favorites and decisions would stay on a row no list
//! shows.
//!
//! So each carried artist takes the key most of its credits land on
//! (present tracks first; ties go to the smaller key). A track credit lands
//! where its album's credit of the same folded name does, as the scan
//! shares them. The key function is the scan's own; which credit shares
//! with which is modelled here, and a credit the scan routes by its
//! MusicBrainz id lands on the row that id is linked to, whatever its key.
//! The old id stays as a `legacy_artist` alias, so links handed out under
//! it keep opening the artist, and every carried reference is rewritten.
//!
//! Two rows never share an id, and no row takes an id that another carried
//! row keeps (the two sentinels, Various Artists rows, rows kept under
//! their v2 id):
//!
//! - when two rows want one key, the row with more votes takes it; the
//!   other takes the row v3 makes for its MusicBrainz id when it has one,
//!   else keeps its v2 id;
//! - a row whose key is taken by a kept id keeps its v2 id.
//!
//! A row v2 merged into another takes a key its survivor's credits under
//! the merged row's name land on, else the key of its own name and sort
//! name, so v3's scan finds the merge when a file still uses that name.
//! Rows without credits keep their v2 id. Re-keyed rows keep v2's display
//! and sort names, which only label the row. The export notes every row
//! kept apart by name.

use std::collections::{BTreeMap, HashMap, HashSet};

use rusqlite::{Connection, params};

use crate::export::error::ExportError;
use crate::export::sections::collections::FAVORITE_NAMES;
use crate::export::sections::library::{
    ALBUM_ARTISTS, ARTIST_ALIASES, ARTIST_DISMISSALS, ARTIST_IDENTITIES, ARTISTS, MERGE_CANDIDATES,
    TRACK_ARTISTS, TRACKS,
};
use crate::export::sections::{ALL, LinkKind};
use droppedneedle_kernel::db::fold::fold_text;
use droppedneedle_library::scan::naming::{
    UNKNOWN_ARTIST_ID, VARIOUS_ARTISTS_ID, artist_key, artist_mbid_key,
};

/// What re-keying the carried artists did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ArtistRekey {
    /// Rows that took a new id.
    pub rekeyed: usize,
    /// `legacy_artist` aliases added for the old ids.
    pub aliases_added: u64,
    /// Rows kept apart from another row with the same key, by name.
    pub kept_apart: Vec<String>,
    /// Rows kept under their v2 id because another row keeps their key
    /// (a sentinel such as Unknown Artist), by name.
    pub kept_reserved: Vec<String>,
}

impl ArtistRekey {
    /// Lines for the export's notes, one per kind of row kept apart.
    #[must_use]
    pub fn notes(&self) -> Vec<String> {
        let mut notes = Vec::new();
        if !self.kept_apart.is_empty() {
            notes.push(format!(
                "Artists kept apart from another artist with the same name and sort name: {}",
                self.kept_apart.join(", ")
            ));
        }
        if !self.kept_reserved.is_empty() {
            notes.push(format!(
                "Artists kept under their v2 id because v3 keeps their name for its own \
                 artist row: {}",
                self.kept_reserved.join(", ")
            ));
        }
        notes
    }
}

fn bundle_error(error: rusqlite::Error) -> ExportError {
    ExportError::Bundle {
        reason: error.to_string(),
    }
}

/// True when bundle table `table` has `column`.
fn has(conn: &Connection, table: &str, column: &str) -> Result<bool, ExportError> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info(?1, 'main') WHERE name = ?2)",
        params![table, column],
        |row| row.get(0),
    )
    .map_err(bundle_error)
}

/// One carried artist row.
struct Artist {
    id: String,
    name: String,
    sort_name: Option<String>,
    retired_into: Option<String>,
    various: bool,
}

/// One credit's vote: the row it is on, its folded name, the key the scan
/// would land it on, and whether its file is present.
struct Vote {
    artist: String,
    folded: String,
    key: String,
    present: bool,
}

/// One carried credit row.
struct CreditRow {
    owner: String,
    artist: String,
    name: Option<String>,
}

/// Votes per key: present credits first, then all.
#[derive(Default)]
struct Tally {
    present: BTreeMap<String, usize>,
    all: BTreeMap<String, usize>,
}

impl Tally {
    fn add(&mut self, key: &str, present: bool) {
        *self.all.entry(key.to_owned()).or_default() += 1;
        if present {
            *self.present.entry(key.to_owned()).or_default() += 1;
        }
    }

    /// Keys by votes, most first; ties go to the smaller key.
    fn ranked(&self) -> Vec<(String, usize)> {
        let counts = if self.present.is_empty() {
            &self.all
        } else {
            &self.present
        };
        let mut ranked: Vec<(String, usize)> = counts
            .iter()
            .map(|(key, count)| (key.clone(), *count))
            .collect();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        ranked
    }
}

/// The value most of `values` hold, ties to a set value over none, then
/// to the smaller one.
fn majority(values: &[Option<String>]) -> Option<String> {
    let mut counts: BTreeMap<&Option<String>, usize> = BTreeMap::new();
    for value in values {
        *counts.entry(value).or_default() += 1;
    }
    counts
        .into_iter()
        .max_by(|a, b| {
            a.1.cmp(&b.1)
                .then_with(|| a.0.is_some().cmp(&b.0.is_some()))
                .then_with(|| b.0.cmp(a.0))
        })
        .and_then(|(value, _)| value.clone())
}

fn read_artists(conn: &Connection) -> Result<Vec<Artist>, ExportError> {
    let sort = if has(conn, ARTISTS.name, "sort_name")? {
        "sort_name"
    } else {
        "NULL"
    };
    let mut stmt = conn
        .prepare(&format!(
            "SELECT id, display_name, {sort}, retired_into_artist_id, kind \
             FROM main.\"{}\" ORDER BY id",
            ARTISTS.name
        ))
        .map_err(bundle_error)?;
    let rows = stmt
        .query_map([], |row| {
            let kind: Option<String> = row.get(4)?;
            Ok(Artist {
                id: row.get(0)?,
                name: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                sort_name: row.get(2)?,
                retired_into: row.get(3)?,
                various: kind.as_deref() == Some("various_artists"),
            })
        })
        .map_err(bundle_error)?;
    rows.collect::<Result<_, _>>().map_err(bundle_error)
}

fn read_credits(
    conn: &Connection,
    table: &str,
    owner: &str,
) -> Result<BTreeMap<String, Vec<CreditRow>>, ExportError> {
    let mut stmt = conn
        .prepare(&format!(
            "SELECT \"{owner}\", local_artist_id, credited_name FROM main.\"{table}\" \
             ORDER BY \"{owner}\", position"
        ))
        .map_err(bundle_error)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(CreditRow {
                owner: row.get(0)?,
                artist: row.get(1)?,
                name: row.get(2)?,
            })
        })
        .map_err(bundle_error)?;
    let mut by_owner: BTreeMap<String, Vec<CreditRow>> = BTreeMap::new();
    for row in rows {
        let row = row.map_err(bundle_error)?;
        by_owner.entry(row.owner.clone()).or_default().push(row);
    }
    Ok(by_owner)
}

/// One carried track's facts the artist keys read.
struct TrackFacts {
    album: String,
    present: bool,
    artist_sort: Option<String>,
    album_artist_sort: Option<String>,
}

fn read_tracks(conn: &Connection) -> Result<HashMap<String, TrackFacts>, ExportError> {
    let column = |name: &str| -> Result<String, ExportError> {
        Ok(if has(conn, TRACKS.name, name)? {
            format!("\"{name}\"")
        } else {
            "NULL".to_owned()
        })
    };
    let mut stmt = conn
        .prepare(&format!(
            "SELECT id, local_album_id, {availability} = 'indexed', {artist_sort}, \
             {album_artist_sort} FROM main.\"{table}\"",
            availability = column("availability")?,
            artist_sort = column("artist_sort")?,
            album_artist_sort = column("album_artist_sort")?,
            table = TRACKS.name,
        ))
        .map_err(bundle_error)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                TrackFacts {
                    album: row.get(1)?,
                    present: row.get::<_, Option<bool>>(2)?.unwrap_or(false),
                    artist_sort: row.get(3)?,
                    album_artist_sort: row.get(4)?,
                },
            ))
        })
        .map_err(bundle_error)?;
    rows.collect::<Result<_, _>>().map_err(bundle_error)
}

/// The trimmed name a credit carries, as the scan reads it.
fn credit_name(row: &CreditRow) -> Option<String> {
    let name = row.name.as_deref()?.trim();
    (!name.is_empty()).then(|| name.to_owned())
}

/// Every credit's vote. An album credit lands on its name with the
/// album's sort name when it is the album's only credit. A track credit
/// lands where the album credit of the same folded name does, else on its
/// own name with the track's sort name when it is the track's only credit.
fn votes(conn: &Connection) -> Result<Vec<Vote>, ExportError> {
    let tracks = read_tracks(conn)?;
    let album_credits = read_credits(conn, ALBUM_ARTISTS.name, "local_album_id")?;
    let track_credits = read_credits(conn, TRACK_ARTISTS.name, "local_track_id")?;
    let mut by_album: HashMap<&str, Vec<&TrackFacts>> = HashMap::new();
    for track in tracks.values() {
        by_album
            .entry(track.album.as_str())
            .or_default()
            .push(track);
    }
    let mut out = Vec::new();
    // Per album: each credit's folded name and key, in position order.
    let mut album_keys: HashMap<&str, Vec<(String, String)>> = HashMap::new();
    for (album, credits) in &album_credits {
        let album_tracks = by_album.get(album.as_str()).map_or(&[][..], Vec::as_slice);
        let present = album_tracks.iter().any(|track| track.present);
        let sorts: Vec<Option<String>> = album_tracks
            .iter()
            .filter(|track| track.present || !present)
            .map(|track| track.album_artist_sort.clone())
            .collect();
        let sort = majority(&sorts);
        let keys = album_keys.entry(album.as_str()).or_default();
        for credit in credits {
            let Some(name) = credit_name(credit) else {
                continue;
            };
            let only = credits.len() == 1;
            let key = artist_key(&name, sort.as_deref().filter(|_| only));
            let folded = fold_text(&name);
            keys.push((folded.clone(), key.clone()));
            out.push(Vote {
                artist: credit.artist.clone(),
                folded,
                key,
                present,
            });
        }
    }
    for (track_id, credits) in &track_credits {
        let Some(track) = tracks.get(track_id) else {
            continue;
        };
        let shared = album_keys.get(track.album.as_str());
        for credit in credits {
            let Some(name) = credit_name(credit) else {
                continue;
            };
            let folded = fold_text(&name);
            let key = shared
                .and_then(|keys| keys.iter().find(|(other, _)| *other == folded))
                .map(|(_, key)| key.clone())
                .unwrap_or_else(|| {
                    let only = credits.len() == 1;
                    artist_key(&name, track.artist_sort.as_deref().filter(|_| only))
                });
            out.push(Vote {
                artist: credit.artist.clone(),
                folded,
                key,
                present: track.present,
            });
        }
    }
    Ok(out)
}

/// The lowercased MusicBrainz id each carried row is linked to.
fn identities(conn: &Connection) -> Result<HashMap<String, String>, ExportError> {
    let table = ARTIST_IDENTITIES.name;
    for column in ["local_artist_id", "provider", "provider_artist_id"] {
        if !has(conn, table, column)? {
            return Ok(HashMap::new());
        }
    }
    let mut stmt = conn
        .prepare(&format!(
            "SELECT local_artist_id, lower(trim(provider_artist_id)) FROM main.\"{table}\" \
             WHERE provider = 'musicbrainz' AND trim(provider_artist_id) <> ''"
        ))
        .map_err(bundle_error)?;
    let rows = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(bundle_error)?;
    rows.collect::<Result<_, _>>().map_err(bundle_error)
}

/// The new id of every carried row that changes, and the rows kept apart.
fn plan(
    artists: &[Artist],
    votes: &[Vote],
    linked: &HashMap<String, String>,
) -> (BTreeMap<String, String>, ArtistRekey) {
    let mut outcome = ArtistRekey::default();
    let reserved: HashSet<&str> = artists
        .iter()
        .filter(|artist| artist.various)
        .map(|artist| artist.id.as_str())
        .chain([UNKNOWN_ARTIST_ID, VARIOUS_ARTISTS_ID])
        .collect();
    let by_id: HashMap<&str, &Artist> = artists
        .iter()
        .map(|artist| (artist.id.as_str(), artist))
        .collect();
    let name_of = |id: &str| {
        by_id
            .get(id)
            .map(|artist| artist.name.clone())
            .unwrap_or_default()
    };
    let mut tallies: HashMap<&str, Tally> = HashMap::new();
    for vote in votes {
        tallies
            .entry(vote.artist.as_str())
            .or_default()
            .add(&vote.key, vote.present);
    }

    // Live rows: each takes the key it has most votes for; when two want
    // one key, the one with more votes takes it.
    let mut wants: BTreeMap<String, Vec<(usize, &str)>> = BTreeMap::new();
    for artist in artists {
        if artist.retired_into.is_some() || reserved.contains(artist.id.as_str()) {
            continue;
        }
        if let Some((key, count)) = tallies
            .get(artist.id.as_str())
            .and_then(|tally| tally.ranked().into_iter().next())
        {
            wants
                .entry(key)
                .or_default()
                .push((count, artist.id.as_str()));
        }
    }
    let mut new_id: BTreeMap<String, String> = BTreeMap::new();
    for (key, mut rows) in wants {
        rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(b.1)));
        let mut rows = rows.into_iter();
        if let Some((_, winner)) = rows.next() {
            new_id.insert(winner.to_owned(), key);
        }
        for (_, loser) in rows {
            outcome.kept_apart.push(name_of(loser));
            if let Some(mbid) = linked.get(loser) {
                new_id.insert(loser.to_owned(), artist_mbid_key(mbid));
            }
        }
    }
    // A key equal to an id that stays (a sentinel, a Various Artists row,
    // a row kept under its v2 id) is never given out: the row that wanted
    // it keeps its v2 id. Each pass only removes rows, so the loop ends.
    loop {
        let staying: HashSet<&str> = artists
            .iter()
            .map(|artist| artist.id.as_str())
            .filter(|id| new_id.get(*id).is_none_or(|key| key == id))
            .chain(reserved.iter().copied())
            .collect();
        let clashing: Vec<String> = new_id
            .iter()
            .filter(|(id, key)| key != id && staying.contains(key.as_str()))
            .map(|(id, _)| id.clone())
            .collect();
        if clashing.is_empty() {
            break;
        }
        for id in clashing {
            new_id.remove(&id);
            outcome.kept_reserved.push(name_of(&id));
        }
    }

    // Rows v2 merged into another: a free key the survivor's credits under
    // the merged row's name land on, else the merged row's own name key.
    let mut used: HashSet<String> = artists
        .iter()
        .map(|artist| artist.id.clone())
        .chain(new_id.values().cloned())
        .chain(reserved.iter().map(|id| (*id).to_owned()))
        .collect();
    for artist in artists {
        if artist.retired_into.is_none() || reserved.contains(artist.id.as_str()) {
            continue;
        }
        let mut survivor = artist;
        for _ in 0..16 {
            match survivor
                .retired_into
                .as_deref()
                .and_then(|id| by_id.get(id))
            {
                Some(next) => survivor = next,
                None => break,
            }
        }
        let folded = fold_text(artist.name.trim());
        let mut tally = Tally::default();
        for vote in votes
            .iter()
            .filter(|vote| vote.artist == survivor.id && vote.folded == folded)
        {
            tally.add(&vote.key, vote.present);
        }
        let own = artist_key(artist.name.trim(), artist.sort_name.as_deref());
        let key = tally
            .ranked()
            .into_iter()
            .map(|(key, _)| key)
            .chain(std::iter::once(own))
            .find(|key| !used.contains(key));
        if let Some(key) = key {
            used.insert(key.clone());
            new_id.insert(artist.id.clone(), key);
        }
    }
    new_id.retain(|id, key| id != key);
    outcome.rekeyed = new_id.len();
    outcome.kept_apart.sort();
    outcome.kept_reserved.sort();
    (new_id, outcome)
}

/// A bundle column holding v2 artist ids: its section, its name, and the
/// column naming the item's kind when only `artist` rows hold one.
type ArtistColumn = (&'static str, &'static str, Option<&'static str>);

/// Every bundle column holding a v2 artist id: the artist rows' own id and
/// merge pointer, each column with an artist parent or an artist link,
/// and the names kept beside favorite artists.
fn artist_columns() -> Vec<ArtistColumn> {
    let mut out: Vec<ArtistColumn> = vec![
        (ARTISTS.name, "id", None),
        (ARTISTS.name, "retired_into_artist_id", None),
        (FAVORITE_NAMES.name, "item_id", Some("item_kind")),
    ];
    for section in ALL {
        for column in section.columns {
            match column.link.map(|link| link.kind) {
                Some(LinkKind::Artist) => out.push((section.name, column.name, None)),
                Some(LinkKind::ByColumn(kind)) => {
                    out.push((section.name, column.name, Some(kind)));
                }
                _ => {}
            }
        }
        for parent in section.parents {
            if parent.table == "local_artists" {
                for (column, _) in parent.columns {
                    out.push((section.name, column, None));
                }
            }
        }
    }
    let mut seen = HashSet::new();
    out.retain(|entry| seen.insert(*entry));
    out
}

/// Give every carried artist the id v3's scan lands its credits on, keep
/// each old id as a `legacy_artist` alias, and rewrite every reference in
/// the bundle. Refuses when a pair of artists would end up naming one row
/// on both sides. A bundle without a catalog is left as it is.
pub fn rekey_artists(conn: &Connection, now: f64) -> Result<ArtistRekey, ExportError> {
    let required: [(&str, &[&str]); 5] = [
        (
            ARTISTS.name,
            &["id", "display_name", "retired_into_artist_id", "kind"],
        ),
        (TRACKS.name, &["id", "local_album_id"]),
        (
            ALBUM_ARTISTS.name,
            &[
                "local_album_id",
                "position",
                "local_artist_id",
                "credited_name",
            ],
        ),
        (
            TRACK_ARTISTS.name,
            &[
                "local_track_id",
                "position",
                "local_artist_id",
                "credited_name",
            ],
        ),
        (
            ARTIST_ALIASES.name,
            &["alias", "local_artist_id", "kind", "created_at"],
        ),
    ];
    for (table, columns) in required {
        for column in columns {
            if !has(conn, table, column)? {
                return Ok(ArtistRekey::default());
            }
        }
    }
    let artists = read_artists(conn)?;
    let votes = votes(conn)?;
    let (new_id, mut outcome) = plan(&artists, &votes, &identities(conn)?);
    if new_id.is_empty() {
        return Ok(outcome);
    }

    conn.execute_batch(
        "CREATE TEMP TABLE artist_rekey (old TEXT PRIMARY KEY, new TEXT NOT NULL UNIQUE)",
    )
    .map_err(bundle_error)?;
    {
        let mut insert = conn
            .prepare("INSERT INTO temp.artist_rekey (old, new) VALUES (?1, ?2)")
            .map_err(bundle_error)?;
        for (old, new) in &new_id {
            insert.execute(params![old, new]).map_err(bundle_error)?;
        }
    }
    for (table, column, kind) in artist_columns() {
        if !has(conn, table, column)? {
            continue;
        }
        let only_artists = match kind {
            Some(kind) if has(conn, table, kind)? => format!(" AND \"{kind}\" = 'artist'"),
            Some(_) => continue,
            None => String::new(),
        };
        conn.execute(
            &format!(
                "UPDATE main.\"{table}\" SET \"{column}\" = \
                 (SELECT new FROM temp.artist_rekey WHERE old = \"{column}\") \
                 WHERE \"{column}\" IN (SELECT old FROM temp.artist_rekey){only_artists}"
            ),
            [],
        )
        .map_err(bundle_error)?;
    }
    // A dismissal names the smaller id on its left.
    let dismissals = ARTIST_DISMISSALS.name;
    if has(conn, dismissals, "left_artist_revision")?
        && has(conn, dismissals, "right_artist_revision")?
    {
        conn.execute(
            &format!(
                "UPDATE main.\"{dismissals}\" SET left_artist_id = right_artist_id, \
                 right_artist_id = left_artist_id, \
                 left_artist_revision = right_artist_revision, \
                 right_artist_revision = left_artist_revision \
                 WHERE left_artist_id > right_artist_id"
            ),
            [],
        )
        .map_err(bundle_error)?;
    } else if has(conn, dismissals, "left_artist_id")? {
        conn.execute(
            &format!(
                "UPDATE main.\"{dismissals}\" SET left_artist_id = right_artist_id, \
                 right_artist_id = left_artist_id WHERE left_artist_id > right_artist_id"
            ),
            [],
        )
        .map_err(bundle_error)?;
    }
    for pairs in [MERGE_CANDIDATES.name, dismissals] {
        if !has(conn, pairs, "left_artist_id")? {
            continue;
        }
        let collapsed: i64 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM main.\"{pairs}\" \
                     WHERE left_artist_id = right_artist_id"
                ),
                [],
                |row| row.get(0),
            )
            .map_err(bundle_error)?;
        if collapsed > 0 {
            return Err(ExportError::Bundle {
                reason: format!(
                    "re-keying artists would leave {collapsed} {pairs} row(s) naming one \
                     artist on both sides"
                ),
            });
        }
    }
    let aliases = ARTIST_ALIASES.name;
    let added = conn
        .execute(
            &format!(
                "INSERT INTO main.\"{aliases}\" (alias, local_artist_id, kind, created_at) \
                 SELECT lower(r.old), r.new, 'legacy_artist', ?1 FROM temp.artist_rekey r \
                 WHERE NOT EXISTS (SELECT 1 FROM main.\"{aliases}\" x \
                 WHERE x.alias IN (r.old, lower(r.old)))"
            ),
            params![now],
        )
        .map_err(bundle_error)?;
    outcome.aliases_added = u64::try_from(added).unwrap_or(u64::MAX);
    conn.execute_batch("DROP TABLE temp.artist_rekey")
        .map_err(bundle_error)?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artist(id: &str, name: &str, retired_into: Option<&str>) -> Artist {
        Artist {
            id: id.to_owned(),
            name: name.to_owned(),
            sort_name: None,
            retired_into: retired_into.map(str::to_owned),
            various: false,
        }
    }

    fn vote(artist: &str, name: &str, key: &str) -> Vote {
        Vote {
            artist: artist.to_owned(),
            folded: fold_text(name),
            key: key.to_owned(),
            present: true,
        }
    }

    /// Two rows that want one key: the one with more votes takes it, a
    /// linked loser takes the row v3 makes for its MusicBrainz id, an
    /// unlinked one keeps its v2 id, and both are named in the notes.
    #[test]
    fn rows_wanting_one_key_stay_apart() {
        let key = artist_key("Nirvana", None);
        let artists = [
            artist("v2-a", "Nirvana", None),
            artist("v2-b", "Nirvana", None),
            artist("v2-c", "Nirvana", None),
        ];
        let votes = [
            vote("v2-a", "Nirvana", &key),
            vote("v2-a", "Nirvana", &key),
            vote("v2-b", "Nirvana", &key),
            vote("v2-c", "Nirvana", &key),
        ];
        let linked: HashMap<String, String> = [(
            "v2-b".to_owned(),
            "5b11f4ce-a62d-471e-81fc-a69a8278c7da".to_owned(),
        )]
        .into();
        let (new_id, outcome) = plan(&artists, &votes, &linked);
        assert_eq!(new_id.get("v2-a"), Some(&key));
        assert_eq!(
            new_id.get("v2-b"),
            Some(&artist_mbid_key("5b11f4ce-a62d-471e-81fc-a69a8278c7da"))
        );
        assert_eq!(new_id.get("v2-c"), None);
        assert_eq!(outcome.kept_apart, ["Nirvana", "Nirvana"]);
    }
}
