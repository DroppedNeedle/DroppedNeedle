//! Two rows of one album, and which match survives: a scan that follows a
//! retag onto a new row, and a merge of two rows across scans.
//!
//! Nothing automatic picks between two different choices people made: both
//! settles refuse before writing. Otherwise a person's match beats an
//! automatic one, and the match that moves takes a revision neither row
//! has had, so nothing kept for either row reads as current against it.

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::remap::{RemapOrigin, queue_placement, queue_remap};
use super::sql;
use super::{DecisionSource, PROVIDER, Rows, delete_album_row};
use crate::identify::queue::bump_identification_stream;

/// An album's match, as far as settling two rows of one album reads it.
struct AlbumMatch {
    release_group: String,
    /// `None` for a match that names only the release group.
    release: Option<String>,
    source: DecisionSource,
}

impl AlbumMatch {
    fn of(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<Self>> {
        conn.prepare_cached(sql::MATCH_OF.text())?
            .query_row(params![album_id, PROVIDER], |row| {
                Ok(Self {
                    release_group: row.get(0)?,
                    release: row.get(1)?,
                    source: row.get(2)?,
                })
            })
            .optional()
    }

    /// Automatic and embedded matches stay revisable. A person's is one
    /// made by hand or one carried over from v2.
    fn revisable(&self) -> bool {
        self.source.is_revisable()
    }

    /// Whether this match and `other`, both a person's, are two different
    /// choices.
    ///
    /// A match carried over from v2 may name only the release group: a
    /// release of that group on the other row is the same album, said more
    /// exactly. A match a person made with no release is "leave unmanaged"
    /// or a custom edition, a decision of its own: a release on the other
    /// row is a different choice even within the group. Otherwise the two
    /// are the same when they name the same group and the same release.
    fn differs_from(&self, other: &Self) -> bool {
        if self.release_group != other.release_group {
            return true;
        }
        // Only a v2 choice may name the group alone and mean the release.
        let imported = |chosen: &Self| chosen.source == DecisionSource::LegacyImport;
        match (&self.release, &other.release) {
            (None, Some(_)) => !imported(self),
            (Some(_), None) => !imported(other),
            (mine, theirs) => mine != theirs,
        }
    }
}

/// Whether two rows of one album each hold a match a person chose (or an
/// imported one) and the two are different choices
/// ([`AlbumMatch::differs_from`]). Nothing automatic picks between two
/// people's choices.
pub(crate) fn chosen_matches_differ(
    conn: &Connection,
    album_id: &str,
    other_album_id: &str,
) -> rusqlite::Result<bool> {
    let (Some(one), Some(other)) = (
        AlbumMatch::of(conn, album_id)?,
        AlbumMatch::of(conn, other_album_id)?,
    ) else {
        return Ok(false);
    };
    Ok(!one.revisable() && !other.revisable() && one.differs_from(&other))
}

/// Whether the album's match is a release someone chose: made by a person
/// or carried over from v2, and naming a release. An automatic match does
/// not count, though it names a release too.
pub(crate) fn release_chosen(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    Ok(AlbumMatch::of(conn, album_id)?
        .is_some_and(|chosen| chosen.source.is_persons_choice() && chosen.release.is_some()))
}

/// Whether the album's match is one a person chose or v2 carried, naming
/// a release or not ("leave unmanaged" and a custom edition name none).
pub(crate) fn match_chosen(conn: &Connection, album_id: &str) -> rusqlite::Result<bool> {
    Ok(AlbumMatch::of(conn, album_id)?.is_some_and(|chosen| !chosen.revisable()))
}

/// The two rows hold different chosen matches: the caller keeps both.
fn refused() -> rusqlite::Error {
    rusqlite::Error::InvalidParameterName("the two rows hold different chosen matches".to_owned())
}

/// Settle the matches of two rows of one album before `gone` is deleted
/// and `kept` carries on: a scan made `gone` for the album while it was
/// being retagged, and a match or a review landed on it before the scan
/// had read the rest. When each row holds a match a person chose and the
/// two differ ([`chosen_matches_differ`]) it is refused with an error,
/// before anything is written: the caller keeps both rows.
///
/// - A person's match on `kept` stays, untouched. The match on `gone`
///   (automatic, or the same choice made again) goes with both its undo
///   rows.
/// - Otherwise the match on `gone` becomes the match of `kept`, with its
///   source and its "unsure" flag. It replaces an automatic one, as the
///   later verdict on the files as they are now tagged or a person's. It
///   also replaces an imported one that names only the release group,
///   when a person chose a release of that group for `gone`.
/// - A review still waiting on `gone` goes with it. One a person closed
///   moves to `kept`.
///
/// Rows made for a match that is no longer there are dropped, never
/// pointed at another match: both kinds of edition undo name the
/// revisions of the match they were made for. The moved match takes a
/// revision neither row has had, so nothing kept from either row reads as
/// current against it. An artist proof names the album revision it was
/// made at, so after the move it stops counting until the album's next
/// pass replaces it. A chosen edition still waiting to be placed on the
/// files is queued again for `kept`, and automatic management looks at
/// `kept` afresh. When the match `kept` ends with is a person's release
/// and a file is not on it yet, the files are queued to be placed on it,
/// changing nothing else ([`queue_placement`]).
pub(crate) fn settle_followed_match(
    tx: &Transaction<'_>,
    gone: &str,
    kept: &str,
    now: f64,
) -> rusqlite::Result<()> {
    let arriving = AlbumMatch::of(tx, gone)?;
    let staying = AlbumMatch::of(tx, kept)?;
    if let (Some(arriving), Some(staying)) = (&arriving, &staying)
        && !arriving.revisable()
        && !staying.revisable()
        && arriving.differs_from(staying)
    {
        return Err(refused());
    }
    // Of two equal choices the one that names the release is kept: the
    // one on `kept`, unless it names only the group and the other one
    // names the release.
    let arriving_names_release = arriving
        .as_ref()
        .is_some_and(|arriving| !arriving.revisable() && arriving.release.is_some());
    let kept_stays = staying.as_ref().is_some_and(|staying| {
        !staying.revisable() && (staying.release.is_some() || !arriving_names_release)
    });
    let managed: bool = tx
        .prepare_cached(sql::MANAGEMENT_QUEUED.text())?
        .query_row(params![gone], |row| row.get(0))?;
    let mut reviews = tx
        .prepare_cached(sql::MOVE_CLOSED_REVIEWS.text())?
        .execute(params![gone, kept])?;
    reviews += tx
        .prepare_cached(sql::DROP_REVIEWS.text())?
        .execute(params![gone])?;
    let moved = match arriving {
        None => false,
        Some(_) if kept_stays => {
            delete_album_row(tx, gone, Rows::Any)?;
            false
        }
        Some(arriving) => {
            carry_match(tx, gone, kept, !arriving.revisable(), now)?;
            true
        }
    };
    if managed || moved {
        crate::management::automatic::queue(tx, kept, now)?;
    }
    if reviews > 0 {
        bump_identification_stream(tx)?;
    }
    queue_placement(tx, kept, now)?;
    Ok(())
}

/// Put the match of `from` on `to`, with its source, its chooser and its
/// "unsure" flag, at a revision neither row has had. The match on `to` and
/// what was made for it go. A person's match (`chosen`) keeps the remap
/// still waiting to place it on the files. What is left on `from` is the
/// caller's.
fn carry_match(
    tx: &Transaction<'_>,
    from: &str,
    to: &str,
    chosen: bool,
    now: f64,
) -> rusqlite::Result<()> {
    let remap: Option<(String, Option<String>, RemapOrigin)> = tx
        .prepare_cached(sql::REMAP_TO_CARRY.text())?
        .query_row(params![from], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .optional()?
        .filter(|_| chosen);
    let revision: i64 = tx
        .prepare_cached(sql::NEXT_SHARED_REVISION.text())?
        .query_row(params![from, to], |row| row.get(0))?;
    for statement in [
        sql::DELETE_ALBUM_ROWS,
        sql::CLEAR_FLAG,
        sql::FORGET_CHOICE_UNDO,
        sql::FORGET_AUTOMATIC_UNDO,
        sql::DROP_REMAP,
    ] {
        tx.prepare_cached(statement.text())?.execute(params![to])?;
    }
    tx.prepare_cached(sql::CARRY_ALBUM_ROW.text())?
        .execute(params![from, to, revision])?;
    tx.prepare_cached(sql::MOVE_MATCH_STATE.text())?
        .execute(params![from, to])?;
    if let Some((release, chosen_by, origin)) = remap {
        queue_remap(tx, to, &release, chosen_by.as_deref(), now)?;
        tx.prepare_cached(sql::SET_REMAP_ORIGIN.text())?
            .execute(params![to, origin.as_str()])?;
    }
    Ok(())
}

/// Whether merging `old` into `new` puts the old row's match on `new`
/// ([`settle_merged_match`]): `old` has a match, and `new` has none, or an
/// automatic one where the old row's is a person's.
pub(crate) fn merge_moves_match(conn: &Connection, old: &str, new: &str) -> rusqlite::Result<bool> {
    let Some(arriving) = AlbumMatch::of(conn, old)? else {
        return Ok(false);
    };
    Ok(match AlbumMatch::of(conn, new)? {
        None => true,
        Some(staying) => !arriving.revisable() && staying.revisable(),
    })
}

/// Settle the matches of two rows of one album when `old` is merged into
/// `new` across two scans: the files left `old` in an earlier scan and sit
/// on `new`, which stays. When each row holds a match a person chose and
/// the two differ ([`chosen_matches_differ`]) it is refused with an error,
/// before anything is written.
///
/// - When [`merge_moves_match`]: the match on `old` becomes the match of
///   `new`, with its source, its chooser and its "unsure" flag, and a
///   person's keeps the remap still waiting to place it on the files. The
///   match on `new` and what was made for it go. A review waiting on `new`
///   is closed as a choice by hand closes one when a person's match
///   arrives (dismissed, by nobody), and goes when an automatic one does:
///   the offer after the merge judges the whole album.
/// - Otherwise the match on `new` stays as it is.
///
/// A review a person closed on `old` moves to `new`; one still waiting on
/// it goes. The moved match takes a revision neither row has had. When
/// the match `new` ends with is a person's release and a file on `new` is
/// not on it yet, the files are queued to be placed on it, changing
/// nothing else. What is left on `old` of its match is the caller's to
/// clear. Returns whether the match on `new` changed.
pub(crate) fn settle_merged_match(
    tx: &Transaction<'_>,
    old: &str,
    new: &str,
    now: f64,
) -> rusqlite::Result<bool> {
    if chosen_matches_differ(tx, old, new)? {
        return Err(refused());
    }
    let moves = merge_moves_match(tx, old, new)?;
    let mut reviews = tx
        .prepare_cached(sql::MOVE_CLOSED_REVIEWS.text())?
        .execute(params![old, new])?;
    reviews += tx
        .prepare_cached(sql::DROP_REVIEWS.text())?
        .execute(params![old])?;
    if let (true, Some(arriving)) = (moves, AlbumMatch::of(tx, old)?) {
        reviews += if arriving.revisable() {
            tx.prepare_cached(sql::DROP_PENDING_REVIEWS.text())?
                .execute(params![new])?
        } else {
            tx.prepare_cached(sql::DISMISS_PENDING_REVIEWS.text())?
                .execute(params![new, (now * 1000.0) as i64])?
        };
        carry_match(tx, old, new, !arriving.revisable(), now)?;
    }
    if reviews > 0 {
        bump_identification_stream(tx)?;
    }
    queue_placement(tx, new, now)?;
    Ok(moves)
}
