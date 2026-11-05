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
//! - [`MergeBasis::same_name`]: live records whose names have the same
//!   `artist_name_key` and that nothing tells apart, the one merge names
//!   decide, and the only one that keeps an undo;
//! - [`MergeBasis::v2_placeholder`]: the upgrade folds a v2 row named
//!   exactly "Unknown Artist" into the built-in one;
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

use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;

use rusqlite::{OptionalExtension as _, Transaction, params};
use serde_json::{Value, json};

use crate::corrections::sql::{
    EVERY_CREDIT_HOLDS, FILE_ID_COUNT, FILE_IDS, HELD_BACK, PROOF_ROWS, RELEASE_PROOF_COUNT,
    RELEASE_PROOFS, STILL_CREDITED, UNLINKED_MEMBER,
};
use crate::identify::artist_links::placeholder;
use crate::identify::merged_ids;
use crate::identity::DecisionSource;
use crate::scan::naming::{UNKNOWN_ARTIST, UNKNOWN_ARTIST_ID, artist_name_key};
use crate::scan::sqlite_store::artists::{
    NAME_MERGE_MARK, SENTINEL_ARTIST_IDS, UNKEYED_NAMES, proven_rows,
};
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
    /// The records share a name key and nothing tells them apart: the key,
    /// and each member with the MusicBrainz ids it holds.
    SameName {
        key: String,
        members: Vec<(String, Vec<String>)>,
    },
    /// The upgrade folded v2's placeholder row of this name.
    V2Placeholder { name: String },
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

/// Why a v2 "Unknown Artist" row stays apart from the built-in one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum V2PlaceholderKept {
    /// It is not a live ordinary row of that name, or the built-in row is
    /// not live.
    NotPlaceholder,
    Linked,
    SourceLink,
    NamesRealId,
    MixedCredit,
    MarkedDistinct,
    /// The store failed on the merge.
    MergeFailed,
    /// The store failed moving on what landed after the merge.
    LateRowsFailed,
}

impl V2PlaceholderKept {
    /// The reason as the upgrade report words it.
    #[must_use]
    pub fn sentence(self) -> &'static str {
        match self {
            Self::NotPlaceholder => "it is not a live artist of exactly that name",
            Self::Linked => "it is linked to a real artist",
            Self::SourceLink => "it has a link to a page on another service",
            Self::NamesRealId => "a credit or proof on it names a real MusicBrainz artist",
            Self::MixedCredit => "some of its credits are under another name",
            Self::MarkedDistinct => "an admin marked the two as different artists",
            Self::MergeFailed => {
                "the merge failed. Its songs, plays and favorites stay on it and nothing is \
                 lost. To try again, run droppedneedle-tool upgrade --undo --yes and then the \
                 upgrade again, before you start using v3"
            }
            Self::LateRowsFailed => {
                "the merge happened, but rows that landed on it later could not move and stay \
                 on it. Nothing is lost"
            }
        }
    }
}

/// What the fold of a v2 "Unknown Artist" row looks at, read from the
/// database for a real fold and from the export bundle for a dry run.
#[derive(Debug, Default)]
pub struct V2PlaceholderFacts {
    /// The ids its links name.
    pub linked_ids: Vec<String>,
    /// It has a link to a page on another service.
    pub source_link: bool,
    /// The MusicBrainz ids its credits and proofs name.
    pub named_ids: Vec<String>,
    /// A credit or album stands under another name.
    pub other_credit: bool,
    /// An administrator marked it and the built-in row as distinct.
    pub marked_distinct: bool,
}

impl V2PlaceholderFacts {
    /// Ok when the row folds. A special-purpose MusicBrainz id is not a
    /// real one, so it never keeps a row apart.
    pub fn judge(&self) -> Result<(), V2PlaceholderKept> {
        let real = |id: &String| !placeholder(&id.trim().to_lowercase());
        if self.linked_ids.iter().any(real) {
            Err(V2PlaceholderKept::Linked)
        } else if self.source_link {
            Err(V2PlaceholderKept::SourceLink)
        } else if self.named_ids.iter().any(real) {
            Err(V2PlaceholderKept::NamesRealId)
        } else if self.other_credit {
            Err(V2PlaceholderKept::MixedCredit)
        } else if self.marked_distinct {
            Err(V2PlaceholderKept::MarkedDistinct)
        } else {
            Ok(())
        }
    }
}

