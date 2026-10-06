//! Pause, resume, and stop: what each control does to a job in each state.
//!
//! Ported from v2's `request_operation_control`. A control on a job that is
//! already where the caller wants it changes nothing (and still answers
//! with the job). A running job is never stopped under its worker: the
//! request is recorded and the worker honors it at its next checkpoint.

use super::models::{Control, ControlRequest, OperationJob, OperationState};

/// Terminal code a failed job may be resumed from: the provider was down,
/// so trying again can succeed.
pub const RESUMABLE_FAILURE: &str = "PROVIDER_TEMPORARILY_UNAVAILABLE";

/// What one control does to one job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlPlan {
    /// Nothing to do; answer with the job as it is.
    Unchanged,
    /// Leave the job running and ask the worker to pause or stop.
    Request(ControlRequest),
    /// Stop now: nothing is running the job.
    StopNow,
    /// Put the job back in the queue.
    Requeue(Requeue),
}

/// How much of a requeued job starts over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Requeue {
    /// Pick up where it paused.
    Continue,
    /// Retry the failed work items; finished ones stay finished.
    RetryFailed,
    /// A stopped re-identification evaluates again from scratch.
    Restart,
}

pub fn plan(job: &OperationJob, control: Control) -> ControlPlan {
    match control {
        Control::Resume => match job.state {
            OperationState::Paused => ControlPlan::Requeue(Requeue::Continue),
            OperationState::Stopped if job.is_reidentification() => {
                ControlPlan::Requeue(Requeue::Restart)
            }
            OperationState::Stopped => ControlPlan::Requeue(Requeue::Continue),
            OperationState::Failed if job.terminal_code.as_deref() == Some(RESUMABLE_FAILURE) => {
                ControlPlan::Requeue(Requeue::RetryFailed)
            }
            _ => ControlPlan::Unchanged,
        },
        Control::Pause | Control::Stop => match job.state {
            OperationState::Succeeded | OperationState::Cancelled | OperationState::Stopped => {
                ControlPlan::Unchanged
            }
            OperationState::Queued | OperationState::Paused if control == Control::Stop => {
                ControlPlan::StopNow
            }
            // A re-identification waiting on a choice holds no worker.
            OperationState::Ready if control == Control::Stop && job.is_reidentification() => {
                ControlPlan::StopNow
            }
            _ => ControlPlan::Request(if control == Control::Stop {
                ControlRequest::Stop
            } else {
                ControlRequest::Pause
            }),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(kind: &str, state: OperationState, terminal: Option<&str>) -> OperationJob {
        OperationJob {
            id: "job".to_owned(),
            kind: kind.to_owned(),
            state,
            requested_by_user_id: None,
            expected_work_count: 1,
            completed_count: 0,
            succeeded_count: 0,
            failed_count: 0,
            skipped_count: 0,
            control_request: ControlRequest::None,
            terminal_code: terminal.map(str::to_owned),
            reidentification_attempt_count: 0,
            row_revision: 1,
            event_revision: 0,
            created_at: 0.0,
            updated_at: 0.0,
        }
    }

    #[test]
    fn controls_follow_v2_per_state() {
        use OperationState::*;
        let reid = "explicit_reidentification";
        let repair = "repair";
        let cases = [
            (
                reid,
                Paused,
                None,
                Control::Resume,
                ControlPlan::Requeue(Requeue::Continue),
            ),
            (
                reid,
                Stopped,
                None,
                Control::Resume,
                ControlPlan::Requeue(Requeue::Restart),
            ),
            (
                repair,
                Stopped,
                None,
                Control::Resume,
                ControlPlan::Requeue(Requeue::Continue),
            ),
            (
                reid,
                Failed,
                Some(RESUMABLE_FAILURE),
                Control::Resume,
                ControlPlan::Requeue(Requeue::RetryFailed),
            ),
            (
                reid,
                Failed,
                Some("STALE_INPUT"),
                Control::Resume,
                ControlPlan::Unchanged,
            ),
            (reid, Running, None, Control::Resume, ControlPlan::Unchanged),
            (reid, Succeeded, None, Control::Stop, ControlPlan::Unchanged),
            (reid, Queued, None, Control::Stop, ControlPlan::StopNow),
            (reid, Paused, None, Control::Stop, ControlPlan::StopNow),
            (reid, Ready, None, Control::Stop, ControlPlan::StopNow),
            (
                repair,
                Ready,
                None,
                Control::Stop,
                ControlPlan::Request(ControlRequest::Stop),
            ),
            (
                reid,
                Running,
                None,
                Control::Stop,
                ControlPlan::Request(ControlRequest::Stop),
            ),
            (
                reid,
                Running,
                None,
                Control::Pause,
                ControlPlan::Request(ControlRequest::Pause),
            ),
            (
                reid,
                Queued,
                None,
                Control::Pause,
                ControlPlan::Request(ControlRequest::Pause),
            ),
        ];
        for (kind, state, terminal, control, expected) in cases {
            assert_eq!(
                plan(&job(kind, state, terminal), control),
                expected,
                "{kind} {state:?} {control:?}"
            );
        }
    }
}
