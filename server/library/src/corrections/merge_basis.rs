//! What an artist merge rests on.
//!
//! Every merge retires a row through `artists::retire_artist`, which takes
//! a [`MergeBasis`] by value. A basis is made only here, by a constructor
//! that reads the rows itself in the transaction that merges them and
//! names both the row that stays and the row retired:
//!
//! - [`MergeBasis::proven_group`]: the live rows MusicBrainz proves to be
//!   one artist, all of a group or none of it;
//! - [`MergeBasis::followed`]: a row a pass of artist links left empty
//!   follows its proven credits to the row they moved to, while a current
//!   proof there still names the artist;
//! - [`MergeBasis::emptied_by_name`]: a row a retag emptied follows its
//!   credits by name, the one merge names alone decide;
//! - [`MergeBasis::person`]: an administrator chose the merge.
//!
//! MusicBrainz's special-purpose artist ids (`[unknown]` and the like)
//! never make a basis. A basis carries the proofs it found, at most
//! [`PROOF_LIST`] of them with their count, for the action log.
//!
//! The group rule: the survivor is the row a person or v2 linked, else the
//! row the app linked, else the oldest row proven by its files or by a
//! matched album's current proofs. A row is retired into it only when it
//! has no link and every one of its credits holds the artist, by its
//! file's id or by a current proof at that credit. A survivor in the group
//! only through a matched album's proofs takes others only when it has
//! credits and every one holds the artist; a survivor the app linked takes
//! them when that holds, or when its own files hold the artist's id and
//! nothing names another. One pair an administrator marked as distinct
//! anywhere in a group keeps the whole group apart.

use std::collections::BTreeSet;
use std::marker::PhantomData;

use rusqlite::{OptionalExtension as _, Transaction, params};
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization as _;

use super::artists::{MOVED_BY_NAME, MOVED_WITH_ID};
use crate::identify::artist_links::{CURRENT_PROOF, placeholder};
use crate::identity::DecisionSource;
use crate::scan::sqlite_store::artists::{NAME_MERGE_MARK, SENTINEL_ARTIST_IDS, proven_rows};
use droppedneedle_auth::Admin;

/// The most proofs one basis lists; the rest are only counted.
const PROOF_LIST: usize = 20;

/// The most of those that are the survivor's own credits, so the
/// retiree's always have room.
const SURVIVOR_CREDITS: usize = 6;

/// The basis of one row's merge into another. Made only by the
/// constructors here, from rows read in the transaction `'tx` borrows;
/// [`super::artists::retire_artist`] takes it by value.
pub(crate) struct MergeBasis<'tx> {
    survivor: String,
    retiree: String,
    kind: Kind,
    _tx: PhantomData<&'tx Transaction<'tx>>,
}

enum Kind {
    /// MusicBrainz proved both rows to be `mbid`.
    Proven {
        mbid: String,
        proofs: Vec<ArtistProof>,
        /// Every proof found, listed or not.
        proof_count: usize,
        /// How many of `proof_count` are the survivor's own.
        survivor_count: usize,
    },
    /// A retag moved every credit of the retiree by name to the survivor;
    /// the recorded moves' ids.
    EmptiedByName { moves: Vec<String> },
    /// An administrator chose the merge.
    PersonChose { user_id: String },
}

#[derive(PartialEq)]
struct ArtistProof {
    kind: ProofKind,
    /// The row the proof is about.
    artist: String,
}

#[derive(PartialEq)]
enum ProofKind {
    /// The credit's file holds the id.
    FileId { subject: Subject, position: i64 },
    /// A matched album's current proof at the credit (its evidence hash).
    ReleaseCredit { proof_id: String, release: String },
    /// The survivor's link.
    Link { source: DecisionSource },
    /// The survivor's link followed MusicBrainz's merge of this id.
    FollowedMerge { retired_mbid: String },
}

#[derive(PartialEq)]
enum Subject {
    Track(String),
    Album(String),
}

/// Where a row that could follow its credits stands.
pub(crate) enum Follow<'tx> {
    /// It merges on this basis.
    Ready(MergeBasis<'tx>),
    /// Some of its credits have not moved yet to this row.
    Waiting(String),
    /// It stays where it is.
    Stays,
}

