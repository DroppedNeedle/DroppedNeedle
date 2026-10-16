//! From recorded checks to one decision.
//!
//! The file checks run first; a rejection or hold there ends the landing
//! before any lookup. Then the match checks run on the matching outcome,
//! and when nothing objects the file plan says what imports. Pure: the
//! caller runs the lookup and acts on the decision.

use super::matching::{FilePlan, MatchSummary, plan};
use super::probe::Landing;
use super::specs::{
    Check, QualityPolicy, Rejection, Subject, Target, Verdict, file_specs, match_specs, run, worst,
};

/// What to do with a landing.
#[derive(Debug, Clone, PartialEq)]
pub enum Outcome {
    /// Publish the planned files (and hold the planned holds).
    Import(FilePlan),
    /// Keep every audio file for a person.
    Hold { code: &'static str, detail: String },
    /// Not importable.
    Reject(Rejection),
}

/// The decision plus every check behind it.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub checks: Vec<Check>,
    pub outcome: Outcome,
    /// The release the files were matched to, when one was.
    pub release_mbid: Option<String>,
    pub distance: Option<f64>,
}

/// The file checks alone. `Some` when they already decide the landing.
pub fn check_files(
    target: &Target,
    landing: &Landing,
    policy: &QualityPolicy,
) -> (Vec<Check>, Option<Outcome>) {
    let subject = Subject {
        target,
        landing,
        policy,
        matched: None,
    };
    let checks = run(&file_specs(), &subject);
    let outcome = worst(&checks).map(outcome_of);
    (checks, outcome)
}

/// The full decision once matching ran.
pub fn decide(
    target: &Target,
    landing: &Landing,
    policy: &QualityPolicy,
    matched: &MatchSummary,
    mut checks: Vec<Check>,
) -> Decision {
    let subject = Subject {
        target,
        landing,
        policy,
        matched: Some(matched),
    };
    checks.extend(run(&match_specs(), &subject));
    let outcome = match worst(&checks).map(outcome_of) {
        Some(outcome) => outcome,
        None => Outcome::Import(plan(matched, target, landing.audio.len())),
    };
    Decision {
        checks,
        outcome,
        release_mbid: matched.best().map(|found| found.release.id.clone()),
        distance: matched.best().map(|found| found.scored.library_distance()),
    }
}

fn outcome_of(verdict: &Verdict) -> Outcome {
    match verdict {
        Verdict::Reject(rejection) => Outcome::Reject(rejection.clone()),
        Verdict::Hold { code, detail } => Outcome::Hold {
            code,
            detail: detail.clone(),
        },
        // `worst` only returns holds and rejections.
        Verdict::Accept { .. } => Outcome::Import(FilePlan::default()),
    }
}
