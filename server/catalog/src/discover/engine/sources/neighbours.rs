//! The neighbours source: for each seed, the kept similar-artists lists,
//! read as rows only. Labs' list comes ordered through the one similar
//! answer ([`ListeningData::similar_artists_each`]); where the person's
//! Last.fm key works, Last.fm's kept list is read beside it, its match fame
//! divided the same way ([`rank::fame_divided`]). A list not kept is wanted
//! for the kept facts' refresh, as is a neighbour's listener count; nothing
//! is asked of a provider.

use std::collections::HashMap;

use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_providers::RequestPriority;
use rusqlite::params;

use super::super::rank::{self, ListRow, Seed, SeedLink, SeedLists};
use crate::discover::facts::{FactKind, FactSource, Kept, ListSource, Notes, WantOrder};
use crate::discover::listening::{Ask, ListeningData};
use crate::discover::sql;

/// What the seeds' lists give the ranker.
#[derive(Debug, Clone, Default)]
pub(crate) struct Neighbours {
    /// Each seed's links, in the seeds' order.
    pub links: Vec<Vec<SeedLink>>,
    /// The counted neighbours' listeners, 0 for one counted with none.
    pub listeners: HashMap<String, i64>,
    /// Whether a seed's labs list is not kept yet.
    pub lists_missing: bool,
    /// Whether a list or a neighbour's count was not had.
    pub incomplete: bool,
}

fn json(ids: &[String]) -> String {
    serde_json::to_string(ids).unwrap_or_else(|_| "[]".to_owned())
}

/// When each of `artists`' lists of `source` was fetched.
async fn fetched(
    db: &Db,
    artists: &[String],
    source: FactSource,
) -> Result<HashMap<String, i64>, DbError> {
    let asked = json(artists);
    db.read("discover.pool.lists_fetched", move |conn| {
        conn.prepare_cached(sql::FACT_FETCHES.text())?
            .query_map(
                params![FactKind::Neighbours, source, asked],
                sql::FactFetchColumns::read,
            )?
            .filter_map(|row| match row {
                Ok(row) => row.fetched_at.map(|Millis(at)| Ok((row.artist_mbid, at))),
                Err(error) => Some(Err(error)),
            })
            .collect::<rusqlite::Result<_>>()
            .map_err(DbError::from)
    })
    .await
}

/// Last.fm's kept lists of `artists`, by artist, each best first.
async fn lastfm_lists(
    db: &Db,
    artists: &[String],
) -> Result<HashMap<String, Vec<(String, Option<String>, f64)>>, DbError> {
    let asked = json(artists);
    let rows = db
        .read("discover.pool.lastfm_lists", move |conn| {
            conn.prepare_cached(sql::KEPT_NEIGHBOURS.text())?
                .query_map(
                    params![ListSource::LastFm, asked],
                    sql::KeptNeighbourColumns::read,
                )?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(DbError::from)
        })
        .await?;
    let mut lists: HashMap<String, Vec<(String, Option<String>, f64)>> = HashMap::new();
    for row in rows {
        lists.entry(row.artist_mbid).or_default().push((
            row.neighbour_mbid,
            row.neighbour_name,
            row.score,
        ));
    }
    Ok(lists)
}

/// The seeds' kept lists, for `user_id`; Last.fm's too when `lastfm` (their
/// Last.fm key works).
///
/// # Errors
///
/// A read of the kept rows failed.
pub(crate) async fn read(
    db: &Db,
    listening: &ListeningData,
    user_id: &str,
    seeds: &[Seed],
    lastfm: bool,
) -> Result<Neighbours, DbError> {
    let ids: Vec<String> = seeds.iter().map(|seed| seed.mbid.clone()).collect();
    let ask = Ask::build(RequestPriority::BackgroundSync);
    let answers = listening
        .similar_artists_each(user_id, &ids, usize::MAX, &ask)
        .await;
    let mut lists_missing = false;
    let labs: Vec<Vec<ListRow>> = answers
        .into_iter()
        .map(|answer| match answer {
            Ok(rows) => rows
                .into_iter()
                .filter_map(|row| {
                    let mbid = row.artist.mbid?.trim().to_ascii_lowercase();
                    let name = Some(row.artist.name).filter(|name| !name.is_empty());
                    Some(ListRow {
                        mbid,
                        name,
                        strength: row.score,
                    })
                })
                .collect(),
            Err(_) => {
                lists_missing = true;
                Vec::new()
            }
        })
        .collect();
    let mut incomplete = ask.met_failure() || lists_missing;
    let labs_at = fetched(db, &ids, FactSource::Labs).await?;
    let (mut lastfm_raw, lastfm_at) = if lastfm {
        (
            lastfm_lists(db, &ids).await?,
            fetched(db, &ids, FactSource::LastFm).await?,
        )
    } else {
        (HashMap::new(), HashMap::new())
    };

    let mut counted: Vec<String> = labs
        .iter()
        .flatten()
        .map(|row| row.mbid.clone())
        .chain(lastfm_raw.values().flatten().map(|row| row.0.clone()))
        .collect();
    counted.sort();
    counted.dedup();
    let facts = listening.facts();
    let looked = facts.listeners(&counted).await?;
    let now = facts.now();
    let mut notes = Notes::default();
    // An artist ListenBrainz counted with no listeners has none, which the
    // floors read; while its reads are off nothing is counted, and an
    // artist not counted yet stays unknown. The fame division reads only
    // real counts, as the one similar answer does.
    let counting = listening.listenbrainz_on();
    let mut fame = HashMap::new();
    let mut listeners = HashMap::new();
    for (id, looked) in &looked {
        match &looked.kept {
            Kept::Found(counts) => {
                if let Some(count) = counts.listeners {
                    fame.insert(id.clone(), count);
                }
                if counting || counts.listeners.is_some() {
                    listeners.insert(id.clone(), counts.listeners.unwrap_or(0));
                }
            }
            Kept::Nothing if counting => {
                listeners.insert(id.clone(), 0);
            }
            Kept::Nothing => {}
            Kept::Missing => incomplete = true,
        }
        notes.saw(
            now,
            (
                FactKind::Listeners,
                FactSource::ListenBrainz,
                WantOrder::Pool,
            ),
            id,
            looked,
        );
    }
    if let Err(cause) = facts.note(notes).await {
        tracing::warn!(%cause, "what the pool's neighbour read saw went unnoted");
    }

    let links = ids
        .iter()
        .zip(labs)
        .map(|(seed, labs)| {
            let lastfm = lastfm_raw
                .remove(seed)
                .map(|rows| rank::fame_divided(rows, &fame))
                .unwrap_or_default();
            rank::links(&SeedLists {
                labs,
                labs_fetched_at: labs_at.get(seed).copied(),
                lastfm,
                lastfm_fetched_at: lastfm_at.get(seed).copied(),
            })
        })
        .collect();
    Ok(Neighbours {
        links,
        listeners,
        lists_missing,
        incomplete,
    })
}
