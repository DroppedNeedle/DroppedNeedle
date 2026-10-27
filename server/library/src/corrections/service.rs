//! Entry points the HTTP handlers call. All of them block on the shared
//! database; handlers run them on `spawn_blocking`.
//!
//! A preview runs the very change in a write and rolls it back, so what it
//! shows is what an apply does. Its token covers the request, the state
//! read and the result; an apply runs the change again and commits only
//! when the token still matches.

use rusqlite::{OptionalExtension as _, Transaction, params};

use super::artists;
use super::membership::{self, Run};
use super::models::{
    Applied, ApplyMeta, ArtistMergeOutcome, ArtistMergeRequest, CorrectionError, CorrectionKind,
    MembershipKind, MembershipOutcome, MembershipRequest, PairJoinRequest, Previewed,
    RetagPairList,
};
use super::reset;
use super::retag_pairs;
use super::token::{self, TokenFault};
use crate::clock::LibraryClock as _;
use crate::settings::LibrarySettings;
use droppedneedle_auth::Admin;
use droppedneedle_kernel::db::{Db, Lane, savepoint};

/// Catalog corrections over the library's store. Preview tokens are signed
/// with the server's data key, held by the settings store.
#[derive(Clone)]
pub struct Corrections {
    db: Db,
    signer: LibrarySettings,
    clock: std::sync::Arc<dyn droppedneedle_kernel::clock::Clock>,
}

impl Corrections {
    pub fn new(
        db: Db,
        signer: LibrarySettings,
        clock: std::sync::Arc<dyn droppedneedle_kernel::clock::Clock>,
    ) -> Self {
        Self { db, signer, clock }
    }

    /// What a split, merge, move or reset would do, and its token.
    pub fn preview_membership(
        &self,
        request: &MembershipRequest,
        actor: &str,
    ) -> Result<Previewed<MembershipOutcome>, CorrectionError> {
        let now = self.clock.now_secs();
        self.db
            .write_blocking(Lane::Foreground, "corrections.preview_membership", |tx| {
                rolled_back(tx, || {
                    let run = run_membership(tx, request, now)?;
                    Ok(Previewed {
                        token: token::issue(&self.signer, actor, &run.material, now as i64),
                        outcome: run.outcome,
                    })
                })
            })
    }

    /// Apply a previewed split, merge, move or reset.
    pub fn apply_membership(
        &self,
        request: &MembershipRequest,
        meta: &ApplyMeta,
    ) -> Result<Applied, CorrectionError> {
        let now = self.clock.now_secs();
        self.db
            .write_blocking(Lane::Foreground, "corrections.apply_membership", |tx| {
                if let Some(done) = replay(tx, meta.idempotency_key.as_deref())? {
                    return Ok(done);
                }
                let run = run_membership(tx, request, now)?;
                self.check_token(meta, &run.material, now)?;
                let outcome = run.outcome;
                let mut applied = Applied {
                    track_ids: outcome.track_ids.clone(),
                    source_album_ids: outcome.source_album_ids.clone(),
                    target_album_id: match request.kind {
                        MembershipKind::Reset => None,
                        _ => outcome.groups.first().map(|group| group.album_id.clone()),
                    },
                    ..Applied::new(request.kind.into())
                };
                applied.catalog_revision = crate::catalog_revision::bump_catalog(tx)?;
                let album = applied
                    .target_album_id
                    .clone()
                    .or_else(|| outcome.source_album_ids.first().cloned())
                    .unwrap_or_default();
                record(
                    tx,
                    &Audit {
                        kind: request.kind.as_str(),
                        album_id: Some(&album),
                        artist_id: None,
                        before: serde_json::json!({
                            "source_album_ids": outcome.source_album_ids,
                            "track_ids": outcome.track_ids,
                            "dropped_editions": dropped_editions(&outcome),
                        }),
                        reason: match request.kind {
                            MembershipKind::Reset => "AUTOMATIC_GROUPING_RESET",
                            _ => "MANUAL_GROUPING",
                        },
                        meta,
                        now,
                    },
                    &applied,
                )?;
                Ok(applied)
            })
    }

    /// What an administrator's artist merge would do, and its token.
    pub fn preview_artist_merge(
        &self,
        request: &ArtistMergeRequest,
        admin: &Admin,
    ) -> Result<Previewed<ArtistMergeOutcome>, CorrectionError> {
        let now = self.clock.now_secs();
        self.db
            .write_blocking(Lane::Foreground, "corrections.preview_artist_merge", |tx| {
                rolled_back(tx, || {
                    let run = artists::merge(tx, admin, request, now)?;
                    Ok(Previewed {
                        token: token::issue(
                            &self.signer,
                            admin.user_id(),
                            &run.material,
                            now as i64,
                        ),
                        outcome: run.outcome,
                    })
                })
            })
    }

    /// Apply an administrator's previewed artist merge.
    pub fn apply_artist_merge(
        &self,
        request: &ArtistMergeRequest,
        meta: &ApplyMeta,
        admin: &Admin,
    ) -> Result<Applied, CorrectionError> {
        let now = self.clock.now_secs();
        self.db
            .write_blocking(Lane::Foreground, "corrections.apply_artist_merge", |tx| {
                if let Some(done) = replay(tx, meta.idempotency_key.as_deref())? {
                    return Ok(done);
                }
                let run = artists::merge(tx, admin, request, now)?;
                self.check_token(meta, &run.material, now)?;
                let mut applied = Applied {
                    surviving_artist_id: Some(run.outcome.surviving_artist_id.clone()),
                    retired_artist_ids: run.outcome.retired_artist_ids.clone(),
                    ..Applied::new(CorrectionKind::MergeArtist)
                };
                applied.catalog_revision = crate::catalog_revision::bump_catalog(tx)?;
                let mut all = vec![run.outcome.surviving_artist_id.clone()];
                all.extend(run.outcome.retired_artist_ids.iter().cloned());
                record(
                    tx,
                    &Audit {
                        kind: "merge_artist",
                        album_id: None,
                        artist_id: Some(&run.outcome.surviving_artist_id),
                        before: serde_json::json!({ "artist_ids": all, "basis": run.basis }),
                        reason: "MANUAL_ARTIST_MERGE",
                        meta,
                        now,
                    },
                    &applied,
                )?;
                Ok(applied)
            })
    }
}