impl MergeBasis<'_> {
    pub(super) fn survivor(&self) -> &str {
        &self.survivor
    }

    pub(super) fn retiree(&self) -> &str {
        &self.retiree
    }

    /// What `local_artists.converged_mbid` records on the retired row: the
    /// proving id, the name merge's mark, or nothing for a person's merge.
    pub(super) fn converged_mark(&self) -> Option<&str> {
        match &self.kind {
            Kind::Proven { mbid, .. } => Some(mbid),
            Kind::EmptiedByName { .. } => Some(NAME_MERGE_MARK),
            Kind::PersonChose { .. } => None,
        }
    }

    /// The MusicBrainz id the merge rests on, when it rests on one.
    pub(super) fn mbid(&self) -> Option<&str> {
        match &self.kind {
            Kind::Proven { mbid, .. } => Some(mbid),
            _ => None,
        }
    }

    /// The basis as the action log records it.
    pub(super) fn audit(&self) -> Value {
        Self::audit_group(std::slice::from_ref(self))
    }

    /// One basis for bases of one kind and one survivor: their proofs
    /// once each, counted once each.
    pub(super) fn audit_group(bases: &[Self]) -> Value {
        let Some(first) = bases.first() else {
            return Value::Null;
        };
        match &first.kind {
            Kind::Proven {
                mbid,
                survivor_count,
                ..
            } => {
                let mut proofs: Vec<&ArtistProof> = Vec::new();
                let mut count = *survivor_count;
                for basis in bases {
                    if let Kind::Proven {
                        proofs: own,
                        proof_count,
                        survivor_count,
                        ..
                    } = &basis.kind
                    {
                        count += proof_count - survivor_count;
                        for proof in own {
                            if !proofs.contains(&proof) {
                                proofs.push(proof);
                            }
                        }
                    }
                }
                json!({
                    "kind": "musicbrainz_proof",
                    "artist_mbid": mbid,
                    "proofs": proofs.iter().map(|proof| proof.json()).collect::<Vec<_>>(),
                    "proof_count": count,
                })
            }
            Kind::EmptiedByName { .. } => {
                let moves: Vec<&str> = bases
                    .iter()
                    .flat_map(|basis| match &basis.kind {
                        Kind::EmptiedByName { moves } => moves.as_slice(),
                        _ => &[],
                    })
                    .map(String::as_str)
                    .collect();
                json!({ "kind": "same_name", "moves": moves })
            }
            Kind::PersonChose { user_id } => json!({ "kind": "person", "user_id": user_id }),
        }
    }

    /// Whether an administrator chose the merge.
    pub(super) fn chosen_by_person(&self) -> bool {
        matches!(self.kind, Kind::PersonChose { .. })
    }

    /// Whether two bases are of one kind and rest on one id.
    pub(super) fn same_kind(&self, other: &Self) -> bool {
        match (&self.kind, &other.kind) {
            (Kind::Proven { mbid: a, .. }, Kind::Proven { mbid: b, .. }) => a == b,
            (Kind::EmptiedByName { .. }, Kind::EmptiedByName { .. }) => true,
            (Kind::PersonChose { user_id: a }, Kind::PersonChose { user_id: b }) => a == b,
            _ => false,
        }
    }
}

impl ArtistProof {
    fn json(&self) -> Value {
        let artist = &self.artist;
        match &self.kind {
            ProofKind::FileId { subject, position } => {
                let (kind, id) = match subject {
                    Subject::Track(id) => ("track", id),
                    Subject::Album(id) => ("album", id),
                };
                json!({ "kind": "file_id", "artist_id": artist, "subject_kind": kind,
                    "subject_id": id, "position": position })
            }
            ProofKind::ReleaseCredit { proof_id, release } => {
                json!({ "kind": "release_credit", "artist_id": artist,
                    "evidence_hash": proof_id, "release_mbid": release })
            }
            ProofKind::Link { source } => {
                json!({ "kind": "link", "artist_id": artist, "decision_source": source.as_str() })
            }
            ProofKind::FollowedMerge { retired_mbid } => {
                json!({ "kind": "followed_merge", "artist_id": artist,
                    "retired_mbid": retired_mbid })
            }
        }
    }
}

impl<'tx> MergeBasis<'tx> {
    fn new(survivor: &str, retiree: &str, kind: Kind) -> Self {
        Self {
            survivor: survivor.to_owned(),
            retiree: retiree.to_owned(),
            kind,
            _tx: PhantomData,
        }
    }

