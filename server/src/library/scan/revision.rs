//! Stat-derived change detection for hashless scans.
//!
//! Port of `backend/services/native/file_revision.py`, including the
//! accepted F-029 tradeoff: a revision is `size:mtime_ns` only. A content
//! swap that preserves size and lands in the same mtime tick classifies as
//! unchanged. Folding `ctime` into the revision would narrow this but
//! mass-promote legacy entries to changed, so it needs explicit owner
//! sign-off before anyone reaches for it.

use std::fs::Metadata;
use std::time::UNIX_EPOCH;

/// Revision string for one file: `{size_bytes}:{mtime_ns}`.
pub fn exact_stat_revision(file_size_bytes: u64, file_mtime_ns: i64) -> String {
    format!("{file_size_bytes}:{file_mtime_ns}")
}

/// Revision string from live metadata.
pub fn revision_from_metadata(meta: &Metadata) -> String {
    let mtime_ns = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0);
    exact_stat_revision(meta.len(), mtime_ns)
}

/// mtime nanoseconds from live metadata (0 when unavailable).
pub fn mtime_ns_from_metadata(meta: &Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

/// One unit in the last place for a float (v2 `math.ulp`).
fn ulp(value: f64) -> f64 {
    if !value.is_normal() {
        // Matches math.ulp(0.0): the smallest subnormal.
        return f64::MIN_POSITIVE * f64::EPSILON;
    }
    2f64.powi(value.abs().log2().floor() as i32 - 52)
}

/// Symmetric epsilon band for legacy float mtime compares (v2
/// `_legacy_mtime_eps_seconds`, F-15/4.12): `max(1us, 4*ulp)`, so the band
/// tracks epoch growth instead of silently drifting. Covers float error
/// only; real rewrites still classify as changed.
pub fn legacy_mtime_eps_seconds(file_mtime: f64) -> f64 {
    1e-6f64.max(4.0 * ulp(file_mtime))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revision_spelling_matches_v2() {
        assert_eq!(
            exact_stat_revision(2892, 1700000000123456789),
            "2892:1700000000123456789"
        );
    }

    #[test]
    fn epsilon_band_has_microsecond_floor() {
        assert_eq!(
            legacy_mtime_eps_seconds(1_700_000_000.0),
            1e-6f64.max(4.0 * ulp(1_700_000_000.0))
        );
        assert!(legacy_mtime_eps_seconds(0.0) >= 1e-6);
        assert_eq!(ulp(1.0), f64::EPSILON);
    }
}