/// Retag pairs a person joins or dismisses.
impl Corrections {
    /// The open pairs a person decides on, newest first.
    pub fn retag_pairs(&self) -> Result<RetagPairList, CorrectionError> {
        self.db
            .read_blocking("corrections.retag_pairs", retag_pairs::list)
    }

    /// Join a pair; returns the album that stays.
    pub fn join_retag_pair(&self, request: &PairJoinRequest) -> Result<String, CorrectionError> {
        let now = self.clock.now_secs();
        self.db
            .write_blocking(Lane::Foreground, "corrections.join_retag_pair", |tx| {
                retag_pairs::join(tx, request, now)
            })
    }

    /// Mark a pair as two albums. False when it was already.
    pub fn dismiss_retag_pair(&self, old: &str, new: &str) -> Result<bool, CorrectionError> {
        let now_ms = self.clock.now_ms();
        self.db
            .write_blocking(Lane::Foreground, "corrections.dismiss_retag_pair", |tx| {
                retag_pairs::dismiss(tx, old, new, now_ms)
            })
    }
}

/// Run `op` inside a savepoint that is always rolled back, so a preview
/// sees the change it describes and nothing of it is kept.
fn rolled_back<T>(
    tx: &Transaction<'_>,
    op: impl FnOnce() -> Result<T, CorrectionError>,
) -> Result<T, CorrectionError> {
    match savepoint(tx, "correction_preview", || Err::<(), _>(op()))? {
        Err(outcome) => outcome,
        Ok(()) => Err(CorrectionError::Store("a preview kept its work".to_owned())),
    }
}

fn run_membership(
    tx: &Transaction<'_>,
    request: &MembershipRequest,
    now: f64,
) -> Result<Run, CorrectionError> {
    match request.kind {
        MembershipKind::Reset => reset::reset(tx, request, now),
        _ => membership::regroup(tx, request, now),
    }
}

impl Corrections {
    fn check_token(
        &self,
        meta: &ApplyMeta,
        material: &str,
        now: f64,
    ) -> Result<(), CorrectionError> {
        token::verify(
            &self.signer,
            &meta.actor,
            &meta.preview_token,
            material,
            now as i64,
        )
        .map_err(|fault| match fault {
            TokenFault::Invalid => CorrectionError::Invalid(fault.reason()),
            TokenFault::Expired | TokenFault::Stale => CorrectionError::Conflict(fault.reason()),
        })
    }
}

/// Editions the change dropped, kept in the audit row so a dropped pin can
/// be put back by hand.
fn dropped_editions(outcome: &MembershipOutcome) -> serde_json::Value {
    serde_json::Value::Array(
        outcome
            .edition_changes
            .iter()
            .flat_map(|change| {
                change.dropped.iter().map(move |dropped| {
                    serde_json::json!({
                        "local_album_id": change.album_id,
                        "release_group_mbid": dropped.release_group_mbid,
                        "release_mbid": dropped.release_mbid,
                        "decision_source": dropped.decision_source,
                        "from_album_id": dropped.album_id,
                    })
                })
            })
            .collect(),
    )
}

/// The result recorded under an idempotency key already used.
fn replay(tx: &Transaction<'_>, key: Option<&str>) -> Result<Option<Applied>, CorrectionError> {
    let Some(key) = key else {
        return Ok(None);
    };
    let stored: Option<String> = tx
        .query_row(
            "SELECT after_json FROM library_catalog_actions WHERE idempotency_key = ?1",
            params![key],
            |row| row.get(0),
        )
        .optional()?;
    match stored {
        Some(json) => serde_json::from_str(&json).map(Some).map_err(|error| {
            CorrectionError::Store(format!("stored correction unreadable: {error}"))
        }),
        None => Ok(None),
    }
}

struct Audit<'a> {
    kind: &'a str,
    album_id: Option<&'a str>,
    artist_id: Option<&'a str>,
    before: serde_json::Value,
    reason: &'a str,
    meta: &'a ApplyMeta,
    now: f64,
}

/// One audit row in `library_catalog_actions`, keyed for replay.
fn record(
    tx: &Transaction<'_>,
    audit: &Audit<'_>,
    applied: &Applied,
) -> Result<(), CorrectionError> {
    let after = serde_json::to_string(applied).map_err(|error| {
        CorrectionError::Store(format!("correction result unwritable: {error}"))
    })?;
    tx.execute(
        "INSERT INTO library_catalog_actions (id, idempotency_key, actor_user_id, action_kind, \
         local_artist_id, local_album_id, before_json, after_json, reason_code, created_at) \
         VALUES (?1, ?2, (SELECT id FROM auth_users WHERE id = ?3), ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            uuid::Uuid::new_v4().to_string(),
            audit.meta.idempotency_key,
            audit.meta.actor,
            audit.kind,
            audit.artist_id,
            audit.album_id,
            audit.before.to_string(),
            after,
            audit.reason,
            audit.now,
        ],
    )?;
    Ok(())
}
