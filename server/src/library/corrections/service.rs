//! Entry points the HTTP handlers call. All of them block (SQLite on the
//! identify store's connection); handlers run them on `spawn_blocking`.
//!
//! A preview runs the very change in a transaction and rolls it back, so
//! what it shows is what an apply does. Its token covers the request, the
//! state read and the result; an apply runs the change again and commits
//! only when the token still matches.

use std::sync::Arc;

use rusqlite::{OptionalExtension as _, Transaction, TransactionBehavior, params};

use super::artists;
use super::membership::{self, Run};
use super::models::{
    Applied, ApplyMeta, ArtistMergeOutcome, ArtistMergeRequest, CorrectionError, MembershipKind,
    MembershipOutcome, MembershipRequest, Previewed,
};
use super::reset;
use super::token::{self, TokenFault};
use crate::library::clock::now_unix;
use crate::library::identify::sqlite::SqliteIdentifyStore;
use crate::library::wiring::LibrarySetup;
use crate::runtime_config::ConfigStore;

/// Catalog corrections over the library's store. Preview tokens are signed
/// with the server's data key, held by the settings store.
#[derive(Clone)]
pub struct Corrections {
    store: Arc<SqliteIdentifyStore>,
    signer: Arc<ConfigStore>,
}

impl Corrections {
    pub fn new(setup: &LibrarySetup) -> Self {
        Self {
            store: setup.identify_store.clone(),
            signer: setup.config.clone(),
        }
    }

    /// Run `op` in one write transaction; commit only when asked.
    fn in_tx<T>(
        &self,
        commit: bool,
        op: impl FnOnce(&Transaction<'_>) -> Result<T, CorrectionError>,
    ) -> Result<T, CorrectionError> {
        self.store.with_connection(|conn| {
            let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let value = op(&tx)?;
            if commit {
                tx.commit()?;
            }
            Ok(value)
        })
    }

    /// What a split, merge, move or reset would do, and its token.
    pub fn preview_membership(
        &self,
        request: &MembershipRequest,
        actor: &str,
    ) -> Result<Previewed<MembershipOutcome>, CorrectionError> {
        let now = now_unix();
        self.in_tx(false, |tx| {
            let run = run_membership(tx, request, actor, now)?;
            Ok(Previewed {
                token: token::issue(&self.signer, actor, &run.material, now as i64),
                outcome: run.outcome,
            })
        })
    }

    /// Apply a previewed split, merge, move or reset.
    pub fn apply_membership(
        &self,
        request: &MembershipRequest,
        meta: &ApplyMeta,
    ) -> Result<Applied, CorrectionError> {
        let now = now_unix();
        self.in_tx(true, |tx| {
            if let Some(done) = replay(tx, meta.idempotency_key.as_deref())? {
                return Ok(done);
            }
            let run = run_membership(tx, request, &meta.actor, now)?;
            self.check_token(meta, &run.material, now)?;
            let outcome = run.outcome;
            let mut applied = Applied {
                kind: request.kind.as_str().to_owned(),
                track_ids: outcome.track_ids.clone(),
                source_album_ids: outcome.source_album_ids.clone(),
                target_album_id: match request.kind {
                    MembershipKind::Reset => None,
                    _ => outcome.groups.first().map(|group| group.album_id.clone()),
                },
                ..Applied::default()
            };
            applied.catalog_revision = bump_catalog(tx)?;
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

    /// What an artist merge would do, and its token.
    pub fn preview_artist_merge(
        &self,
        request: &ArtistMergeRequest,
        actor: &str,
    ) -> Result<Previewed<ArtistMergeOutcome>, CorrectionError> {
        let now = now_unix();
        self.in_tx(false, |tx| {
            let run = artists::merge(tx, request, now)?;
            Ok(Previewed {
                token: token::issue(&self.signer, actor, &run.material, now as i64),
                outcome: run.outcome,
            })
        })
    }

    /// Apply a previewed artist merge.
    pub fn apply_artist_merge(
        &self,
        request: &ArtistMergeRequest,
        meta: &ApplyMeta,
    ) -> Result<Applied, CorrectionError> {
        let now = now_unix();
        self.in_tx(true, |tx| {
            if let Some(done) = replay(tx, meta.idempotency_key.as_deref())? {
                return Ok(done);
            }
            let run = artists::merge(tx, request, now)?;
            self.check_token(meta, &run.material, now)?;
            let mut applied = Applied {
                kind: "merge_artist".to_owned(),
                surviving_artist_id: Some(run.outcome.surviving_artist_id.clone()),
                retired_artist_ids: run.outcome.retired_artist_ids.clone(),
                ..Applied::default()
            };
            applied.catalog_revision = bump_catalog(tx)?;
            let mut all = vec![run.outcome.surviving_artist_id.clone()];
            all.extend(run.outcome.retired_artist_ids.iter().cloned());
            record(
                tx,
                &Audit {
                    kind: "merge_artist",
                    album_id: None,
                    artist_id: Some(&run.outcome.surviving_artist_id),
                    before: serde_json::json!({ "artist_ids": all }),
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

fn run_membership(
    tx: &Transaction<'_>,
    request: &MembershipRequest,
    actor: &str,
    now: f64,
) -> Result<Run, CorrectionError> {
    match request.kind {
        MembershipKind::Reset => reset::reset(tx, request, actor, now),
        _ => membership::regroup(tx, request, actor, now),
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

/// Move the catalog revision so cached reads refresh; returns the new one.
fn bump_catalog(tx: &Transaction<'_>) -> rusqlite::Result<i64> {
    tx.execute(
        "INSERT INTO library_catalog_revision (singleton, value) VALUES (1, 0) \
         ON CONFLICT (singleton) DO NOTHING",
        [],
    )?;
    tx.query_row(
        "UPDATE library_catalog_revision SET value = value + 1 WHERE singleton = 1 \
         RETURNING value",
        [],
        |row| row.get(0),
    )
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