    /// The bases that merge every row of `mbid`'s group into its survivor
    /// (module docs), or none when the rule keeps any of it apart.
    pub(crate) fn proven_group(
        tx: &'tx Transaction<'_>,
        mbid: &str,
    ) -> rusqlite::Result<Option<Vec<Self>>> {
        if placeholder(mbid) {
            return Ok(None);
        }
        let rows = group(tx, mbid)?;
        let Some((survivor, others)) = rows.split_first() else {
            return Ok(None);
        };
        let mut retire = Vec::new();
        for row in others.iter().filter(|row| !row.linked) {
            if every_credit_holds(tx, &row.id, mbid)? {
                retire.push(row.id.as_str());
            } else {
                tracing::debug!(
                    artist_mbid = mbid,
                    artist_id = row.id,
                    "artist row has credits not proven to be this artist; not merged"
                );
            }
        }
        if retire.is_empty() {
            return Ok(None);
        }
        let takes_others = if survivor.proof_only {
            fully_proven(tx, &survivor.id, mbid)?
        } else if survivor.automatic {
            fully_proven(tx, &survivor.id, mbid)? || files_prove(tx, &survivor.id, mbid)?
        } else {
            true
        };
        if !takes_others {
            tracing::debug!(
                artist_mbid = mbid,
                artist_id = survivor.id,
                "survivor proven by a matched album has credits not proven to be this artist; \
                 not merged"
            );
            return Ok(None);
        }
        let ids: Vec<&str> = rows.iter().map(|row| row.id.as_str()).collect();
        if marked_distinct(tx, &ids)? {
            tracing::info!(
                artist_mbid = mbid,
                artist_ids = ?ids,
                "artist rows marked distinct by an administrator; not merged"
            );
            return Ok(None);
        }
        let mut bases = Vec::with_capacity(retire.len());
        for retiree in retire {
            match proven(tx, &survivor.id, retiree, mbid)? {
                Some(kind) => bases.push(Self::new(&survivor.id, retiree, kind)),
                None => return Ok(None),
            }
        }
        Ok(Some(bases))
    }

    /// Whether row `from`, whose credits proven to be `mbid` moved to the
    /// row that is now `to`'s live row, merges into it: once nothing of
    /// its own is left ([`HELD_BACK`], [`STILL_CREDITED`]), nothing proves
    /// that row another artist, no administrator marked the two distinct,
    /// and a current proof there still names `mbid`.
    pub(crate) fn followed(
        tx: &'tx Transaction<'_>,
        from: &str,
        to: &str,
        mbid: &str,
    ) -> rusqlite::Result<Follow<'tx>> {
        if placeholder(mbid) {
            return Ok(Follow::Stays);
        }
        let Some(survivor) = live_row(tx, to)? else {
            return Ok(Follow::Stays);
        };
        if survivor == from {
            return Ok(Follow::Stays);
        }
        if held_back(tx, from, &survivor, mbid)? {
            tracing::debug!(
                artist_mbid = mbid,
                artist_id = from,
                "artist row whose credits moved has something of its own; not merged"
            );
            return Ok(Follow::Stays);
        }
        if contradicted(tx, &survivor, mbid)? {
            tracing::debug!(
                artist_mbid = mbid,
                artist_id = survivor,
                "row the credits moved to is proven to be another artist; not merged"
            );
            return Ok(Follow::Stays);
        }
        if marked_distinct(tx, &[from, survivor.as_str()])? {
            tracing::info!(
                artist_mbid = mbid,
                artist_ids = ?[from, survivor.as_str()],
                "artist rows marked distinct by an administrator; not merged"
            );
            return Ok(Follow::Stays);
        }
        let (mut proofs, current) = release_proofs(tx, &survivor, mbid, PROOF_LIST - 2)?;
        if current == 0 {
            tracing::debug!(
                artist_mbid = mbid,
                artist_id = survivor,
                "no current proof names the artist on the row the credits moved to; not merged"
            );
            return Ok(Follow::Stays);
        }
        let credited: bool = tx
            .prepare_cached(STILL_CREDITED)?
            .query_row(params![from], |row| row.get(0))?;
        if credited {
            return Ok(Follow::Waiting(survivor));
        }
        let (mut links, linked) = link_proofs(tx, &survivor, mbid)?;
        links.append(&mut proofs);
        let count = linked + current;
        Ok(Follow::Ready(Self::new(
            &survivor,
            from,
            Kind::Proven {
                mbid: mbid.to_owned(),
                proofs: links,
                proof_count: count,
                survivor_count: count,
            },
        )))
    }

    /// Whether row `from`, which a scan left with no credits, merges into
    /// the one row its credits went to by name: every open move recorded
    /// for it went by name (no id in the file on either side, no proof at
    /// the old credit) to that one row, neither row has a link or a proof
    /// naming any id, the play guard holds, the two names differ only in
    /// sort name, capitals or spacing, and no administrator marked them
    /// distinct.
    pub(crate) fn emptied_by_name(
        tx: &'tx Transaction<'_>,
        from: &str,
    ) -> rusqlite::Result<Follow<'tx>> {
        let credited: bool = tx
            .prepare_cached(STILL_CREDITED)?
            .query_row(params![from], |row| row.get(0))?;
        if credited {
            return Ok(Follow::Stays);
        }
        let moves: Vec<(String, String, String)> = tx
            .prepare_cached(
                "SELECT id, right_artist_id, reason_code FROM local_artist_merge_candidates \
                 WHERE left_artist_id = ?1 AND reason_code IN (?2, ?3) AND state = 'open' \
                 ORDER BY id",
            )?
            .query_map(params![from, MOVED_BY_NAME, MOVED_WITH_ID], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<rusqlite::Result<_>>()?;
        if moves.is_empty() || moves.iter().any(|(_, _, reason)| reason == MOVED_WITH_ID) {
            return Ok(Follow::Stays);
        }
        let mut into = BTreeSet::new();
        for (_, to, _) in &moves {
            if let Some(live) = live_row(tx, to)?
                && live != from
            {
                into.insert(live);
            }
        }
        let mut into = into.into_iter();
        let (Some(survivor), None) = (into.next(), into.next()) else {
            return Ok(Follow::Stays);
        };
        // No link, no proof row naming any id (the column is never empty),
        // and the play guard.
        if held_back(tx, from, &survivor, "")? {
            return Ok(Follow::Stays);
        }
        let (Some(old), Some(new)) = (ordinary_name(tx, from)?, ordinary_name(tx, &survivor)?)
        else {
            return Ok(Follow::Stays);
        };
        if name_key(&old) != name_key(&new) || marked_distinct(tx, &[from, survivor.as_str()])? {
            return Ok(Follow::Stays);
        }
        Ok(Follow::Ready(Self::new(
            &survivor,
            from,
            Kind::EmptiedByName {
                moves: moves.into_iter().map(|(id, _, _)| id).collect(),
            },
        )))
    }

    /// An administrator's merge of live row `retiree` into live row
    /// `survivor`; none when either is not live or they are one row.
    pub(crate) fn person(
        tx: &'tx Transaction<'_>,
        admin: &Admin,
        survivor: &str,
        retiree: &str,
    ) -> rusqlite::Result<Option<Self>> {
        if survivor == retiree {
            return Ok(None);
        }
        let live: i64 = tx
            .prepare_cached(
                "SELECT COUNT(*) FROM local_artists WHERE id IN (?1, ?2) \
                 AND retired_into_artist_id IS NULL",
            )?
            .query_row(params![survivor, retiree], |row| row.get(0))?;
        Ok((live == 2).then(|| {
            Self::new(
                survivor,
                retiree,
                Kind::PersonChose {
                    user_id: admin.user_id().to_owned(),
                },
            )
        }))
    }
}

