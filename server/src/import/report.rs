//! Machine-readable import report: one JSON object per run.
//!
//! Import and dry-run both produce exactly one of these; the CLI prints it
//! to stdout. Counts only, never secret material. Items carry semantic
//! keys; non-`imported` outcomes are itemized in full while imports are
//! sampled (first 50 per entity, then counts only).

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::auth::times::to_iso;

/// Report envelope marker.
pub const REPORT_FORMAT: &str = "droppedneedle-import-report";
/// Report schema version.
pub const REPORT_FORMAT_VERSION: u32 = 1;
/// Imported outcomes sampled per entity before counts-only cutoff.
pub const IMPORT_SAMPLE_LIMIT: usize = 50;

/// Counted entities, in stable report order.
pub const ENTITIES: &[&str] = &[
    "user",
    "provider",
    "app_password",
    "recovery_code",
    "follow",
    "approval",
];

/// Import exit code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ExitCode {
    /// All imported or idempotent skips.
    #[serde(rename = "OK")]
    Ok,
    /// Any dropped/conflict/nulled count is nonzero; still exit 0.
    #[serde(rename = "OK_WITH_DROPS")]
    OkWithDrops,
    /// The export failed validation; zero writes.
    #[serde(rename = "FAILED_VALIDATION")]
    FailedValidation,
    /// The passphrase did not open the envelope; zero writes.
    #[serde(rename = "ENVELOPE_AUTH_FAILED")]
    EnvelopeAuthFailed,
    /// Anything else; the transaction rolled back.
    #[serde(rename = "FAILED_INTERNAL")]
    FailedInternal,
}

/// Per-entity outcome counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct EntityCounts {
    /// Rows inserted or applied fresh.
    pub imported: u64,
    /// Rows already identical; zero writes.
    pub skipped_identical: u64,
    /// Rows where the existing value won a conflict or merge.
    pub conflict_kept_existing: u64,
    /// Follows/approvals dropped for an unknown user.
    pub dropped_unknown_user: u64,
    /// Records dropped as invalid at import time.
    pub dropped_invalid: u64,
    /// Fields nulled to survive a collision. Counts fields, not records:
    /// the record itself also counts under its own outcome.
    pub nulled_field: u64,
    /// Rows that failed with an internal error.
    pub error: u64,
}

impl EntityCounts {
    /// True when any attention-worthy counter is nonzero.
    #[must_use]
    pub fn has_drops(&self) -> bool {
        self.conflict_kept_existing > 0
            || self.dropped_unknown_user > 0
            || self.dropped_invalid > 0
            || self.nulled_field > 0
            || self.error > 0
    }
}

/// One itemized outcome with its semantic key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReportItem {
    /// Entity name (`user`, `follow`, ...; `export` for file-level
    /// findings, `settings` for section notes, `rebuild` for post-import
    /// work).
    pub entity: String,
    /// Semantic key (an MBID or username), never secret material.
    pub key: String,
    /// Outcome (`imported`, `skipped_identical`, `conflict_kept_existing`,
    /// `dropped_unknown_user`, `dropped_invalid`, `nulled_field`, `error`).
    pub outcome: String,
    /// Short human detail, empty when obvious.
    pub detail: String,
}

/// Provenance of the imported file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExportProvenance {
    /// Export `format_version`.
    pub format_version: u32,
    /// Export `exported_at` verbatim.
    pub exported_at: String,
    /// Export `instance_id` verbatim.
    pub instance_id: String,
}

/// Exit status block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExitStatus {
    /// Machine-readable exit code.
    pub code: ExitCode,
    /// Human sentence, empty on clean success.
    pub message: String,
}

/// The full report object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// Always `droppedneedle-import-report`.
    pub format: String,
    /// Always 1.
    pub format_version: u32,
    /// True for dry-run reports.
    pub dry_run: bool,
    /// Report start, UTC ISO-8601.
    pub started_at: String,
    /// Report finish, UTC ISO-8601.
    pub finished_at: String,
    /// Provenance copied from the export file.
    pub export_file: ExportProvenance,
    /// Exit status.
    pub exit: ExitStatus,
    /// Per-entity counters; every entity key present even when zero.
    pub entities: HashMap<String, EntityCounts>,
    /// Settings sections applied from the file.
    pub settings_applied: Vec<String>,
    /// Known sections missing from the file, reset to v3 defaults.
    pub settings_defaulted: Vec<String>,
    /// Secrets re-encrypted under the v3 key (count only).
    pub secrets_reencrypted: u64,
    /// Itemized outcomes with semantic keys.
    pub items: Vec<ReportItem>,
}

impl ImportReport {
    /// Render the report as the single JSON object the CLI prints.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_owned())
    }
}

/// Builder that accumulates counters and sampled items during the run.
#[derive(Debug)]
pub struct ReportBuilder {
    dry_run: bool,
    started_at: String,
    export_file: ExportProvenance,
    entities: HashMap<String, EntityCounts>,
    settings_applied: Vec<String>,
    settings_defaulted: Vec<String>,
    secrets_reencrypted: u64,
    items: Vec<ReportItem>,
    sampled_imports: HashMap<String, usize>,
    committed: bool,
}