/// What the same-name rule makes of one group of records
/// ([`MergeBasis::same_name`]).
pub(crate) enum SameName<'tx> {
    /// They join: one basis per record retired, and the sort name the
    /// survivor takes when it has none.
    Join {
        bases: Vec<MergeBasis<'tx>>,
        sort_fill: Option<String>,
    },
    /// Two MusicBrainz ids or more keep them apart: those ids.
    Apart(BTreeSet<String>),
    /// Nothing joins.
    Stays,
}

/// One record of a same-name group, with the MusicBrainz ids it holds.
struct Namesake {
    row: crate::corrections::sql::Namesake,
    ids: BTreeSet<String>,
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
            Kind::SameName { .. } | Kind::V2Placeholder { .. } => Some(NAME_MERGE_MARK),
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
            Kind::SameName { key, members } => json!({
                "kind": "same_name",
                "name_key": key,
                "members": members
                    .iter()
                    .map(|(id, mbids)| json!({ "artist_id": id, "artist_mbids": mbids }))
                    .collect::<Vec<_>>(),
            }),
            Kind::V2Placeholder { name } => json!({ "kind": "v2_placeholder", "name": name }),
            Kind::PersonChose { user_id } => json!({ "kind": "person", "user_id": user_id }),
        }
    }

    /// Whether the merge keeps a record its undo puts back: only a
    /// same-name join does.
    pub(super) fn keeps_undo(&self) -> bool {
        matches!(self.kind, Kind::SameName { .. })
    }

    /// Whether an administrator chose the merge.
    pub(super) fn chosen_by_person(&self) -> bool {
        matches!(self.kind, Kind::PersonChose { .. })
    }

    /// Whether two bases are of one kind and rest on one id.
    pub(super) fn same_kind(&self, other: &Self) -> bool {
        match (&self.kind, &other.kind) {
            (Kind::Proven { mbid: a, .. }, Kind::Proven { mbid: b, .. }) => a == b,
            (Kind::SameName { key: a, .. }, Kind::SameName { key: b, .. }) => a == b,
            (Kind::V2Placeholder { .. }, Kind::V2Placeholder { .. }) => true,
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
            .prepare_cached(STILL_CREDITED.text())?
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

    /// The join of live records `ids`, whose names have one
    /// [`artist_name_key`], read again here. A record whose own MusicBrainz
    /// ids (its link, its credits' files and its current proofs, as
    /// MusicBrainz merged them) number two or more sits out. The rest join
    /// when they hold one id between them or none, and no administrator
    /// marked any two of them distinct; two ids or more keep them apart.
    /// The survivor is the record a person or v2 linked, else the one the
    /// app linked, else one holding the id, else the one with most credits
    /// (indexed tracks, then live albums), else the oldest.
    pub(crate) fn same_name(
        tx: &'tx Transaction<'_>,
        ids: &[String],
    ) -> rusqlite::Result<SameName<'tx>> {
        let mut key: Option<String> = None;
        let mut members: Vec<Namesake> = Vec::new();
        for id in ids {
            let row = tx
                .prepare_cached(crate::corrections::sql::NAMESAKE.text())?
                .query_row(
                    params![id, SENTINEL_ARTIST_IDS[0], SENTINEL_ARTIST_IDS[1]],
                    crate::corrections::sql::Namesake::read,
                )
                .optional()?;
            let Some(row) = row else { continue };
            let own_key = artist_name_key(&row.display_name);
            if UNKEYED_NAMES.contains(&own_key.as_str()) {
                return Ok(SameName::Stays);
            }
            match &key {
                Some(key) if *key != own_key => return Ok(SameName::Stays),
                Some(_) => {}
                None => key = Some(own_key),
            }
            let mut held: Vec<String> = tx
                .prepare_cached(crate::corrections::sql::NAMESAKE_IDS.text())?
                .query_map(params![row.id], |found| found.get(0))?
                .collect::<rusqlite::Result<_>>()?;
            held.extend(row.link_mbid.clone());
            let mut own = BTreeSet::new();
            for mbid in held {
                let current = merged_ids::current_id(tx, &mbid)?;
                if !placeholder(&current) {
                    own.insert(current);
                }
            }
            members.push(Namesake { row, ids: own });
        }
        let Some(key) = key else {
            return Ok(SameName::Stays);
        };
        members.retain(|member| {
            let sits_out = member.ids.len() > 1;
            if sits_out {
                tracing::debug!(
                    artist_id = member.row.id,
                    "artist record holds two MusicBrainz ids; not joined by name"
                );
            }
            !sits_out
        });
        if members.len() < 2 {
            return Ok(SameName::Stays);
        }
        let held: BTreeSet<String> = members.iter().flat_map(|m| m.ids.iter().cloned()).collect();
        if held.len() > 1 {
            return Ok(SameName::Apart(held));
        }
        let member_ids: Vec<&str> = members.iter().map(|m| m.row.id.as_str()).collect();
        if marked_distinct(tx, &member_ids)? {
            tracing::debug!(
                artist_ids = ?member_ids,
                "artist records of one name marked distinct; not joined"
            );
            return Ok(SameName::Stays);
        }
        members.sort_by(|a, b| {
            let rank = |m: &Namesake| {
                (
                    !m.row.chosen,
                    m.row.link_mbid.is_none(),
                    m.ids.is_empty(),
                    std::cmp::Reverse(m.row.track_credits),
                    std::cmp::Reverse(m.row.album_credits),
                    m.row.created_at,
                )
            };
            rank(a).cmp(&rank(b)).then_with(|| a.row.id.cmp(&b.row.id))
        });
        let Some((survivor, retirees)) = members.split_first() else {
            return Ok(SameName::Stays);
        };
        if retirees.iter().any(|m| m.row.link_mbid.is_some()) {
            return Ok(SameName::Stays);
        }
        let sort_fill = if survivor.row.sort_name.is_none() {
            retirees
                .iter()
                .filter(|m| m.row.sort_name.is_some())
                .max_by_key(|m| {
                    (
                        m.row.track_credits + m.row.album_credits,
                        std::cmp::Reverse(m.row.created_at),
                    )
                })
                .and_then(|m| m.row.sort_name.clone())
        } else {
            None
        };
        let listed: BTreeMap<String, Vec<String>> = members
            .iter()
            .map(|m| (m.row.id.clone(), m.ids.iter().cloned().collect()))
            .collect();
        let bases = retirees
            .iter()
            .map(|retiree| {
                Self::new(
                    &survivor.row.id,
                    &retiree.row.id,
                    Kind::SameName {
                        key: key.clone(),
                        members: listed.clone().into_iter().collect(),
                    },
                )
            })
            .collect();
        Ok(SameName::Join { bases, sort_fill })
    }

    /// The fold of v2's placeholder row `retiree` into the built-in Unknown
    /// Artist: both live, `retiree` an ordinary row whose trimmed name is
    /// exactly that one (a scan keys the name exactly, so a folded match
    /// would move back at the first scan), and [`V2PlaceholderFacts::judge`]
    /// holds.
    pub(crate) fn v2_placeholder(
        tx: &'tx Transaction<'_>,
        retiree: &str,
    ) -> rusqlite::Result<Result<Self, V2PlaceholderKept>> {
        let live: i64 = tx
            .prepare_cached(crate::corrections::sql::LIVE_ARTISTS_OF_PAIR.text())?
            .query_row(params![UNKNOWN_ARTIST_ID, retiree], |row| row.get(0))?;
        let name = ordinary_name(tx, retiree)?;
        let Some(name) = name.filter(|name| live == 2 && name.trim() == UNKNOWN_ARTIST) else {
            return Ok(Err(V2PlaceholderKept::NotPlaceholder));
        };
        let facts = v2_placeholder_facts(tx, retiree)?;
        Ok(facts
            .judge()
            .map(|()| Self::new(UNKNOWN_ARTIST_ID, retiree, Kind::V2Placeholder { name })))
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
            .prepare_cached(crate::corrections::sql::LIVE_ARTISTS_OF_PAIR.text())?
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

fn v2_placeholder_facts(
    tx: &Transaction<'_>,
    retiree: &str,
) -> rusqlite::Result<V2PlaceholderFacts> {
    let ids = |statement: droppedneedle_kernel::db::statement::Statement| {
        tx.prepare_cached(statement.text())?
            .query_map(params![retiree], |row| row.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()
    };
    Ok(V2PlaceholderFacts {
        linked_ids: ids(crate::corrections::sql::LINKED_IDS)?,
        source_link: tx
            .prepare_cached(crate::corrections::sql::HAS_SOURCE_LINK.text())?
            .query_row(params![retiree], |row| row.get(0))?,
        named_ids: ids(crate::corrections::sql::NAMED_IDS)?,
        other_credit: tx
            .prepare_cached(crate::corrections::sql::CREDITED_UNDER_ANOTHER_NAME.text())?
            .query_row(params![retiree, UNKNOWN_ARTIST], |row| row.get(0))?,
        marked_distinct: marked_distinct(tx, &[retiree, UNKNOWN_ARTIST_ID])?,
    })
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
        .prepare_cached(PROOF_ROWS.text())?
        .query_map(params![mbid], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for row in by_proofs {
        if !unlinked.iter().any(|(id, _)| *id == row)
            && !members.iter().any(|member| member.id == row)
        {
            unlinked.push((row, true));
        }
    }
    let member_sql = UNLINKED_MEMBER.text();
    let mut dated: Vec<(f64, String, bool)> = Vec::new();
    for (row, proof_only) in unlinked {
        let created: Option<f64> = tx
            .prepare_cached(member_sql)?
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
        .prepare_cached(crate::corrections::sql::LINK_DECISION_SOURCE.text())?
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
        .prepare_cached(crate::corrections::sql::MBIDS_FROM_MERGE_ACTIONS.text())?
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
        .prepare_cached(FILE_IDS.text())?
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
        .prepare_cached(FILE_ID_COUNT.text())?
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
        .prepare_cached(RELEASE_PROOFS.text())?
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
        .prepare_cached(RELEASE_PROOF_COUNT.text())?
        .query_row(params![artist, mbid], |row| row.get(0))?;
    Ok((proofs, count as usize))
}

/// Whether every track and album credit of `artist` holds `mbid`, by its
/// file's id or by a current proof at that credit.
fn every_credit_holds(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    tx.prepare_cached(EVERY_CREDIT_HOLDS.text())?
        .query_row(params![artist, mbid], |row| row.get(0))
}

/// Whether `artist` has a credit, every credit holds `mbid`, and nothing
/// proves it another artist: what a survivor proven by a matched album
/// needs before it takes in other rows.
fn fully_proven(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    let credited: bool = tx
        .prepare_cached(crate::corrections::sql::FULLY_PROVEN.text())?
        .query_row(params![artist], |row| row.get(0))?;
    Ok(credited && every_credit_holds(tx, artist, mbid)? && !other_id(tx, artist, mbid)?)
}

/// Whether anything proves `artist` to be an artist other than `mbid`.
fn other_id(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    tx.prepare_cached(crate::corrections::sql::OTHER_ID.text())?
        .query_row(params![artist, mbid], |row| row.get(0))
}

/// Whether a file of one of `artist`'s credits holds `mbid` and nothing
/// proves it another artist: what lets a survivor the app linked take in
/// others while a credit of its own is not proven.
fn files_prove(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    let held: bool = tx
        .prepare_cached(crate::corrections::sql::FILES_PROVE.text())?
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
    tx.prepare_cached(HELD_BACK.text())?.query_row(
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
    tx.prepare_cached(crate::corrections::sql::LIVE_ROW.text())?
        .query_row(params![artist], |row| row.get(0))
        .optional()
}

/// Whether `artist` is proven to be an artist other than `mbid`: a link
/// to another id, a file holding another id, or a current proof naming
/// another.
fn contradicted(tx: &Transaction<'_>, artist: &str, mbid: &str) -> rusqlite::Result<bool> {
    let link: Option<String> = tx
        .prepare_cached(crate::corrections::sql::CONTRADICTED.text())?
        .query_row(params![artist], |row| row.get(0))
        .optional()?;
    Ok(link.is_some_and(|link| link != mbid) || other_id(tx, artist, mbid)?)
}

/// Whether an administrator marked any two of the rows as distinct
/// artists, at any revision, either row possibly merged since.
fn marked_distinct(tx: &Transaction<'_>, ids: &[&str]) -> rusqlite::Result<bool> {
    let ids = serde_json::to_string(ids).unwrap_or_default();
    tx.prepare_cached(crate::corrections::sql::MARKED_DISTINCT.text())?
        .query_row(params![ids], |row| row.get(0))
}

/// The display name of live row `artist` when it is an ordinary row:
/// neither a sentinel nor a Various Artists row.
fn ordinary_name(tx: &Transaction<'_>, artist: &str) -> rusqlite::Result<Option<String>> {
    tx.prepare_cached(crate::corrections::sql::ORDINARY_NAME.text())?
        .query_row(
            params![artist, SENTINEL_ARTIST_IDS[0], SENTINEL_ARTIST_IDS[1]],
            |row| row.get(0),
        )
        .optional()
}
