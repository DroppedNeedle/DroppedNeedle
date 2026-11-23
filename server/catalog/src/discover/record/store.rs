//! The pick record's writes from builds and the web UI, and forgetting a
//! person's rows.

use super::{MadeFrom, Offer, PickEvent};
use crate::discover::sql;
use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};

/// Upsert one row per offer for `user_id`, offered on `surface` at `now_ms`,
/// in one background write. A row offered before keeps its first surface and
/// date and takes the new evidence and names. Nothing is recorded for a
/// person who no longer exists, nor for an offer whose release group is
/// blank.
pub(super) async fn offered(
    db: &Db,
    user_id: &str,
    surface: MadeFrom,
    offers: Vec<Offer>,
    now_ms: i64,
    built: super::Built<'_>,
) -> Result<(), DbError> {
    let rows: Vec<(String, Offer, String)> = offers
        .into_iter()
        .filter_map(|offer| {
            let group = offer.release_group_mbid.trim().to_ascii_lowercase();
            let evidence = serde_json::to_string(&offer.evidence).ok()?;
            (!group.is_empty()).then_some((group, offer, evidence))
        })
        .collect();
    let user_id = user_id.to_owned();
    let (generation, forgets) = (built.generation, built.forgets.clone());
    db.write(Lane::Background, "discover.picks.offered", move |tx| {
        // Read on the writer, so a forget's delete and these rows are ordered.
        if !forgets.holds(&user_id, generation) {
            return Ok(());
        }
        let mut statement = tx.prepare_cached(sql::OFFER_PICK.text())?;
        for (group, offer, evidence) in rows {
            statement.execute(rusqlite::params![
                user_id,
                group,
                offer.release_group_mbid.trim(),
                offer.artist_mbid.trim(),
                offer.album_name,
                offer.artist_name,
                surface,
                Millis(now_ms),
                evidence,
            ])?;
        }
        Ok::<_, DbError>(())
    })
    .await
}

/// Stamp `event` at `now_ms` on `user_id`'s row for `group` (lowercased).
/// A missing row stays missing.
pub(super) async fn stamp(
    db: &Db,
    user_id: &str,
    group: &str,
    event: PickEvent,
    now_ms: i64,
) -> Result<(), DbError> {
    let (user_id, group) = (user_id.to_owned(), group.to_owned());
    let statement = match event {
        PickEvent::Shown => sql::PICK_SHOWN,
        PickEvent::Sampled => sql::PICK_SAMPLED,
        PickEvent::Opened => sql::PICK_OPENED,
    };
    db.write(Lane::Foreground, "discover.picks.event", move |tx| {
        tx.execute(
            statement.text(),
            rusqlite::params![user_id, group, Millis(now_ms)],
        )?;
        Ok::<_, DbError>(())
    })
    .await
}

/// Remove every row of `user_id`'s; answers how many went.
pub(super) async fn forget(db: &Db, user_id: &str) -> Result<usize, DbError> {
    let user_id = user_id.to_owned();
    db.write(Lane::Foreground, "discover.picks.forget", move |tx| {
        Ok::<_, DbError>(tx.execute(sql::FORGET_PICKS.text(), rusqlite::params![user_id])?)
    })
    .await
}
