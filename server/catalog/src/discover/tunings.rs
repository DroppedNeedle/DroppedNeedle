//! How a person tuned what Discover offers them (`catalog_discover_tunings`):
//! one row a person, kind and target, listed beside what they told
//! Discover and undone there.
//!
//! One kind is taken today: "Less from this" (`less_from_seed`), about one
//! of their artists, which the pool's ranking weighs at a part of its
//! weight ([`rank::tuned_weight`](crate::discover::engine::rank::tuned_weight)).
//! The other kinds are declared with the table and refused until the work
//! that gives them meaning. Saving or undoing one queues the person's pool
//! build in the same write, through [`Due::queue_now`].

use std::collections::HashSet;
use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::queue::JobError;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::discover::due::Due;
use crate::discover::engine::{pool, sql};
use crate::discover::models::{DiscoverTuningEntry, DiscoverTuningRequest};
use crate::discover::read_all;

/// What a tuning is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TuningKind {
    /// Less from one of the person's artists: its weight is cut wherever
    /// the pool sums support.
    LessFromSeed,
    /// Less of one kind of pick. Not taken yet: later work gives it its
    /// meaning.
    LessOfKind,
    /// No picks from one source. Not taken yet: later work gives it its
    /// meaning.
    NotThisSource,
}

droppedneedle_kernel::spelled!(TuningKind {
    LessFromSeed => "less_from_seed",
    LessOfKind => "less_of_kind",
    NotThisSource => "not_this_source",
});

/// Why a tuning was refused.
#[derive(Debug)]
pub enum TuningError {
    /// The request does not say what it is about, or asks for a kind not
    /// taken yet.
    Invalid(String),
    /// The database failed.
    Store(JobError),
}

impl std::fmt::Display for TuningError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Invalid(reason) => f.write_str(reason),
            Self::Store(error) => error.fmt(f),
        }
    }
}

impl From<DbError> for TuningError {
    fn from(error: DbError) -> Self {
        Self::Store(JobError::from(error))
    }
}

impl From<JobError> for TuningError {
    fn from(error: JobError) -> Self {
        Self::Store(error)
    }
}

impl From<rusqlite::Error> for TuningError {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(JobError::from(error))
    }
}

/// `kind` and `target` checked: a kind taken today, and a MusicBrainz id,
/// trimmed and lowercased.
fn checked(kind: TuningKind, target: &str) -> Result<String, TuningError> {
    if kind != TuningKind::LessFromSeed {
        return Err(TuningError::Invalid(format!(
            "{} is not available yet",
            kind.as_str()
        )));
    }
    let target = target.trim();
    if !droppedneedle_providers::musicbrainz::is_valid_mbid(target) {
        return Err(TuningError::Invalid(
            "target must be a MusicBrainz id".to_owned(),
        ));
    }
    Ok(target.to_ascii_lowercase())
}

/// The parsed kind and target of an undo, from its path.
///
/// # Errors
///
/// A kind that is not one, or not taken yet, or a target that is not a
/// MusicBrainz id.
pub fn checked_path(kind: &str, target: &str) -> Result<(TuningKind, String), TuningError> {
    let kind = TuningKind::parse(kind.trim())
        .ok_or_else(|| TuningError::Invalid("kind is not a tuning".to_owned()))?;
    Ok((kind, checked(kind, target)?))
}

/// The targets of `user_id`'s "Less from this", lowercased.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn less_from(db: &Db, user_id: &str) -> Result<HashSet<String>, DbError> {
    let rows = read_all(
        db,
        "discover.tunings.less_from",
        sql::TUNING_TARGETS.text(),
        (user_id.to_owned(), TuningKind::LessFromSeed),
        |row| row.get::<_, String>(0),
    )
    .await?;
    Ok(rows.into_iter().collect())
}

/// Each person's tunings: kept, listed and undone, each change queuing
/// their pool build.
#[derive(Clone)]
pub struct Tunings {
    db: Db,
    clock: Arc<dyn Clock>,
    due: Arc<Due>,
}

impl Tunings {
    /// Tunings in `db`, dated by `clock`, queuing pool builds through `due`.
    pub fn new(db: Db, clock: Arc<dyn Clock>, due: Arc<Due>) -> Self {
        Self { db, clock, due }
    }

    /// Keep one tuning of `user_id`'s and queue their pool build in the
    /// same write. The target is one of their own artists, named as their
    /// taste rows name it. Answers it as stored.
    ///
    /// # Errors
    ///
    /// A kind not taken yet, a target that is not an id or not one of
    /// their artists, or the write's failure.
    pub async fn save(
        &self,
        user_id: &str,
        request: &DiscoverTuningRequest,
    ) -> Result<DiscoverTuningEntry, TuningError> {
        let target = checked(request.kind, &request.target)?;
        let now_ms = self.clock.now_ms();
        let (kind, user, due) = (request.kind, user_id.to_owned(), Arc::clone(&self.due));
        self.db
            .write(Lane::Foreground, "discover.tunings.save", move |tx| {
                let name: Option<String> = tx
                    .prepare_cached(sql::TUNING_TARGET_NAME.text())?
                    .query_map(params![user, target], |row| row.get(0))?
                    .next()
                    .transpose()?;
                let Some(name) = name else {
                    return Err(TuningError::Invalid(
                        "target must be one of your artists".to_owned(),
                    ));
                };
                tx.execute(
                    sql::TUNING_SAVE.text(),
                    params![user, kind, target, name, Millis(now_ms)],
                )?;
                due.queue_now(tx, pool::KIND, &user)?;
                Ok(DiscoverTuningEntry {
                    kind,
                    target,
                    name,
                    created_at: now_ms / 1000,
                })
            })
            .await
    }

    /// Take back one tuning of `user_id`'s, queuing their pool build in
    /// the same write when there was one.
    ///
    /// # Errors
    ///
    /// The write's failure.
    pub async fn undo(
        &self,
        user_id: &str,
        kind: TuningKind,
        target: &str,
    ) -> Result<(), TuningError> {
        let (user, target, due) = (user_id.to_owned(), target.to_owned(), Arc::clone(&self.due));
        self.db
            .write(Lane::Foreground, "discover.tunings.undo", move |tx| {
                let removed = tx.execute(sql::TUNING_UNDO.text(), params![user, kind, target])?;
                if removed > 0 {
                    due.queue_now(tx, pool::KIND, &user)?;
                }
                Ok::<_, JobError>(())
            })
            .await
            .map_err(TuningError::Store)
    }

    /// `user_id`'s tunings, newest first.
    ///
    /// # Errors
    ///
    /// The read's failure.
    pub async fn list(&self, user_id: &str) -> Result<Vec<DiscoverTuningEntry>, DbError> {
        let rows = read_all(
            &self.db,
            "discover.tunings.list",
            sql::TUNING_LIST.text(),
            [user_id.to_owned()],
            sql::TuningRow::read,
        )
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| DiscoverTuningEntry {
                kind: row.kind,
                target: row.target,
                name: row.target_name,
                created_at: row.created_at.0 / 1000,
            })
            .collect())
    }
}
