//! What a membership change does to album editions.
//!
//! An album's edition is its MusicBrainz identity row; a person's choice
//! (a pin) is that row decided by a person, now or in v2. The rules:
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
//! - an album that ends with an edition a person chose (or v2 carried),
//!   with tracks not yet on it, queues them to be placed on that release.
//!   Placing them changes nothing else: the edition, who chose it and
//!   "leave unmanaged" stay as they were.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};

use super::models::{DroppedEdition, EditionChange, EditionChangeKind, IdentityChoice};
use super::reasons;
use crate::edition::choice::queue_placement;
use crate::identify::models::CandidateEvidence;
use crate::identity::{self, DecisionSource};
use crate::reasons::Reason;

const PROVIDER: &str = "musicbrainz";

/// An album's edition as this module needs it.
#[derive(Debug, Clone)]
pub(super) struct Identity {
    pub album_id: String,
    pub release_group_mbid: String,
    pub release_mbid: Option<String>,
    pub decision_source: DecisionSource,
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
        self.decision_source.is_persons_choice()
    }

    fn dropped(&self) -> DroppedEdition {
        DroppedEdition {
            album_id: self.album_id.clone(),
            release_group_mbid: self.release_group_mbid.clone(),
            release_mbid: self.release_mbid.clone(),
            decision_source: self.decision_source,
        }
    }
}

