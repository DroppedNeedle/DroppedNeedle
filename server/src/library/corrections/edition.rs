//! What a membership change does to album editions.
//!
//! An album's edition is its MusicBrainz identity row; a person's choice
//! (a pin) is that row with `decision_source = 'manual'`. The rules:
//!
//! - an album that keeps its id keeps its edition (a split or move leaves
//!   the source's pin where it is);
//! - an album emptied by the change hands its edition to the album its
//!   tracks went to, when that album has none and nothing competes;
//! - a person's choice (a pin) beats an automatic edition, whatever the
//!   conflict choice;
//! - otherwise, when the editions of the albums being combined differ, the
//!   receiving album keeps its own (`retain_manual`) or every one is
//!   dropped (`detach`); with no edition of its own and several on offer,
//!   none wins and all are dropped;
//! - a dropped edition leaves a pending review carrying the reason, with
//!   the dropped editions as its candidates (approving one puts it back),
//!   and the album is offered to identification again;
//! - a track keeps its track identity only when it sits on the receiving
//!   album's release;
//! - an album keeping a chosen edition that gains tracks queues them to be
//!   placed on that release.

use std::collections::BTreeSet;

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension as _, Transaction, params, params_from_iter};

use super::models::{DroppedEdition, EditionChange, EditionChangeKind, IdentityChoice};
use super::reasons;
use crate::library::identify::models::CandidateEvidence;
use crate::library::operations::choice::queue_remap;
use crate::library::operations::reasons::Reason;

const PROVIDER: &str = "musicbrainz";

/// An album's edition as this module needs it.
#[derive(Debug, Clone)]
pub(super) struct Identity {
    pub album_id: String,
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    pub decision_source: String,
    pub row_revision: i64,
    /// A custom edition is active: it lists this album's own tracks.
    pub custom: bool,
}

impl Identity {
    /// The edition this identity names: the release, else the group.
    fn key(&self) -> String {
        self.release_mbid
            .as_deref()
            .unwrap_or(&self.release_group_mbid)
            .to_lowercase()
    }

    fn pinned(&self) -> bool {
        self.decision_source == "manual"
    }

    fn dropped(&self) -> DroppedEdition {
        DroppedEdition {
            album_id: self.album_id.clone(),
            release_group_mbid: self.release_group_mbid.clone(),
            release_mbid: self.release_mbid.clone(),
            decision_source: self.decision_source.clone(),
        }
    }
}

pub(super) fn identity(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<Identity>> {
    conn.query_row(
        "SELECT release_group_mbid, release_mbid, decision_source, row_revision, \
         EXISTS(SELECT 1 FROM library_custom_edition_active c WHERE c.local_album_id = ?1) \
         FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2",
        params![album_id, PROVIDER],
        |row| {
            Ok(Identity {
                album_id: album_id.to_owned(),
                release_group_mbid: row.get(0)?,
                release_mbid: row.get(1)?,
                decision_source: row.get(2)?,
                row_revision: row.get(3)?,
                custom: row.get(4)?,
            })
        },
    )
    .optional()
}

pub(super) fn album_title(conn: &Connection, album_id: &str) -> rusqlite::Result<String> {
    Ok(conn
        .query_row(
            "SELECT title FROM local_albums WHERE id = ?1",
            params![album_id],
            |row| row.get(0),
        )
        .optional()?
        .unwrap_or_default())
}

/// One receiving album's settlement.
pub(super) struct Settle<'a> {
    /// The album that receives tracks and keeps its id.
    pub dest: &'a str,
    /// Albums emptied into `dest` by this change.
    pub retired_into: &'a [String],
    /// Tracks that joined `dest`.
    pub moved_tracks: &'a [String],
    pub choice: IdentityChoice,
    pub actor: &'a str,
    pub now: f64,
}

/// Competing editions and what happened to each album's edition.
#[derive(Debug, Default)]
pub(super) struct Settled {
    pub conflicts: Vec<String>,
    pub changes: Vec<EditionChange>,
}

/// Which edition the receiving album ends with.
#[derive(Debug, Clone, Copy)]
enum Verdict {
    /// No album involved has an edition.
    Nothing,
    KeepOwn(Reason),
    /// Take the edition of `incoming[index]`.
    Take(usize, Reason),
    DropAll(Reason),
}

