//! One refresh of a person's lists on rotation: look, pick, write.

use std::collections::HashMap;

use rusqlite::Transaction;

use super::store::{self, Change, Found, Written};
use super::{RotationIdea, Standing, pick, week_start};
use crate::collections::db::StoreError;
use crate::collections::service::import_entry;
use crate::collections::state::CollectionsState;
use crate::local::player::PlayerTrack;
use crate::local::stores::StoreError as LibraryStoreError;

/// Refresh `user_id`'s lists on rotation as of `now_ms`: read every idea
/// from one snapshot, pick the live ones, and write in one transaction
/// what changed. Safe to repeat: the same week and the same rows pick the
/// same lists and songs, and a list whose songs are as they were is not
/// written. `done` finishes the job in that transaction.
pub async fn refresh(
    state: &CollectionsState,
    user_id: &str,
    now_ms: i64,
    done: impl FnOnce(&Transaction) -> Result<(), StoreError> + Send + 'static,
) -> Result<Written, StoreError> {
    let look = store::look(&state.db, user_id, now_ms).await?;
    if !look.enabled {
        return store::write(&state.db, user_id, now_ms, Vec::new(), Vec::new(), done).await;
    }
    let week = week_start(now_ms);
    let row_of = |idea: RotationIdea| look.rows.iter().find(|row| row.idea == idea);
    let repick = look.rows.is_empty()
        || look
            .rows
            .iter()
            .any(|row| row.checked_at.is_none_or(|at| at.0 < week));
    let standings: Vec<Standing> = look
        .found
        .iter()
        .map(|found| {
            let row = row_of(found.idea);
            Standing {
                idea: found.idea,
                matches: found.matches,
                live: row.is_some_and(|row| row.playlist_id.is_some()),
                pinned: row.is_some_and(|row| row.pinned),
                last_live_at: row.and_then(|row| row.last_live_at.map(|at| at.0)),
            }
        })
        .collect();
    let live = pick(user_id, &standings, now_ms, repick);

    let mut wanted: Vec<&Found> = Vec::new();
    let mut changes = Vec::new();
    for found in &look.found {
        let row = row_of(found.idea);
        let mut playlist_id = row.and_then(|row| row.playlist_id.clone());
        // A decade list that moves to another decade is named for it, so it
        // goes and comes back as a new playlist.
        if found.idea == RotationIdea::Decade
            && let Some(id) = playlist_id.clone()
            && live.contains(&found.idea)
            && row.and_then(|row| row.decade) != look.decade
        {
            changes.push(Change::Drop {
                idea: found.idea,
                playlist_id: id,
            });
            playlist_id = None;
        }
        match (live.contains(&found.idea), playlist_id) {
            (true, Some(playlist_id)) => {
                let current = look.entries.get(&playlist_id).cloned().unwrap_or_default();
                let same = current.len() == found.ids.len()
                    && current
                        .iter()
                        .zip(&found.ids)
                        .all(|(file, id)| file.as_deref() == Some(id.as_str()));
                if same {
                    changes.push(Change::Keep { idea: found.idea });
                } else {
                    wanted.push(found);
                    changes.push(Change::Replace {
                        idea: found.idea,
                        playlist_id,
                        entries: Vec::new(),
                    });
                }
            }
            (true, None) => {
                wanted.push(found);
                let decade = (found.idea == RotationIdea::Decade)
                    .then_some(look.decade)
                    .flatten()
                    .and_then(|decade| i32::try_from(decade).ok());
                changes.push(Change::Create {
                    idea: found.idea,
                    name: found.idea.name(decade),
                    entries: Vec::new(),
                });
            }
            (false, Some(playlist_id)) => changes.push(Change::Drop {
                idea: found.idea,
                playlist_id,
            }),
            (false, None) => {}
        }
    }

    let ids: Vec<String> = wanted
        .iter()
        .flat_map(|found| found.ids.iter().cloned())
        .collect();
    let tracks: HashMap<String, PlayerTrack> = state
        .items
        .tracks_by_ids(&ids)
        .await
        .map_err(|error| match error {
            LibraryStoreError::Busy(operation) => StoreError::Busy(operation),
            LibraryStoreError::Internal(cause) => StoreError::Internal(cause),
        })?
        .into_iter()
        .map(|track| (track.id.clone(), track))
        .collect();
    let entries_of = |idea: RotationIdea| {
        look.found
            .iter()
            .find(|found| found.idea == idea)
            .map(|found| {
                found
                    .ids
                    .iter()
                    .filter_map(|id| tracks.get(id))
                    .map(import_entry)
                    .collect()
            })
            .unwrap_or_default()
    };
    for change in &mut changes {
        match change {
            Change::Create { idea, entries, .. } | Change::Replace { idea, entries, .. } => {
                *entries = entries_of(*idea);
            }
            Change::Keep { .. } | Change::Drop { .. } => {}
        }
    }

    let looked = look
        .found
        .iter()
        .map(|found| {
            let decade = (found.idea == RotationIdea::Decade && live.contains(&found.idea))
                .then_some(look.decade)
                .flatten();
            (found.idea, found.matches, decade)
        })
        .collect();
    store::write(&state.db, user_id, now_ms, looked, changes, done).await
}
