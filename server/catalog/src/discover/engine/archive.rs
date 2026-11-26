//! Past weeks: each person's weekly sets read back as they were shown,
//! newest first, with what became of every pick, and the albums they kept
//! from them.
//!
//! Reads only. A set and its reasons are a record from the day they were
//! built ([`weekly_store`](super::weekly_store) is their one writer); a
//! pick's state is worked out from the request, library, Later and
//! feedback rows on each read, by the rule the week itself reads, and its
//! arrival from the pick record.

use std::sync::Arc;

use droppedneedle_kernel::clock::Clock;
use droppedneedle_kernel::db::time::Millis;
use droppedneedle_kernel::db::{Db, DbError};
use droppedneedle_kernel::page::{Page, PageRequest};
use rusqlite::params;

use super::sql;
use super::weekly::{album_row, week_number};
use crate::discover::feedback::FeedbackTarget;
use crate::discover::models::{
    AnswerCounts, FoundOnDiscover, FoundPick, PastPick, PastWeek, PastWeeksPage, PickAnswer,
    PickEvidence, PickReason, PickReasonKind, WeeksRecord,
};

/// Weeks a page holds unless asked for fewer.
pub const PAGE_DEFAULT: usize = 8;
/// The most weeks a page holds.
pub const PAGE_MAX: usize = 20;
/// How long after its arrival a kept pick can be "still played".
const MONTH_MS: i64 = 30 * 86_400_000;

/// What narrows the picks of past weeks.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WeeksFilter {
    /// Only picks with this answer.
    pub answer: Option<PickAnswer>,
    /// Only picks whose title or artist holds this, lowercased.
    pub text: Option<String>,
}

/// The JSON list of the states counted as `answer`.
fn states_json(answer: PickAnswer) -> String {
    let states: Vec<&str> = answer
        .states()
        .into_iter()
        .map(|state| state.as_str())
        .collect();
    serde_json::to_string(&states).unwrap_or_else(|_| "[]".to_owned())
}

/// A pick's reason as it was shown: from its stored evidence, else its
/// stored line, which a set holds only for picks through the person's
/// artists.
fn shown_reason(evidence: &str, line: &str) -> PickReason {
    match serde_json::from_str::<PickEvidence>(evidence) {
        Ok(evidence) => evidence.reason,
        Err(error) => {
            tracing::warn!(%error, "a kept pick's evidence does not read; its line stands");
            PickReason {
                kind: PickReasonKind::YourArtists,
                line: line.to_owned(),
                artists: Vec::new(),
            }
        }
    }
}

/// Each person's past weeks and what they kept from them.
#[derive(Clone)]
pub struct PastWeeks {
    db: Db,
    clock: Arc<dyn Clock>,
}

impl PastWeeks {
    /// The past weeks over `db`, their states read at `clock`'s time.
    pub fn new(db: Db, clock: Arc<dyn Clock>) -> Self {
        Self { db, clock }
    }