/// The rules, in order: one edition on offer is kept or moves in (a pin
/// preferred over an automatic row naming the same edition); a single pin
/// among competing editions wins whatever the conflict choice; otherwise
/// the conflict choice decides, and several editions with none on the
/// receiving album means none wins. A custom edition never moves: it
/// lists its own album's tracks.
fn decide(
    own: Option<&Identity>,
    incoming: &[Identity],
    distinct: usize,
    choice: IdentityChoice,
) -> Verdict {
    // A custom edition cannot move; the receiving album then keeps its own
    // when that names the same edition.
    let take = |index: usize, reason: Reason| match (incoming[index].custom, own) {
        (false, _) => Verdict::Take(index, reason),
        (true, Some(own)) if own.key() == incoming[index].key() => {
            Verdict::KeepOwn(reasons::EDITION_KEPT)
        }
        (true, _) => Verdict::DropAll(reasons::EDITION_CUSTOM_CLEARED),
    };
    let incoming_pin = incoming.iter().position(Identity::pinned);
    let pins: BTreeSet<String> = own
        .into_iter()
        .chain(incoming.iter())
        .filter(|identity| identity.pinned())
        .map(Identity::key)
        .collect();
    let own_pinned = own.is_some_and(Identity::pinned);
    match (own, distinct) {
        (_, 0) => Verdict::Nothing,
        (Some(_), 1) => match incoming_pin {
            Some(index) if !own_pinned => take(index, reasons::EDITION_MOVED),
            _ => Verdict::KeepOwn(reasons::EDITION_KEPT),
        },
        (None, 1) => take(incoming_pin.unwrap_or(0), reasons::EDITION_MOVED),
        _ if pins.len() == 1 => {
            if own_pinned {
                Verdict::KeepOwn(reasons::EDITION_PIN_WINS)
            } else {
                match incoming_pin {
                    Some(index) => take(index, reasons::EDITION_PIN_WINS),
                    None => Verdict::DropAll(reasons::EDITION_AMBIGUOUS),
                }
            }
        }
        (Some(_), _)
            if choice == IdentityChoice::RetainManual && (own_pinned || pins.is_empty()) =>
        {
            Verdict::KeepOwn(reasons::EDITION_CONFLICT_KEPT)
        }
        (Some(_), _) if choice == IdentityChoice::Detach => {
            Verdict::DropAll(reasons::EDITION_CONFLICT_CLEARED)
        }
        _ => Verdict::DropAll(reasons::EDITION_AMBIGUOUS),
    }
}

pub(super) fn settle(tx: &Transaction<'_>, s: &Settle<'_>) -> rusqlite::Result<Settled> {
    let own = identity(tx, s.dest)?;
    let mut incoming = Vec::new();
    for album in s.retired_into {
        if let Some(found) = identity(tx, album)? {
            incoming.push(found);
        }
    }
    let all: Vec<&Identity> = own.iter().chain(incoming.iter()).collect();
    let keys: BTreeSet<String> = all.iter().map(|identity| identity.key()).collect();
    let conflicts: Vec<String> = if keys.len() > 1 {
        keys.iter().cloned().collect()
    } else {
        Vec::new()
    };
    let verdict = decide(own.as_ref(), &incoming, keys.len(), s.choice);
    let winner: Option<&Identity> = match verdict {
        Verdict::KeepOwn(_) => own.as_ref(),
        Verdict::Take(index, _) => incoming.get(index),
        Verdict::DropAll(_) | Verdict::Nothing => None,
    };
    let kept_key = winner.map(Identity::key);
    // Every identity that goes and names another edition than the one kept.
    let dropped: Vec<DroppedEdition> = all
        .iter()
        .filter(|identity| kept_key.as_deref() != Some(identity.key().as_str()))
        .map(|identity| identity.dropped())
        .collect();
    let title = album_title(tx, s.dest)?;
    let change = |kind: EditionChangeKind,
                  from: Option<&str>,
                  release: Option<&str>,
                  reason: Reason| EditionChange {
        album_id: s.dest.to_owned(),
        album_title: title.clone(),
        change: kind,
        from_album_id: from.map(str::to_owned),
        release_mbid: release.map(str::to_owned),
        reason,
        dropped: dropped.clone(),
    };
    let mut changes = Vec::new();
    // The album's own edition changed: every track is checked against the
    // new one, not just the moved ones.
    let mut replaced = false;
    let mut review: Option<Reason> = None;
    let kept: Option<Identity> = match verdict {
        Verdict::Nothing => None,
        Verdict::KeepOwn(reason) => {
            changes.push(change(
                EditionChangeKind::Kept,
                None,
                own.as_ref().and_then(|own| own.release_mbid.as_deref()),
                reason,
            ));
            review = Some(reason);
            own.clone()
        }
        Verdict::Take(index, reason) => {
            let from = incoming[index].clone();
            if own.is_some() {
                drop_edition(tx, s.dest)?;
                replaced = true;
            }
            move_edition(tx, &from.album_id, s.dest)?;
            changes.push(change(
                EditionChangeKind::Moved,
                Some(&from.album_id),
                from.release_mbid.as_deref(),
                reason,
            ));
            review = Some(reason);
            identity(tx, s.dest)?
        }
        Verdict::DropAll(reason) => {
            drop_edition(tx, s.dest)?;
            replaced = true;
            changes.push(change(EditionChangeKind::Cleared, None, None, reason));
            review = Some(reason);
            None
        }
    };
    for album in s.retired_into {
        drop_edition(tx, album)?;
    }
    if let Some(reason) = review
        && !dropped.is_empty()
    {
        open_review(tx, s.dest, reason, &title, &dropped)?;
    }
    let checked: Vec<String> = if replaced {
        album_tracks(tx, s.dest)?
    } else {
        s.moved_tracks.to_vec()
    };
    if checked.is_empty() {
        return Ok(Settled { conflicts, changes });
    }
    let release = kept
        .as_ref()
        .and_then(|identity| identity.release_mbid.clone());
    let marks = placeholders(checked.len());
    let mut values = vec![
        Value::Text(PROVIDER.to_owned()),
        release.map_or(Value::Null, Value::Text),
    ];
    values.extend(checked.iter().cloned().map(Value::Text));
    // Without an edition, no track keeps a release placement.
    tx.execute(
        &format!(
            "DELETE FROM local_track_external_identities WHERE provider = ?1 \
             AND release_mbid IS NOT NULL AND (?2 IS NULL OR lower(release_mbid) <> lower(?2)) \
             AND local_track_id IN ({marks})"
        ),
        params_from_iter(values),
    )?;
    if let Some(chosen) = kept
        .as_ref()
        .filter(|identity| identity.pinned() && !identity.custom)
        && let Some(release) = chosen.release_mbid.as_deref()
        && unplaced(tx, &checked, release)?
    {
        queue_remap(tx, s.dest, release, Some(s.actor), s.now)?;
        changes.push(EditionChange {
            album_id: s.dest.to_owned(),
            album_title: title.clone(),
            change: EditionChangeKind::RemapQueued,
            from_album_id: None,
            release_mbid: Some(release.to_owned()),
            reason: reasons::EDITION_REMAP,
            dropped: Vec::new(),
        });
    }
    Ok(Settled { conflicts, changes })
}

