//! How a person tuned what Discover offers them (`catalog_discover_tunings`):
//! one row a person, kind and target, listed beside what they told
//! Discover and undone there or on their taste page.
//!
//! Three kinds are taken today, each about one of their artists, and an
//! artist holds at most one of them:
//!
//! - "Less from this" (`less_from_seed`) and "More from this"
//!   (`more_from_seed`): the pool's ranking weighs the seed at a part, or a
//!   multiple, of its weight
//!   ([`rank::tuned_weight`](crate::discover::engine::rank::tuned_weight)).
//!   Saving or undoing one queues the person's pool build.
//! - "That was a phase" (`phase`): the artist's plays and imported listens
//!   no longer speak for it when its seeds are weighed
//!   ([`weigh`](crate::discover::queue::seeds::weigh)); what the person
//!   chose (hearts, follows and the rest) still does. Saving or undoing one
//!   queues the person's taste build, whose completion queues the pool.
//!
//! A statement about a whole taste group is one row for each artist the
//! group holds as it stands, each keyed on the group's anchor artist and
//! carrying its name, so it outlives the groups being worked out again; an artist already holding a
//! statement of its own keeps it. The other kinds are declared with the
//! table and refused until the work that gives them meaning. Every queue
//! is written in the same write, through [`Due::queue_now`].

use std::collections::HashSet;
use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{Db, DbError, writer::Lane};
use droppedneedle_kernel::jobs::queue::{JobError, JobKind};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::discover::due::Due;
use crate::discover::engine::{jobs, pool, sql};
use crate::discover::models::{DiscoverTuningEntry, DiscoverTuningRequest};
use crate::discover::read_all;

/// What a tuning is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TuningKind {
    /// Less from one of the person's artists: its weight is cut wherever
    /// the pool sums support.
    LessFromSeed,
    /// More from one of the person's artists: its weight is raised
    /// wherever the pool sums support.
    MoreFromSeed,
    /// "That was a phase": the artist's plays and imported listens no
    /// longer count toward its weight.
    Phase,
    /// Less of one kind of pick. Not taken yet: later work gives it its
    /// meaning.
    LessOfKind,
    /// No picks from one source. Not taken yet: later work gives it its
    /// meaning.
    NotThisSource,
}

droppedneedle_kernel::spelled!(TuningKind {
    LessFromSeed => "less_from_seed",
    MoreFromSeed => "more_from_seed",
    Phase => "phase",
    LessOfKind => "less_of_kind",
    NotThisSource => "not_this_source",
});

impl TuningKind {
    /// Whether it is one of the statements about one of the person's
    /// artists, of which an artist holds one at a time.
    fn about_artist(self) -> bool {
        matches!(self, Self::LessFromSeed | Self::MoreFromSeed | Self::Phase)
    }

    /// The build a change to it makes due: the taste for a phase, else the
    /// pool.
    fn rebuilds(self) -> JobKind {
        if self == Self::Phase {
            jobs::KIND
        } else {
            pool::KIND
        }
    }
}

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

/// `kind` checked: one taken today.
fn taken(kind: TuningKind) -> Result<(), TuningError> {
    if kind.about_artist() {
        Ok(())
    } else {
        Err(TuningError::Invalid(format!(
            "{} is not available yet",
            kind.as_str()
        )))
    }
}

/// `kind` and `target` checked: a kind taken today, and for each of them
/// a MusicBrainz id, trimmed and lowercased.
fn checked(kind: TuningKind, target: &str) -> Result<String, TuningError> {
    taken(kind)?;
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

/// The targets of `user_id`'s tunings of `kind`, lowercased.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn of_kind(
    db: &Db,
    user_id: &str,
    kind: TuningKind,
) -> Result<HashSet<String>, DbError> {
    let rows = read_all(
        db,
        "discover.tunings.of_kind",
        sql::TUNING_TARGETS.text(),
        (user_id.to_owned(), kind),
        |row| row.get::<_, String>(0),
    )
    .await?;
    Ok(rows.into_iter().collect())
}