/// SQL that is true when artist `?1` is proven to be an artist other
/// than `?2`: a credit's file holds another id, or a current proof names
/// another. Each probe is a range on an (artist, id) index.
const OTHER_ID: &str = "(EXISTS(SELECT 1 FROM local_track_artists \
     WHERE local_artist_id = ?1 AND artist_mbid < ?2) \
     OR EXISTS(SELECT 1 FROM local_track_artists \
     WHERE local_artist_id = ?1 AND artist_mbid > ?2) \
     OR EXISTS(SELECT 1 FROM local_album_artists \
     WHERE local_artist_id = ?1 AND artist_mbid < ?2) \
     OR EXISTS(SELECT 1 FROM local_album_artists \
     WHERE local_artist_id = ?1 AND artist_mbid > ?2) \
     OR EXISTS(SELECT 1 FROM library_artist_credit_proofs p \
     WHERE p.source_local_artist_id = ?1 AND p.artist_mbid < ?2 AND (CURRENT_PROOF)) \
     OR EXISTS(SELECT 1 FROM library_artist_credit_proofs p \
     WHERE p.source_local_artist_id = ?1 AND p.artist_mbid > ?2 AND (CURRENT_PROOF)))";

/// The rows a current proof says are `?1`.
const PROOF_ROWS: &str = "SELECT DISTINCT p.source_local_artist_id \
     FROM library_artist_credit_proofs p WHERE p.artist_mbid = ?1 \
     AND p.source_local_artist_id IS NOT NULL AND (CURRENT_PROOF)";

