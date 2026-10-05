//! Requests-to-flows ledger mirror.
//!
//! Requests owns the user-facing ledger; the flows loops read
//! their own memory ledger. The mirror runs ahead of every wanted and
//! sync pass (through the loops' pre-pass hook) and projects requests
//! rows and watches into the flows stores so the loops act on real asks.
//!
//! The projection is forward-only: missing flows rows insert, task links
//! fill in, and statuses move terminal-ward, but a terminal flows row is
//! never resurrected from a stale requests row (that would duplicate
//! `request_fulfilled` ticks and re-enrol satisfied watches).

use std::sync::Arc;

use super::flows::stores::{RequestLedger, RequestRow, WantedStore as FlowsWantedStore};
use super::requests::ledger::RequestStore;
use super::requests::ledger::WantedStore as RequestsWantedStore;

/// Mirror one requests store plus its watches into the flows stores.
/// Lock or read failures skip the pass; the next pass retries.
pub fn mirror_requests_into_flows(
    requests: &Arc<RequestStore>,
    wanted: &Arc<RequestsWantedStore>,
    ledger: &Arc<RequestLedger>,
    watches: &Arc<FlowsWantedStore>,
    now: i64,
) {
    mirror_rows(requests, ledger);
    mirror_watches(wanted, watches, now);
}

/// Whether a flows status is terminal (mirrors `is_terminal` so this
/// module does not depend on its exact set drifting silently).
fn flows_terminal(status: &str) -> bool {
    super::flows::stores::is_terminal(status)
}

/// requests statuses that count as terminal for the forward-only rule.
/// `cancelling` is transient (its row is becoming cancelled); `rejected`
/// is terminal for the requester but unknown to the flows vocabulary, so
/// it projects to `cancelled`.
fn project_status(status: &str) -> &str {
    match status {
        "rejected" | "cancelling" => "cancelled",
        other => other,
    }
}

fn mirror_rows(requests: &Arc<RequestStore>, ledger: &Arc<RequestLedger>) {
    let rows = match requests.history(None, None, None, "newest") {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(?error, "acquire mirror: requests read failed");
            return;
        }
    };
    for record in rows {
        let status = project_status(&record.status);
        if let Some(existing) = ledger.get(&record.key) {
            // Never resurrect a terminal flows row from a stale requests
            // row, and never clobber a flows-side task link with nothing.
            if flows_terminal(&existing.status) && !flows_terminal(status) {
                continue;
            }
            if existing.status == status
                && (record.task_id.is_none() || existing.task_id == record.task_id)
            {
                continue;
            }
            ledger.upsert(RequestRow {
                mbid: record.key.clone(),
                kind: record.kind.as_str().to_owned(),
                user_id: record
                    .user_id
                    .clone()
                    .or_else(|| {
                        record
                            .requesters
                            .first()
                            .map(|requester| requester.user_id.clone())
                    })
                    .unwrap_or_default(),
                artist: record.artist_name.clone(),
                title: record
                    .track_title
                    .clone()
                    .filter(|_| record.kind.as_str() == "track")
                    .unwrap_or_else(|| record.album_title.clone()),
                status: status.to_owned(),
                task_id: record.task_id.clone().or(existing.task_id),
                generation: existing.generation,
                completed_at: record.completed_at.map(|at| at as i64),
            });
            continue;
        }
        ledger.upsert(RequestRow {
            mbid: record.key.clone(),
            kind: record.kind.as_str().to_owned(),
            user_id: record
                .user_id
                .clone()
                .or_else(|| {
                    record
                        .requesters
                        .first()
                        .map(|requester| requester.user_id.clone())
                })
                .unwrap_or_default(),
            artist: record.artist_name.clone(),
            title: record
                .track_title
                .clone()
                .filter(|_| record.kind.as_str() == "track")
                .unwrap_or_else(|| record.album_title.clone()),
            status: status.to_owned(),
            task_id: record.task_id.clone(),
            generation: 0,
            completed_at: record.completed_at.map(|at| at as i64),
        });
    }
}

fn mirror_watches(wanted: &Arc<RequestsWantedStore>, watches: &Arc<FlowsWantedStore>, now: i64) {
    let rows = match wanted.watches_for("", true) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(?error, "acquire mirror: wanted read failed");
            return;
        }
    };
    for watch in rows {
        if watch.state != "watching" {
            continue;
        }
        // `enrol` keeps the existing watch when one covers the MBID, so a
        // mirrored watch never resets the loop's quiet streak or cursor.
        watches.enrol(super::flows::stores::Watch {
            rg_mbid: watch.key,
            user_id: watch.user_id,
            artist: watch.artist_name,
            title: watch.album_title,
            first_release_date: None,
            quiet_streak: 0,
            next_check_at: now,
        });
    }
}