/// The targets of `user_id`'s "Less from this", lowercased.
///
/// # Errors
///
/// The read's failure.
pub(crate) async fn less_from(db: &Db, user_id: &str) -> Result<HashSet<String>, DbError> {
    of_kind(db, user_id, TuningKind::LessFromSeed).await
}

/// What a person said about one target, of any kind.
fn said_about(
    tx: &Transaction,
    user: &str,
    target: &str,
) -> rusqlite::Result<Vec<sql::TuningOfTarget>> {
    tx.prepare_cached(sql::TUNING_OF_TARGET.text())?
        .query_map(params![user, target], sql::TuningOfTarget::read)?
        .collect()
}

/// Each person's tunings: kept, listed and undone, each change queuing
/// the build it makes due.
#[derive(Clone)]
pub struct Tunings {
    db: Db,
    clock: Arc<dyn Clock>,
    due: Arc<Due>,
}

impl Tunings {
    /// Tunings in `db`, dated by `clock`, queuing builds through `due`.
    pub fn new(db: Db, clock: Arc<dyn Clock>, due: Arc<Due>) -> Self {
        Self { db, clock, due }
    }

    /// Keep one tuning of `user_id`'s, about one of their artists or every
    /// artist of one of their taste groups, and queue the build it makes
    /// due in the same write. Answers the rows written.
    ///
    /// # Errors
    ///
    /// A kind not taken yet, a request naming neither or both of an artist
    /// and a group, a target that is not an id or not one of their artists,
    /// a group they do not have, or the write's failure.
    pub async fn save(
        &self,
        user_id: &str,
        request: &DiscoverTuningRequest,
    ) -> Result<Vec<DiscoverTuningEntry>, TuningError> {
        let kind = request.kind;
        match (request.target.as_deref(), request.group_no) {
            (Some(target), None) => {
                let target = checked(kind, target)?;
                self.save_one(user_id, kind, target)
                    .await
                    .map(|row| vec![row])
            }
            (None, Some(group_no)) => {
                taken(kind)?;
                self.save_group(user_id, kind, group_no).await
            }
            _ => Err(TuningError::Invalid(
                "name either a target or a group_no".to_owned(),
            )),
        }
    }

    /// One statement about one artist, in place of any other the artist
    /// held.
    async fn save_one(
        &self,
        user_id: &str,
        kind: TuningKind,
        target: String,
    ) -> Result<DiscoverTuningEntry, TuningError> {
        let now_ms = self.clock.now_ms();
        let (user, due) = (user_id.to_owned(), Arc::clone(&self.due));
        self.db
            .write(Lane::Foreground, "discover.tunings.save", move |tx| {
                let name: Option<String> = tx
                    .prepare_cached(sql::TUNING_TARGET_NAME.text())?
                    .query_row(params![user, target], |row| row.get(0))
                    .optional()?;
                let Some(name) = name else {
                    return Err(TuningError::Invalid(
                        "target must be one of your artists".to_owned(),
                    ));
                };
                let mut rebuild = kind.rebuilds();
                for held in said_about(tx, &user, &target)? {
                    if held.kind != kind && held.kind.about_artist() {
                        tx.execute(sql::TUNING_UNDO.text(), params![user, held.kind, target])?;
                        if held.kind.rebuilds() == jobs::KIND {
                            rebuild = jobs::KIND;
                        }
                    }
                }
                tx.execute(
                    sql::TUNING_SAVE.text(),
                    params![
                        user,
                        kind,
                        target,
                        name,
                        Millis(now_ms),
                        None::<String>,
                        None::<String>
                    ],
                )?;
                due.queue_now(tx, rebuild, &user)?;
                Ok(DiscoverTuningEntry {
                    kind,
                    target,
                    name,
                    group_anchor_mbid: None,
                    group_label: None,
                    created_at: now_ms / 1000,
                })
            })
            .await
    }