/// The `created_at` of row `?1`, proven to be `?2` without a link, when it
/// belongs in that group: live, neither sentinel (`?3`, `?4`) nor a
/// Various Artists row, no link, and nothing proving it another artist.
const UNLINKED_MEMBER: &str = "SELECT created_at FROM local_artists WHERE id = ?1 \
     AND retired_into_artist_id IS NULL AND kind <> 'various_artists' AND id NOT IN (?3, ?4) \
     AND NOT EXISTS (SELECT 1 FROM local_artist_external_identities WHERE local_artist_id = ?1) \
     AND NOT OTHER_ID";

/// True when every track and album credit of artist `?1` holds `?2`: by
/// its file's id, or by a current proof of `?2` at that very credit.
const EVERY_CREDIT_HOLDS: &str = "SELECT NOT EXISTS (SELECT 1 FROM local_track_artists credit \
     WHERE credit.local_artist_id = ?1 AND credit.artist_mbid IS NOT ?2 \
     AND NOT EXISTS (SELECT 1 FROM library_artist_credit_proofs p \
      WHERE p.subject_kind = 'track' AND p.subject_id = credit.local_track_id \
      AND p.credit_position = credit.position AND p.source_local_artist_id = ?1 \
      AND p.artist_mbid = ?2 AND (CURRENT_PROOF))) \
     AND NOT EXISTS (SELECT 1 FROM local_album_artists credit \
     WHERE credit.local_artist_id = ?1 AND credit.artist_mbid IS NOT ?2 \
     AND NOT EXISTS (SELECT 1 FROM library_artist_credit_proofs p \
      WHERE p.subject_kind = 'album' AND p.subject_id = credit.local_album_id \
      AND p.credit_position = credit.position AND p.source_local_artist_id = ?1 \
      AND p.artist_mbid = ?2 AND (CURRENT_PROOF)))";

/// Up to `?3` of artist `?1`'s credits whose file holds `?2`.
const FILE_IDS: &str = "SELECT 'track', local_track_id, position FROM local_track_artists \
     WHERE local_artist_id = ?1 AND artist_mbid = ?2 \
     UNION ALL SELECT 'album', local_album_id, position FROM local_album_artists \
     WHERE local_artist_id = ?1 AND artist_mbid = ?2 LIMIT ?3";

/// How many of artist `?1`'s credits have a file holding `?2`.
const FILE_ID_COUNT: &str = "SELECT \
     (SELECT COUNT(*) FROM local_track_artists WHERE local_artist_id = ?1 AND artist_mbid = ?2) \
     + (SELECT COUNT(*) FROM local_album_artists WHERE local_artist_id = ?1 AND artist_mbid = ?2)";

/// Up to `?3` current proofs that artist `?1` is `?2`.
const RELEASE_PROOFS: &str = "SELECT p.evidence_hash, p.release_mbid \
     FROM library_artist_credit_proofs p WHERE p.source_local_artist_id = ?1 \
     AND p.artist_mbid = ?2 AND (CURRENT_PROOF) \
     ORDER BY p.subject_kind, p.subject_id, p.credit_position LIMIT ?3";

/// How many current proofs say artist `?1` is `?2`.
const RELEASE_PROOF_COUNT: &str = "SELECT COUNT(*) FROM library_artist_credit_proofs p \
     WHERE p.source_local_artist_id = ?1 AND p.artist_mbid = ?2 AND (CURRENT_PROOF)";

/// True when something other than credits still on it keeps row `?1`,
/// whose proven credits for `?3` moved to the row that is now `?2`, from
/// following them: it is not live, is a sentinel (`?4`, `?5`) or a Various
/// Artists row, has a link, has a proof naming another id, or has a play
/// of a track now credited to neither row (a play goes with its row, and
/// must not land on another artist's).
const HELD_BACK: &str = "SELECT NOT EXISTS (SELECT 1 FROM local_artists WHERE id = ?1 \
     AND retired_into_artist_id IS NULL AND kind <> 'various_artists' AND id NOT IN (?4, ?5)) \
     OR EXISTS (SELECT 1 FROM local_artist_external_identities WHERE local_artist_id = ?1) \
     OR EXISTS (SELECT 1 FROM library_artist_credit_proofs \
      WHERE (source_local_artist_id = ?1 OR local_artist_id = ?1) AND artist_mbid <> ?3) \
     OR EXISTS (SELECT 1 FROM library_play_history h WHERE h.local_artist_id = ?1 \
      AND EXISTS (SELECT 1 FROM local_track_artists c WHERE c.local_track_id = h.local_track_id) \
      AND NOT EXISTS (SELECT 1 FROM local_track_artists c \
       WHERE c.local_track_id = h.local_track_id AND c.local_artist_id IN (?1, ?2)))";