pub(super) fn identity(conn: &Connection, album_id: &str) -> rusqlite::Result<Option<Identity>> {
    conn.query_row(
        crate::corrections::sql::IDENTITY.text(),
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

/// The edition as a card shows it and as a preview token covers it: its
/// revision with what it names and how it was chosen. A dropped edition
/// chosen again starts at revision 1, so the revision alone could match one
/// seen before the drop.
pub(super) fn identity_token(identity: Option<&Identity>) -> String {
    identity.map_or_else(
        || "0".to_owned(),
        |found| {
            format!(
                "{}:{}:{}:{}:{}",
                found.row_revision,
                found.decision_source.as_str(),
                found.release_group_mbid,
                found.release_mbid.as_deref().unwrap_or_default(),
                u8::from(found.custom)
            )
        },
    )
}

pub(super) fn album_title(conn: &Connection, album_id: &str) -> rusqlite::Result<String> {
    Ok(conn
        .query_row(
            crate::corrections::sql::ALBUM_TITLE.text(),
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
            identity::move_edition(tx, &from.album_id, s.dest)?;
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
        open_review(tx, s.dest, reason, &title, &dropped, s.now)?;
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
        .and_then(|identity| identity.release_mbid.as_deref());
    // Without an edition, no track keeps a release placement.
    identity::settle_tracks(tx, release, &checked)?;
    if let Some(release) = kept.and_then(|identity| identity.release_mbid)
        && queue_placement(tx, s.dest, s.now)?
    {
        changes.push(EditionChange {
            album_id: s.dest.to_owned(),
            album_title: title.clone(),
            change: EditionChangeKind::RemapQueued,
            from_album_id: None,
            release_mbid: Some(release),
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
    now: f64,
) -> rusqlite::Result<Option<EditionChange>> {
    let Some(found) = identity(tx, album_id)? else {
        return Ok(None);
    };
    let title = album_title(tx, album_id)?;
    let dropped = vec![found.dropped()];
    drop_edition(tx, album_id)?;
    open_review(
        tx,
        successor,
        reasons::EDITION_ALBUM_GONE,
        &title,
        &dropped,
        now,
    )?;
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

/// Remove an album's edition and everything that hangs off it, with no
/// undo ([`identity::drop_edition`]).
pub(crate) fn drop_edition(tx: &Transaction<'_>, album_id: &str) -> rusqlite::Result<()> {
    identity::drop_edition(tx, album_id)
}

/// Leave a pending review saying why editions went. The dropped editions
/// are its candidates, so approving one puts that edition back.
pub(crate) fn open_review(
    tx: &Transaction<'_>,
    album_id: &str,
    reason: Reason,
    title: &str,
    dropped: &[DroppedEdition],
    now: f64,
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
    let now_ms = crate::clock::ms(now);
    tx.execute(
        crate::corrections::sql::OPEN_REVIEW.text(),
        params![
            uuid::Uuid::new_v4().to_string(),
            album_id,
            reason.code,
            candidates,
            now_ms
        ],
    )?;
    crate::identify::sqlite::bump_identification_stream(tx)
}

fn album_tracks(tx: &Transaction<'_>, album_id: &str) -> rusqlite::Result<Vec<String>> {
    tx.prepare(crate::corrections::sql::ALBUM_TRACKS.text())?
        .query_map(params![album_id], |row| row.get(0))?
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edition(album: &str, release: &str, source: &str) -> Identity {
        Identity {
            album_id: album.to_owned(),
            release_group_mbid: "rg".to_owned(),
            release_mbid: Some(release.to_owned()),
            decision_source: DecisionSource::parse(source).expect("a decision source"),
            row_revision: 1,
            custom: false,
        }
    }

    /// The edition rules: a pin beats an automatic edition under either
    /// conflict choice, and an edition carried over from v2 is a person's
    /// choice, on the receiving album or arriving at it; the same edition
    /// prefers the pinned row; otherwise the choice decides.
    #[test]
    fn edition_conflicts_follow_their_rules() {
        use IdentityChoice::{Detach, RetainManual};
        let auto_own = edition("dest", "r1", "automatic");
        let pin_own = edition("dest", "r1", "manual");
        let v2_own = edition("dest", "r1", "legacy_import");
        let pin_in = [edition("src", "r2", "manual")];
        let auto_in = [edition("src", "r2", "automatic")];
        let v2_in = [edition("src", "r2", "legacy_import")];
        let same_pin_in = [edition("src", "r1", "manual")];
        let case = |own: Option<&Identity>, incoming: &[Identity], choice| {
            let keys: BTreeSet<String> = own
                .into_iter()
                .chain(incoming.iter())
                .map(Identity::key)
                .collect();
            match decide(own, incoming, keys.len(), choice) {
                Verdict::Nothing => "nothing".to_owned(),
                Verdict::KeepOwn(_) => "keep".to_owned(),
                Verdict::Take(index, _) => format!("take {index}"),
                Verdict::DropAll(_) => "drop".to_owned(),
            }
        };
        for choice in [Detach, RetainManual] {
            for (label, own, incoming, expected) in [
                ("pin over automatic", Some(&auto_own), &pin_in[..], "take 0"),
                (
                    "pin kept over automatic",
                    Some(&pin_own),
                    &auto_in[..],
                    "keep",
                ),
                ("pin to an empty album", None, &pin_in[..], "take 0"),
                (
                    "same edition, pinned row",
                    Some(&auto_own),
                    &same_pin_in[..],
                    "take 0",
                ),
                (
                    "v2 kept over automatic",
                    Some(&v2_own),
                    &auto_in[..],
                    "keep",
                ),
                ("v2 over automatic", Some(&auto_own), &v2_in[..], "take 0"),
            ] {
                assert_eq!(case(own, incoming, choice), expected, "{label}, {choice:?}");
            }
        }
        let mut custom_same = edition("src", "r1", "manual");
        custom_same.custom = true;
        assert_eq!(
            case(Some(&auto_own), &[custom_same], Detach),
            "keep",
            "custom never moves"
        );
        assert_eq!(case(Some(&auto_own), &auto_in, Detach), "drop", "detach");
        assert_eq!(
            case(Some(&auto_own), &auto_in, RetainManual),
            "keep",
            "retain"
        );
        let two_pins = [edition("a", "r2", "manual"), edition("b", "r3", "manual")];
        assert_eq!(
            case(None, &two_pins, RetainManual),
            "drop",
            "two pins, none own"
        );
        assert_eq!(
            case(Some(&auto_own), &two_pins, RetainManual),
            "drop",
            "two pins"
        );
    }
}