    /// One statement about every artist of the group `group_no` as it
    /// stands, each row keyed on the group's anchor artist and carrying its
    /// name. An artist holding a statement of its own, or one said for
    /// another group, keeps it; one this group's earlier statement covers
    /// takes the new one instead. Nothing is written, and nothing queued,
    /// when every artist keeps its own.
    async fn save_group(
        &self,
        user_id: &str,
        kind: TuningKind,
        group_no: i64,
    ) -> Result<Vec<DiscoverTuningEntry>, TuningError> {
        let now_ms = self.clock.now_ms();
        let (user, due) = (user_id.to_owned(), Arc::clone(&self.due));
        self.db
            .write(Lane::Foreground, "discover.tunings.save_group", move |tx| {
                let anchor = tx
                    .prepare_cached(sql::TUNING_GROUP_ANCHOR.text())?
                    .query_row(params![user, group_no], sql::TuningGroupAnchor::read)
                    .optional()?;
                let Some(anchor) = anchor else {
                    return Err(TuningError::Invalid(
                        "group_no must be one of your groups".to_owned(),
                    ));
                };
                let artists = tx
                    .prepare_cached(sql::TUNING_GROUP_ARTISTS.text())?
                    .query_map(params![user, group_no], sql::TuningGroupArtist::read)?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut rebuild = kind.rebuilds();
                let mut written = Vec::new();
                for artist in artists {
                    let target = artist.artist_mbid_lower;
                    let held: Vec<_> = said_about(tx, &user, &target)?
                        .into_iter()
                        .filter(|held| held.kind.about_artist())
                        .collect();
                    if held
                        .iter()
                        .any(|held| held.group_anchor_mbid.as_deref() != Some(anchor.mbid.as_str()))
                    {
                        continue;
                    }
                    for held in held.iter().filter(|held| held.kind != kind) {
                        tx.execute(sql::TUNING_UNDO.text(), params![user, held.kind, target])?;
                        if held.kind.rebuilds() == jobs::KIND {
                            rebuild = jobs::KIND;
                        }
                    }
                    tx.execute(
                        sql::TUNING_SAVE.text(),
                        params![
                            user,
                            kind,
                            target,
                            artist.name,
                            Millis(now_ms),
                            anchor.mbid,
                            anchor.name
                        ],
                    )?;
                    written.push(DiscoverTuningEntry {
                        kind,
                        target,
                        name: artist.name,
                        group_anchor_mbid: Some(anchor.mbid.clone()),
                        group_label: Some(anchor.name.clone()),
                        created_at: now_ms / 1000,
                    });
                }
                if !written.is_empty() {
                    due.queue_now(tx, rebuild, &user)?;
                }
                Ok(written)
            })
            .await
    }

    /// Take back one tuning of `user_id`'s, or with `group` every row of
    /// that kind said for the same group (anchor) as it, queuing the build it makes
    /// due in the same write when there was one.
    ///
    /// # Errors
    ///
    /// The write's failure.
    pub async fn undo(
        &self,
        user_id: &str,
        kind: TuningKind,
        target: &str,
        group: bool,
    ) -> Result<(), TuningError> {
        let (user, target, due) = (user_id.to_owned(), target.to_owned(), Arc::clone(&self.due));
        self.db
            .write(Lane::Foreground, "discover.tunings.undo", move |tx| {
                let anchor: Option<String> = if group {
                    tx.prepare_cached(sql::TUNING_ANCHOR.text())?
                        .query_row(params![user, kind, target], |row| row.get(0))
                        .optional()?
                        .flatten()
                } else {
                    None
                };
                let removed = match anchor {
                    Some(anchor) => {
                        tx.execute(sql::TUNING_UNDO_GROUP.text(), params![user, kind, anchor])?
                    }
                    None => tx.execute(sql::TUNING_UNDO.text(), params![user, kind, target])?,
                };
                if removed > 0 {
                    due.queue_now(tx, kind.rebuilds(), &user)?;
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
                group_anchor_mbid: row.group_anchor_mbid,
                group_label: row.group_label,
                created_at: row.created_at.0 / 1000,
            })
            .collect())
    }
}