impl ReportBuilder {
    /// Start a report for one run.
    #[must_use]
    pub fn new(dry_run: bool, export_file: ExportProvenance) -> Self {
        let mut entities = HashMap::new();
        for entity in ENTITIES {
            entities.insert((*entity).to_owned(), EntityCounts::default());
        }
        Self {
            dry_run,
            started_at: utc_now_iso(),
            export_file,
            entities,
            settings_applied: Vec::new(),
            settings_defaulted: Vec::new(),
            secrets_reencrypted: 0,
            items: Vec::new(),
            sampled_imports: HashMap::new(),
            committed: false,
        }
    }

    /// Secrets counted as re-encrypted so far.
    #[must_use]
    pub fn secrets_counted(&self) -> u64 {
        self.secrets_reencrypted
    }

    /// Take back secrets counted for a config that is not written.
    pub fn uncount_secrets(&mut self, count: u64) {
        self.secrets_reencrypted = self.secrets_reencrypted.saturating_sub(count);
    }

    /// Note that the database transaction committed.
    pub fn mark_committed(&mut self) {
        self.committed = true;
    }

    /// True once the database transaction committed.
    #[must_use]
    pub fn committed(&self) -> bool {
        self.committed
    }

    /// Zero every counter: the run failed before its writes landed, so the
    /// planned counts describe nothing that happened.
    pub fn discard_counts(&mut self) {
        for counts in self.entities.values_mut() {
            *counts = EntityCounts::default();
        }
        self.secrets_reencrypted = 0;
    }

    /// Record one entity outcome, sampling plain imports.
    pub fn record(&mut self, entity: &str, key: String, outcome: &str, detail: String) {
        let counts = self.entities.entry(entity.to_owned()).or_default();
        match outcome {
            "imported" => counts.imported += 1,
            "skipped_identical" => counts.skipped_identical += 1,
            "conflict_kept_existing" => counts.conflict_kept_existing += 1,
            "dropped_unknown_user" => counts.dropped_unknown_user += 1,
            "dropped_invalid" => counts.dropped_invalid += 1,
            "nulled_field" => counts.nulled_field += 1,
            "error" => counts.error += 1,
            _ => counts.error += 1,
        }
        let sampled = outcome == "imported" && {
            let seen = self.sampled_imports.entry(entity.to_owned()).or_insert(0);
            *seen += 1;
            *seen > IMPORT_SAMPLE_LIMIT
        };
        if !sampled {
            self.items.push(ReportItem {
                entity: entity.to_owned(),
                key,
                outcome: outcome.to_owned(),
                detail,
            });
        }
    }

    /// Record a file-level, settings, or rebuild note. These never touch
    /// entity counters.
    pub fn note(&mut self, entity: &str, key: String, outcome: &str, detail: String) {
        self.items.push(ReportItem {
            entity: entity.to_owned(),
            key,
            outcome: outcome.to_owned(),
            detail,
        });
    }

    /// Count one re-encrypted secret.
    pub fn secret_reencrypted(&mut self) {
        self.secrets_reencrypted += 1;
    }

    /// Mark a settings section applied.
    pub fn setting_applied(&mut self, section: String) {
        self.settings_applied.push(section);
    }

    /// Mark a known section reset to v3 defaults.
    pub fn setting_defaulted(&mut self, section: String) {
        self.settings_defaulted.push(section);
    }

    /// Move a section from defaulted to applied (the schedule carry lands
    /// after the replace loop already defaulted the missing section).
    pub fn setting_reapplied(&mut self, section: &str) {
        self.settings_defaulted.retain(|listed| listed != section);
        self.setting_applied(section.to_owned());
    }

    /// Finish with an explicit exit status.
    #[must_use]
    pub fn finish(mut self, code: ExitCode, message: String) -> ImportReport {
        self.settings_applied.sort();
        self.settings_defaulted.sort();
        ImportReport {
            format: REPORT_FORMAT.to_owned(),
            format_version: REPORT_FORMAT_VERSION,
            dry_run: self.dry_run,
            started_at: self.started_at,
            finished_at: utc_now_iso(),
            export_file: self.export_file,
            exit: ExitStatus { code, message },
            entities: self.entities,
            settings_applied: self.settings_applied,
            settings_defaulted: self.settings_defaulted,
            secrets_reencrypted: self.secrets_reencrypted,
            items: self.items,
        }
    }

    /// True when any attention-worthy counter is nonzero.
    #[must_use]
    pub fn has_drops(&self) -> bool {
        self.entities.values().any(EntityCounts::has_drops)
    }

    /// Current per-entity counters as JSON, for the audit row.
    #[must_use]
    pub fn counts_json(&self) -> String {
        serde_json::to_string(&self.entities).unwrap_or_else(|_| "{}".to_owned())
    }

    /// Finish a completed run: `OK`, or `OK_WITH_DROPS` when any
    /// attention-worthy counter is nonzero.
    #[must_use]
    pub fn finish_completed(self) -> ImportReport {
        let attention = self.entities.values().any(EntityCounts::has_drops);
        if attention {
            self.finish(
                ExitCode::OkWithDrops,
                "import completed with drops or conflicts; see items".to_owned(),
            )
        } else {
            self.finish(ExitCode::Ok, String::new())
        }
    }
}

/// Current UTC time as ISO-8601. Falls back to the epoch on clock
/// failure rather than failing the report.
#[must_use]
pub fn utc_now_iso() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|span| span.as_secs())
        .unwrap_or(0);
    to_iso(i64::try_from(secs).unwrap_or(0))
}