    /// One page of `user_id`'s sets, newest first, each with its picks
    /// that match `filter`, in order; a set none of whose picks match is
    /// left out. The first page also carries the record of every set.
    ///
    /// # Errors
    ///
    /// A read's failure.
    pub async fn page(
        &self,
        user_id: &str,
        paging: PageRequest<i64>,
        filter: WeeksFilter,
    ) -> Result<PastWeeksPage, DbError> {
        let now_ms = self.clock.now_ms();
        // The record is a read of its own: it counts every set, and a
        // page read a moment apart need not agree with it.
        let record = match paging.after {
            None => Some(self.record(user_id, now_ms).await?),
            Some(_) => None,
        };
        let before = paging.after.unwrap_or(i64::MAX);
        let limit = paging.limit;
        let user = user_id.to_owned();
        let states = filter.answer.map(states_json);
        let rows = self
            .db
            .read("discover.weeks.page", move |conn| {
                conn.prepare_cached(sql::PAST_WEEKS_PAGE.text())?
                    .query_map(
                        params![
                            user,
                            Millis(before),
                            Millis(now_ms),
                            FeedbackTarget::Album,
                            FeedbackTarget::Artist,
                            i64::try_from(limit + 1).unwrap_or(i64::MAX),
                            states,
                            filter.text,
                        ],
                        sql::PastWeekPickRow::read,
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(DbError::from)
            })
            .await?;
        let mut weeks: Vec<PastWeek> = Vec::new();
        for row in rows {
            let week_start_at = row.week_start_at.0;
            if weeks
                .last()
                .is_none_or(|week| week.week_start_at != week_start_at)
            {
                weeks.push(PastWeek {
                    week_start_at,
                    week_number: week_number(week_start_at),
                    tally: AnswerCounts::default(),
                    picks: Vec::new(),
                });
            }
            let Some(week) = weeks.last_mut() else {
                continue;
            };
            let answer = PickAnswer::of(row.state);
            week.tally.add(answer, 1);
            if !row.matches {
                continue;
            }
            let kept = answer == PickAnswer::Kept;
            week.picks.push(PastPick {
                pick_no: row.pick_no,
                album: album_row(
                    &row.release_group_mbid,
                    &row.title,
                    &row.artist_name,
                    &row.artist_mbid,
                    row.primary_type.as_deref(),
                    row.first_release_date.as_deref(),
                ),
                reason: shown_reason(&row.evidence, &row.reason_line),
                state: row.state,
                arrived_at: row.arrived_at.filter(|_| kept).map(|at| at.0),
                plays: row.plays.filter(|plays| kept && *plays > 0),
            });
        }
        Ok(PastWeeksPage {
            page: Page::cut(weeks, limit, |week| week.week_start_at),
            record,
        })
    }

    /// What every set of `user_id`'s came to at `now_ms`.
    async fn record(&self, user_id: &str, now_ms: i64) -> Result<WeeksRecord, DbError> {
        let user = user_id.to_owned();
        let rows = self
            .db
            .read("discover.weeks.record", move |conn| {
                conn.prepare_cached(sql::PAST_WEEKS_RECORD.text())?
                    .query_map(
                        params![
                            user,
                            Millis(i64::MAX),
                            Millis(now_ms),
                            FeedbackTarget::Album,
                            FeedbackTarget::Artist,
                        ],
                        sql::PastWeeksTally::read,
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(DbError::from)
            })
            .await?;
        let mut answers = AnswerCounts::default();
        let mut first_arrival: Option<i64> = None;
        for row in &rows {
            let answer = PickAnswer::of(row.state);
            answers.add(answer, row.picks);
            if answer == PickAnswer::Kept
                && let Some(at) = row.arrived_at
            {
                first_arrival = Some(first_arrival.map_or(at.0, |first| first.min(at.0)));
            }
        }
        Ok(WeeksRecord {
            picked: rows.iter().map(|row| row.picks).sum(),
            answers,
            counted_from_at: first_arrival.map(|at| at + MONTH_MS),
        })
    }

    /// The albums `user_id` kept from their sets, each with the first week
    /// it was picked, newest week first.
    ///
    /// # Errors
    ///
    /// The read's failure.
    pub async fn found(&self, user_id: &str) -> Result<FoundOnDiscover, DbError> {
        let now_ms = self.clock.now_ms();
        let user = user_id.to_owned();
        let kept = states_json(PickAnswer::Kept);
        let rows = self
            .db
            .read("discover.weeks.found", move |conn| {
                conn.prepare_cached(sql::PAST_WEEKS_FOUND.text())?
                    .query_map(
                        params![
                            user,
                            Millis(i64::MAX),
                            Millis(now_ms),
                            FeedbackTarget::Album,
                            FeedbackTarget::Artist,
                            kept,
                        ],
                        sql::FoundPickRow::read,
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()
                    .map_err(DbError::from)
            })
            .await?;
        Ok(FoundOnDiscover {
            items: rows
                .into_iter()
                .map(|row| FoundPick {
                    release_group_mbid: row.release_group_mbid,
                    week_start_at: row.week_start_at.0,
                    week_number: week_number(row.week_start_at.0),
                })
                .collect(),
        })
    }
}