/// An album emptied with nowhere clear to go loses its edition. The review
/// that keeps what was dropped goes on `successor`, the album the old one
/// now points to (the old album's own reviews close when it retires).
pub(super) fn drop_scattered(
    tx: &Transaction<'_>,
    album_id: &str,
    successor: &str,
) -> rusqlite::Result<Option<EditionChange>> {
    let Some(found) = identity(tx, album_id)? else {
        return Ok(None);
    };
    let title = album_title(tx, album_id)?;
    let dropped = vec![found.dropped()];
    drop_edition(tx, album_id)?;
    open_review(tx, successor, reasons::EDITION_ALBUM_GONE, &title, &dropped)?;
    Ok(Some(EditionChange {
        album_id: album_id.to_owned(),
        album_title: title,
        change: EditionChangeKind::Cleared,
        from_album_id: None,
        release_mbid: found.release_mbid,
        reason: reasons::EDITION_ALBUM_GONE,
        dropped,
    }))
}

/// Move `from`'s edition, with its match state and pending remap, onto
/// `to` (which has none).
fn move_edition(tx: &Transaction<'_>, from: &str, to: &str) -> rusqlite::Result<()> {
    tx.execute(
        "UPDATE local_album_external_identities SET local_album_id = ?2, \
         row_revision = row_revision + 1 WHERE local_album_id = ?1 AND provider = ?3",
        params![from, to, PROVIDER],
    )?;
    for table in ["library_album_match_state", "library_edition_remap_queue"] {
        tx.execute(
            &format!("UPDATE OR REPLACE {table} SET local_album_id = ?2 WHERE local_album_id = ?1"),
            params![from, to],
        )?;
    }
    Ok(())
}

/// Remove an album's edition and everything that hangs off it.
fn drop_edition(tx: &Transaction<'_>, album_id: &str) -> rusqlite::Result<()> {
    tx.execute(
        "DELETE FROM local_album_external_identities WHERE local_album_id = ?1 AND provider = ?2",
        params![album_id, PROVIDER],
    )?;
    for table in [
        "library_album_match_state",
        "library_edition_choice_undo",
        "library_automatic_edition_undo",
        "library_edition_remap_queue",
        "library_custom_edition_active",
    ] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE local_album_id = ?1"),
            params![album_id],
        )?;
    }
    Ok(())
}