/// True when row `?1` still has a credit, or an album names it as album
/// artist: it follows its credits once the rest of them have moved.
const STILL_CREDITED: &str = "SELECT EXISTS (SELECT 1 FROM local_album_artists \
     WHERE local_artist_id = ?1) \
     OR EXISTS (SELECT 1 FROM local_track_artists WHERE local_artist_id = ?1) \
     OR EXISTS (SELECT 1 FROM local_albums WHERE album_artist_id = ?1)";

fn with_current(sql: &str) -> String {
    sql.replace("OTHER_ID", OTHER_ID)
        .replace("CURRENT_PROOF", CURRENT_PROOF)
}

/// One row of a group proven to be one artist.
struct Member {
    id: String,
    /// Proven by a link, not by its files or proofs.
    linked: bool,
    /// The link is the app's own.
    automatic: bool,
    /// In the group only through a matched album's current proofs.
    proof_only: bool,
}

/// The live rows proven to be `mbid`, the survivor first: the row a
/// person or v2 linked, then the row the app linked, then the rows with
/// no link proven by their files or by current proofs, oldest first
/// (`created_at`, then id). A row with no link is left out when anything
/// proves it another artist.
fn group(tx: &Transaction<'_>, mbid: &str) -> rusqlite::Result<Vec<Member>> {
    let mut members: Vec<Member> = Vec::new();
    let mut unlinked: Vec<(String, bool)> = Vec::new();
    for row in proven_rows(tx, mbid)? {
        if row.linked {
            members.push(Member {
                id: row.id,
                linked: true,
                automatic: row.automatic,
                proof_only: false,
            });
        } else {
            unlinked.push((row.id, false));
        }
    }
    let by_proofs: Vec<String> = tx
        .prepare_cached(&with_current(PROOF_ROWS))?
        .query_map(params![mbid], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for row in by_proofs {
        if !unlinked.iter().any(|(id, _)| *id == row)
            && !members.iter().any(|member| member.id == row)
        {
            unlinked.push((row, true));
        }
    }
    let member_sql = with_current(UNLINKED_MEMBER);
    let mut dated: Vec<(f64, String, bool)> = Vec::new();
    for (row, proof_only) in unlinked {
        let created: Option<f64> = tx
            .prepare_cached(&member_sql)?
            .query_row(
                params![row, mbid, SENTINEL_ARTIST_IDS[0], SENTINEL_ARTIST_IDS[1]],
                |found| found.get(0),
            )
            .optional()?;
        if let Some(created) = created {
            dated.push((created, row, proof_only));
        }
    }
    dated.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    members.extend(dated.into_iter().map(|(_, id, proof_only)| Member {
        id,
        linked: false,
        automatic: false,
        proof_only,
    }));
    Ok(members)
}

/// The proven basis of `retiree`'s merge into `survivor` as `mbid`: the
/// survivor's link, then the retiree's credits that hold the id, then the
/// survivor's. None when no credit of the retiree holds it.
fn proven(
    tx: &Transaction<'_>,
    survivor: &str,
    retiree: &str,
    mbid: &str,
) -> rusqlite::Result<Option<Kind>> {
    let (mut proofs, linked) = link_proofs(tx, survivor, mbid)?;
    let room = PROOF_LIST - proofs.len() - SURVIVOR_CREDITS;
    let (mut own, own_count) = credit_proofs(tx, retiree, mbid, room)?;
    if own_count == 0 {
        return Ok(None);
    }
    proofs.append(&mut own);
    let room = PROOF_LIST - proofs.len();
    let (mut theirs, their_count) = credit_proofs(tx, survivor, mbid, room)?;
    proofs.append(&mut theirs);
    Ok(Some(Kind::Proven {
        mbid: mbid.to_owned(),
        proofs,
        proof_count: linked + own_count + their_count,
        survivor_count: linked + their_count,
    }))
}

/// The survivor's link to `mbid`, and the merge it followed to get there,
/// with their count.
fn link_proofs(
    tx: &Transaction<'_>,
    artist: &str,
    mbid: &str,
) -> rusqlite::Result<(Vec<ArtistProof>, usize)> {
    let source: Option<DecisionSource> = tx
        .prepare_cached(
            "SELECT decision_source FROM local_artist_external_identities \
             WHERE local_artist_id = ?1 AND provider = 'musicbrainz' \
             AND lower(provider_artist_id) = ?2",
        )?
        .query_row(params![artist, mbid], |row| row.get(0))
        .optional()?;
    let Some(source) = source else {
        return Ok((Vec::new(), 0));
    };
    let mut proofs = vec![ArtistProof {
        kind: ProofKind::Link { source },
        artist: artist.to_owned(),
    }];
    let followed: Option<String> = tx
        .prepare_cached(
            "SELECT json_extract(before_json, '$.provider_artist_id') \
             FROM library_catalog_actions WHERE local_artist_id = ?1 \
             AND reason_code = 'MUSICBRAINZ_MERGED_ARTIST_ID' \
             AND json_valid(after_json) AND json_valid(before_json) \
             AND json_extract(after_json, '$.provider_artist_id') = ?2 \
             ORDER BY created_at DESC LIMIT 1",
        )?
        .query_row(params![artist, mbid], |row| row.get(0))
        .optional()?
        .flatten();
    if let Some(retired_mbid) = followed {
        proofs.push(ArtistProof {
            kind: ProofKind::FollowedMerge { retired_mbid },
            artist: artist.to_owned(),
        });
    }
    let count = proofs.len();
    Ok((proofs, count))
}

/// Up to `limit` of `artist`'s credits that hold `mbid`, by file id first
/// and then by current proof, with how many there are in all.
fn credit_proofs(
    tx: &Transaction<'_>,
    artist: &str,
    mbid: &str,
    limit: usize,
) -> rusqlite::Result<(Vec<ArtistProof>, usize)> {
    let mut proofs: Vec<ArtistProof> = tx
        .prepare_cached(FILE_IDS)?
        .query_map(params![artist, mbid, limit as i64], |row| {
            let kind: String = row.get(0)?;
            let id: String = row.get(1)?;
            let subject = if kind == "track" {
                Subject::Track(id)
            } else {
                Subject::Album(id)
            };
            Ok(ArtistProof {
                kind: ProofKind::FileId {
                    subject,
                    position: row.get(2)?,
                },
                artist: artist.to_owned(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let files: i64 = tx
        .prepare_cached(FILE_ID_COUNT)?
        .query_row(params![artist, mbid], |row| row.get(0))?;
    let room = limit.saturating_sub(proofs.len());
    let (mut releases, current) = release_proofs(tx, artist, mbid, room)?;
    proofs.append(&mut releases);
    Ok((proofs, files as usize + current))
}

/// Up to `limit` current proofs that `artist` is `mbid`, with how many
/// there are in all.
fn release_proofs(
    tx: &Transaction<'_>,
    artist: &str,
    mbid: &str,
    limit: usize,
) -> rusqlite::Result<(Vec<ArtistProof>, usize)> {
    let proofs: Vec<ArtistProof> = tx
        .prepare_cached(&with_current(RELEASE_PROOFS))?
        .query_map(params![artist, mbid, limit as i64], |row| {
            Ok(ArtistProof {
                kind: ProofKind::ReleaseCredit {
                    proof_id: row.get(0)?,
                    release: row.get(1)?,
                },
                artist: artist.to_owned(),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let count: i64 = tx
        .prepare_cached(&with_current(RELEASE_PROOF_COUNT))?
        .query_row(params![artist, mbid], |row| row.get(0))?;
    Ok((proofs, count as usize))
}

/// Whether every track and album credit of `artist` holds `mbid`, by its
/// file's id or by a current proof at that credit.
fn every_credit_holds(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    tx.prepare_cached(&with_current(EVERY_CREDIT_HOLDS))?
        .query_row(params![artist, mbid], |row| row.get(0))
}

/// Whether `artist` has a credit, every credit holds `mbid`, and nothing
/// proves it another artist: what a survivor proven by a matched album
/// needs before it takes in other rows.
fn fully_proven(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    let credited: bool = tx
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM local_track_artists WHERE local_artist_id = ?1) \
             OR EXISTS (SELECT 1 FROM local_album_artists WHERE local_artist_id = ?1)",
        )?
        .query_row(params![artist], |row| row.get(0))?;
    Ok(credited && every_credit_holds(tx, artist, mbid)? && !other_id(tx, artist, mbid)?)
}

/// Whether anything proves `artist` to be an artist other than `mbid`.
fn other_id(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    tx.prepare_cached(&with_current("SELECT OTHER_ID"))?
        .query_row(params![artist, mbid], |row| row.get(0))
}

/// Whether a file of one of `artist`'s credits holds `mbid` and nothing
/// proves it another artist: what lets a survivor the app linked take in
/// others while a credit of its own is not proven.
fn files_prove(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    let held: bool = tx
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM local_track_artists \
             WHERE local_artist_id = ?1 AND artist_mbid = ?2) \
             OR EXISTS (SELECT 1 FROM local_album_artists \
             WHERE local_artist_id = ?1 AND artist_mbid = ?2)",
        )?
        .query_row(params![artist, mbid], |row| row.get(0))?;
    Ok(held && !other_id(tx, artist, mbid)?)
}

/// [`HELD_BACK`] for row `from` and the row `survivor` its credits went
/// to, as `mbid`.
fn held_back(
    tx: &Transaction<'_>,
    from: &str,
    survivor: &str,
    mbid: &str,
) -> rusqlite::Result<bool> {
    tx.prepare_cached(HELD_BACK)?.query_row(
        params![
            from,
            survivor,
            mbid,
            SENTINEL_ARTIST_IDS[0],
            SENTINEL_ARTIST_IDS[1]
        ],
        |row| row.get(0),
    )
}

/// The live row `artist` is now: itself, or the row a chain of merges
/// retired it into.
fn live_row(tx: &Transaction<'_>, artist: &str) -> rusqlite::Result<Option<String>> {
    tx.prepare_cached(
        "WITH RECURSIVE chain(id, next, depth) AS ( \
         SELECT id, retired_into_artist_id, 0 FROM local_artists WHERE id = ?1 \
         UNION ALL SELECT a.id, a.retired_into_artist_id, chain.depth + 1 \
          FROM local_artists a JOIN chain ON a.id = chain.next WHERE chain.depth < 16) \
         SELECT id FROM chain WHERE next IS NULL LIMIT 1",
    )?
    .query_row(params![artist], |row| row.get(0))
    .optional()
}

/// Whether `artist` is proven to be an artist other than `mbid`: a link
/// to another id, a file holding another id, or a current proof naming
/// another.
fn contradicted(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    let link: Option<String> = tx
        .prepare_cached(
            "SELECT lower(provider_artist_id) FROM local_artist_external_identities \
             WHERE local_artist_id = ?1 AND provider = 'musicbrainz'",
        )?
        .query_row(params![artist], |row| row.get(0))
        .optional()?;
    Ok(link.is_some_and(|link| link != mbid) || other_id(tx, artist, mbid)?)
}

/// Whether an administrator marked any two of the rows as distinct
/// artists, at any revision, either row possibly merged since.
fn marked_distinct(tx: &Transaction<'_>, ids: &[&str]) -> rusqlite::Result<bool> {
    let ids = serde_json::to_string(ids).unwrap_or_default();
    tx.prepare_cached(
        "WITH members(id) AS (SELECT value FROM json_each(?1)), \
         pairs AS (SELECT \
          COALESCE((SELECT local_artist_id FROM local_artist_aliases \
           WHERE alias = d.left_artist_id), d.left_artist_id) AS left_id, \
          COALESCE((SELECT local_artist_id FROM local_artist_aliases \
           WHERE alias = d.right_artist_id), d.right_artist_id) AS right_id \
          FROM library_artist_reconciliation_dismissals d) \
         SELECT EXISTS(SELECT 1 FROM pairs WHERE left_id <> right_id \
          AND left_id IN (SELECT id FROM members) AND right_id IN (SELECT id FROM members))",
    )?
    .query_row(params![ids], |row| row.get(0))
}

/// A name as [`MergeBasis::emptied_by_name`] compares it: composed,
/// case-folded and with its spacing collapsed. Accents and other marks are
/// kept, so a name that differs by one is another name.
fn name_key(name: &str) -> String {
    let composed: String = name.nfc().collect();
    caseless::default_case_fold_str(&composed)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The display name of live row `artist` when it is an ordinary row:
/// neither a sentinel nor a Various Artists row.
fn ordinary_name(tx: &Transaction<'_>, artist: &str) -> rusqlite::Result<Option<String>> {
    tx.prepare_cached(
        "SELECT display_name FROM local_artists WHERE id = ?1 \
         AND retired_into_artist_id IS NULL AND kind <> 'various_artists' AND id NOT IN (?2, ?3)",
    )?
    .query_row(
        params![artist, SENTINEL_ARTIST_IDS[0], SENTINEL_ARTIST_IDS[1]],
        |row| row.get(0),
    )
    .optional()
}

#[cfg(test)]
mod tests {
    use super::name_key;

    /// Names that differ only in capitals or spacing merge by name; one
    /// accent or other mark makes another name. The sort name is never
    /// part of the key, so a sort change alone always matches.
    #[test]
    fn a_name_merge_keeps_marks_apart() {
        assert_eq!(name_key("The  Oh"), name_key("the oh"));
        assert_eq!(name_key(" BEYONCÉ"), name_key("Beyoncé"));
        assert_ne!(name_key("Beyonce"), name_key("Beyoncé"));
        assert_ne!(name_key("ガゼット"), name_key("カセット"));
    }
}