/// Leave a pending review saying why editions went. The dropped editions
/// are its candidates, so approving one puts that edition back.
fn open_review(
    tx: &Transaction<'_>,
    album_id: &str,
    reason: Reason,
    title: &str,
    dropped: &[DroppedEdition],
) -> rusqlite::Result<()> {
    let mut seen = BTreeSet::new();
    let candidates: Vec<CandidateEvidence> = dropped
        .iter()
        .filter(|edition| {
            seen.insert((
                edition.release_group_mbid.to_lowercase(),
                edition.release_mbid.as_deref().map(str::to_lowercase),
            ))
        })
        .map(|edition| CandidateEvidence {
            candidate_key: format!(
                "{}:{}",
                edition.release_group_mbid,
                edition.release_mbid.as_deref().unwrap_or_default()
            ),
            release_group_mbid: edition.release_group_mbid.clone(),
            release_mbid: edition.release_mbid.clone(),
            album_title: title.to_owned(),
            album_artist_name: String::new(),
            track_evidence: Vec::new(),
            score: 0.0,
            reason_code: reason.code.to_owned(),
            distance: 0.0,
            penalties: Vec::new(),
        })
        .collect();
    let candidates = serde_json::to_string(&candidates)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    let now_ms = crate::library::clock::now_ms() as i64;
    tx.execute(
        "INSERT INTO library_identify_reviews (id, local_album_id, reason_code, candidates_json, \
         state, created_ms, updated_ms) VALUES (?1, ?2, ?3, ?4, 'pending', ?5, ?5)",
        params![
            uuid::Uuid::new_v4().to_string(),
            album_id,
            reason.code,
            candidates,
            now_ms
        ],
    )?;
    Ok(())
}

fn album_tracks(tx: &Transaction<'_>, album_id: &str) -> rusqlite::Result<Vec<String>> {
    tx.prepare("SELECT id FROM local_tracks WHERE local_album_id = ?1")?
        .query_map(params![album_id], |row| row.get(0))?
        .collect()
}

/// Some of `tracks` is not placed on `release`.
fn unplaced(tx: &Transaction<'_>, tracks: &[String], release: &str) -> rusqlite::Result<bool> {
    let marks = placeholders(tracks.len());
    let placed: i64 = tx.query_row(
        &format!(
            "SELECT COUNT(*) FROM local_track_external_identities WHERE provider = 'musicbrainz' \
             AND lower(release_mbid) = lower(?1) AND release_track_mbid IS NOT NULL \
             AND local_track_id IN ({marks})"
        ),
        params_from_iter(std::iter::once(release).chain(tracks.iter().map(String::as_str))),
        |row| row.get(0),
    )?;
    Ok(placed < tracks.len() as i64)
}

pub(super) fn placeholders(n: usize) -> String {
    vec!["?"; n.max(1)].join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edition(album: &str, release: &str, source: &str) -> Identity {
        Identity {
            album_id: album.to_owned(),
            release_group_mbid: "rg".to_owned(),
            release_mbid: Some(release.to_owned()),
            decision_source: source.to_owned(),
            row_revision: 1,
            custom: false,
        }
    }

    /// The edition rules: a pin beats an automatic edition under either
    /// conflict choice; the same edition prefers the pinned row; otherwise
    /// the choice decides.
    #[test]
    fn pins_win_and_choices_decide_the_rest() {
        use IdentityChoice::{Detach, RetainManual};
        let auto_own = edition("dest", "r1", "automatic");
        let pin_own = edition("dest", "r1", "manual");
        let pin_in = [edition("src", "r2", "manual")];
        let auto_in = [edition("src", "r2", "automatic")];
        let same_pin_in = [edition("src", "r1", "manual")];
        let case = |own: Option<&Identity>, incoming: &[Identity], choice| {
            let keys: BTreeSet<String> = own
                .into_iter()
                .chain(incoming.iter())
                .map(Identity::key)
                .collect();
            match decide(own, incoming, keys.len(), choice) {
                Verdict::Nothing => "nothing",
                Verdict::KeepOwn(_) => "keep",
                Verdict::Take(..) => "take",
                Verdict::DropAll(_) => "drop",
            }
        };
        for choice in [Detach, RetainManual] {
            assert_eq!(case(Some(&auto_own), &pin_in, choice), "take");
            assert_eq!(case(Some(&pin_own), &auto_in, choice), "keep");
            assert_eq!(case(None, &pin_in, choice), "take");
            assert_eq!(case(Some(&auto_own), &same_pin_in, choice), "take");
        }
        let mut custom_same = edition("src", "r1", "manual");
        custom_same.custom = true;
        assert_eq!(case(Some(&auto_own), &[custom_same], Detach), "keep");
        assert_eq!(case(Some(&auto_own), &auto_in, Detach), "drop");
        assert_eq!(case(Some(&auto_own), &auto_in, RetainManual), "keep");
        let two_pins = [edition("a", "r2", "manual"), edition("b", "r3", "manual")];
        assert_eq!(case(None, &two_pins, RetainManual), "drop");
        assert_eq!(case(Some(&auto_own), &two_pins, RetainManual), "drop");
    }
}
