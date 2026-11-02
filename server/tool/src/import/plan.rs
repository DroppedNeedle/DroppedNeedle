//! The merge plan: conflict rules, deleted-ID rules and idempotent re-import.
//! The decisions are pure functions over a snapshot, so dry-run and real
//! import share them by construction.
//!
//! Deleted IDs fail closed: the validator refuses the whole file on a
//! dangling user reference, and the operator repair is dropping the orphan
//! rows from v2 and re-exporting. The `dropped_unknown_user` branches below
//! are defense in depth unreachable through [`super::run_import`]. Merges
//! follow the export spec exactly: most-permissive state wins (a pending
//! import reopens a rejected row), `requested_at` takes the min, and a nulled
//! reviewer keeps its display name as audit residue.

use std::collections::{HashMap, HashSet};

use droppedneedle_auth::Role;
use droppedneedle_auth::federated::password_import::HashScheme;
use droppedneedle_auth::times::parse_iso_ms;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::report::ReportBuilder;
use super::snapshot::{DatabaseSnapshot, coord_bits};
use super::sql::UserRow;

/// Cities kept per user; the same limit as catalog's `concerts::models::MAX_CITIES`.
pub const MAX_EVENT_CITIES: usize = 50;
/// City match radius bounds in km; the same as catalog's `MIN_RADIUS_KM` and `MAX_RADIUS_KM`.
pub const EVENT_CITY_RADIUS_KM: std::ops::RangeInclusive<f64> = 1.0..=500.0;
/// Radius a v2 city without one gets, as the picker defaults it.
const DEFAULT_RADIUS_KM: f64 = 30.0;

/// A city's radius as v3 stores it: the v2 value clamped into range.
fn event_city_radius(record: &Value) -> f64 {
    num_or(record, "radius_km", DEFAULT_RADIUS_KM)
        .clamp(*EVENT_CITY_RADIUS_KM.start(), *EVENT_CITY_RADIUS_KM.end())
}

/// Per user: cities already saved or planned, and the next free position.
#[derive(Debug, Default, Clone, Copy)]
struct CitySlots {
    count: usize,
    next_position: i64,
}

/// An approval time from the export (REAL epoch seconds) as the table
/// stores it: integer milliseconds.
pub(super) fn approval_ms(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

// --- merge plan --------------------------------------------------------------

/// One decided record: what to write (real runs) and what to report.
pub(super) enum Planned {
    InsertUser {
        record: Value,
        email_nulled: bool,
        username_nulled: bool,
    },
    InsertProvider {
        record: Value,
        user_id: String,
    },
    InsertAppPassword {
        record: Value,
        user_id: String,
        name: String,
        secret_path: String,
    },
    InsertRecovery {
        record: Value,
        user_id: String,
    },
    InsertFollow {
        record: Value,
    },
    MergeFollow {
        key: (String, String),
        auto_download: bool,
        followed_at: i64,
        updated_at: i64,
    },
    InsertApproval {
        record: Value,
        reviewer_nulled: bool,
    },
    MergeApproval {
        key: (String, String),
        state: String,
        requested_at: i64,
        reviewed_by_id: Option<String>,
        reviewed_by_name: Option<String>,
        reviewed_at: Option<i64>,
        batch_id: Option<String>,
        source: Option<String>,
    },
    InsertEventCity {
        record: Value,
        radius_km: f64,
        position: i64,
    },
    SetEventSeen {
        user_id: String,
        seen_at: f64,
    },
}

/// The decided import. A dry run decides it for the report and applies
/// nothing.
pub(super) struct Plan {
    pub(super) writes: Vec<Planned>,
}

impl Plan {
    /// True when the plan carries at least one database write.
    pub(super) fn has_writes(&self) -> bool {
        !self.writes.is_empty()
    }

    /// Decide every record against the snapshot, recording outcomes.
    /// Pure apart from the report: dry-run and real import share it.
    pub(super) fn decide(
        root: &Value,
        snapshot: &DatabaseSnapshot,
        unsealed: &HashMap<String, String>,
        report: &mut ReportBuilder,
    ) -> Self {
        let mut plan = Self { writes: Vec::new() };
        let empty_list = Vec::new();
        let users = root
            .get("users")
            .and_then(Value::as_array)
            .unwrap_or(&empty_list);
        let mut known_users: HashSet<String> = snapshot.users.keys().cloned().collect();
        let mut batch_emails: HashMap<String, String> = HashMap::new();
        let mut batch_usernames: HashMap<String, String> = HashMap::new();
        let mut batch_shas: HashSet<String> = HashSet::new();

        for user in users {
            plan.decide_user(
                user,
                snapshot,
                &mut batch_emails,
                &mut batch_usernames,
                report,
            );
            if let Some(id) = user.get("id").and_then(Value::as_str) {
                known_users.insert(id.to_owned());
            }
        }
        for (user_index, user) in users.iter().enumerate() {
            plan.decide_user_children(
                user,
                user_index,
                snapshot,
                unsealed,
                &mut batch_shas,
                report,
            );
        }
        let follows = root
            .get("follows")
            .and_then(Value::as_array)
            .unwrap_or(&empty_list);
        for follow in follows {
            plan.decide_follow(follow, snapshot, &known_users, report);
        }
        let approvals = root
            .get("approvals")
            .and_then(Value::as_array)
            .unwrap_or(&empty_list);
        for approval in approvals {
            plan.decide_approval(approval, snapshot, &known_users, report);
        }
        let event_cities = root
            .get("event_cities")
            .and_then(Value::as_array)
            .unwrap_or(&empty_list);
        let mut slots: HashMap<String, CitySlots> = HashMap::new();
        for city in snapshot.event_cities.values() {
            let slot = slots.entry(city.user_id.clone()).or_default();
            slot.count += 1;
            slot.next_position = slot.next_position.max(city.position + 1);
        }
        let mut event_cities: Vec<&Value> = event_cities.iter().collect();
        event_cities.sort_by_key(|city| city.get("position").and_then(Value::as_i64).unwrap_or(0));
        for city in event_cities {
            plan.decide_event_city(city, snapshot, &known_users, &mut slots, report);
        }
        let event_seen = root
            .get("event_seen")
            .and_then(Value::as_array)
            .unwrap_or(&empty_list);
        for seen in event_seen {
            plan.decide_event_seen(seen, snapshot, &known_users, report);
        }
        plan
    }

    fn decide_user(
        &mut self,
        user: &Value,
        snapshot: &DatabaseSnapshot,
        batch_emails: &mut HashMap<String, String>,
        batch_usernames: &mut HashMap<String, String>,
        report: &mut ReportBuilder,
    ) {
        let id = user.get("id").and_then(Value::as_str).unwrap_or_default();
        if id.is_empty() {
            report.record(
                "user",
                String::new(),
                "dropped_invalid",
                "user record without an id".to_owned(),
            );
            return;
        }
        if let Some(existing) = snapshot.users.get(id) {
            if users_identical(existing, user) {
                report.record("user", id.to_owned(), "skipped_identical", String::new());
            } else {
                report.record(
                    "user",
                    id.to_owned(),
                    "conflict_kept_existing",
                    "same id, different fields; existing row kept".to_owned(),
                );
            }
            return;
        }
        let mut email_nulled = false;
        let mut username_nulled = false;
        if let Some(email) = opt_str(user, "email") {
            let taken_elsewhere = snapshot.emails.get(&email).is_some_and(|owner| owner != id)
                || batch_emails.get(&email).is_some_and(|owner| owner != id);
            if taken_elsewhere {
                email_nulled = true;
                report.record(
                    "user",
                    id.to_owned(),
                    "nulled_field",
                    "email collides across ids; imported nulled".to_owned(),
                );
            } else {
                batch_emails.insert(email, id.to_owned());
            }
        }
        if let Some(username) = opt_str(user, "username") {
            let taken_elsewhere = snapshot
                .usernames
                .get(&username)
                .is_some_and(|owner| owner != id)
                || batch_usernames
                    .get(&username)
                    .is_some_and(|owner| owner != id);
            if taken_elsewhere {
                username_nulled = true;
                report.record(
                    "user",
                    id.to_owned(),
                    "nulled_field",
                    "username collides across ids; imported nulled".to_owned(),
                );
            } else {
                batch_usernames.insert(username, id.to_owned());
            }
        }
        report.record("user", id.to_owned(), "imported", String::new());
        self.writes.push(Planned::InsertUser {
            record: user.clone(),
            email_nulled,
            username_nulled,
        });
    }

    fn decide_user_children(
        &mut self,
        user: &Value,
        user_index: usize,
        snapshot: &DatabaseSnapshot,
        unsealed: &HashMap<String, String>,
        batch_shas: &mut HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let user_id = user.get("id").and_then(Value::as_str).unwrap_or_default();
        if user_id.is_empty() {
            return;
        }
        let empty_list = Vec::new();
        let providers = user
            .get("providers")
            .and_then(Value::as_array)
            .unwrap_or(&empty_list);
        for provider in providers {
            self.decide_provider(provider, user_id, snapshot, report);
        }
        let passwords = user
            .get("app_passwords")
            .and_then(Value::as_array)
            .unwrap_or(&empty_list);
        for (index, password) in passwords.iter().enumerate() {
            let secret_path = format!("users[{user_index}].app_passwords[{index}].secret");
            self.decide_app_password(
                password,
                user_id,
                &secret_path,
                snapshot,
                unsealed,
                batch_shas,
                report,
            );
        }
        if let Some(recovery) = user.get("recovery_code")
            && !recovery.is_null()
        {
            self.decide_recovery(recovery, user_id, snapshot, report);
        }
    }

    fn decide_provider(
        &mut self,
        provider: &Value,
        user_id: &str,
        snapshot: &DatabaseSnapshot,
        report: &mut ReportBuilder,
    ) {
        let name = provider
            .get("provider")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let uid = provider
            .get("provider_uid")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let key = format!("{name}|{uid}");
        if name.is_empty() || uid.is_empty() {
            report.record(
                "provider",
                key,
                "dropped_invalid",
                "provider record without a binding".to_owned(),
            );
            return;
        }
        match snapshot.providers.get(&(name.to_owned(), uid.to_owned())) {
            None => {
                report.record("provider", key, "imported", String::new());
                self.writes.push(Planned::InsertProvider {
                    record: provider.clone(),
                    user_id: user_id.to_owned(),
                });
            }
            Some(existing) => {
                let (provider_data, password) = provider_columns(provider);
                let (scheme, hash) = password.unzip();
                if existing.user_id == user_id
                    && existing.provider_data == provider_data
                    && existing.hash_scheme == scheme
                    && existing.password_hash == hash
                    && existing.created_at == time_ms(provider, "created_at")
                {
                    report.record("provider", key, "skipped_identical", String::new());
                } else {
                    report.record(
                        "provider",
                        key,
                        "conflict_kept_existing",
                        "provider binding already mapped; existing row kept".to_owned(),
                    );
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn decide_app_password(
        &mut self,
        password: &Value,
        user_id: &str,
        secret_path: &str,
        snapshot: &DatabaseSnapshot,
        unsealed: &HashMap<String, String>,
        batch_shas: &mut HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let name = str_or(password, "name", "");
        if name.is_empty() {
            report.record(
                "app_password",
                format!("{user_id}|"),
                "dropped_invalid",
                "app password without a name".to_owned(),
            );
            return;
        }
        let created_at = str_or(password, "created_at", "");
        let created_ms = time_ms(password, "created_at");
        let key = format!("{user_id}|{name}|{created_at}");
        let secret_sha = unsealed
            .get(secret_path)
            .map(|plaintext| sha_hex(plaintext));
        let rows = snapshot
            .app_passwords
            .get(&(user_id.to_owned(), name.clone()));
        // The hash column is UNIQUE across all users: a reused secret
        // cannot insert twice, so the first registration wins.
        let sha_taken = secret_sha.as_ref().is_some_and(|sha| {
            let same_row = rows.is_some_and(|existing| {
                existing
                    .iter()
                    .any(|row| row.secret_sha256 == *sha && row.created_at == created_ms)
            });
            !same_row && (snapshot.app_password_shas.contains(sha) || batch_shas.contains(sha))
        });
        if sha_taken {
            report.record(
                "app_password",
                key,
                "conflict_kept_existing",
                "secret hash already registered under another name; existing kept".to_owned(),
            );
            return;
        }
        if let Some(sha) = secret_sha.clone() {
            batch_shas.insert(sha);
        }
        match rows {
            None => {
                report.record("app_password", key, "imported", String::new());
                report.secret_reencrypted();
                self.writes.push(Planned::InsertAppPassword {
                    record: password.clone(),
                    user_id: user_id.to_owned(),
                    name,
                    secret_path: secret_path.to_owned(),
                });
            }
            Some(existing) => {
                let identical = existing.iter().any(|row| {
                    row.created_at == created_ms
                        && secret_sha
                            .as_ref()
                            .is_some_and(|sha| *sha == row.secret_sha256)
                        && row.last_used_at == opt_time_ms(password, "last_used_at")
                        && row.last_client == opt_str(password, "last_client")
                        && row.revoked == i64::from(bool_or(password, "revoked", false))
                });
                if identical {
                    report.record("app_password", key, "skipped_identical", String::new());
                    return;
                }
                if existing.iter().any(|row| row.created_at == created_ms) {
                    report.record(
                        "app_password",
                        key,
                        "conflict_kept_existing",
                        "same name and timestamp with a different secret; existing kept".to_owned(),
                    );
                    return;
                }
                let mut candidate = format!("{name} ({created_at})");
                let mut suffix = 2;
                while snapshot
                    .app_passwords
                    .contains_key(&(user_id.to_owned(), candidate.clone()))
                {
                    candidate = format!("{name} ({created_at}) #{suffix}");
                    suffix += 1;
                }
                report.record(
                    "app_password",
                    key,
                    "conflict_kept_existing",
                    format!("name collides; imported as {candidate:?}"),
                );
                report.secret_reencrypted();
                self.writes.push(Planned::InsertAppPassword {
                    record: password.clone(),
                    user_id: user_id.to_owned(),
                    name: candidate,
                    secret_path: secret_path.to_owned(),
                });
            }
        }
    }

    fn decide_recovery(
        &mut self,
        recovery: &Value,
        user_id: &str,
        snapshot: &DatabaseSnapshot,
        report: &mut ReportBuilder,
    ) {
        match snapshot.recoveries.get(user_id) {
            None => {
                report.record(
                    "recovery_code",
                    user_id.to_owned(),
                    "imported",
                    String::new(),
                );
                self.writes.push(Planned::InsertRecovery {
                    record: recovery.clone(),
                    user_id: user_id.to_owned(),
                });
            }
            Some(existing) => {
                if existing.code_hash == str_or(recovery, "code_hash", "")
                    && existing.created_at == time_ms(recovery, "created_at")
                    && existing.expires_at == time_ms(recovery, "expires_at")
                {
                    report.record(
                        "recovery_code",
                        user_id.to_owned(),
                        "skipped_identical",
                        String::new(),
                    );
                } else {
                    report.record(
                        "recovery_code",
                        user_id.to_owned(),
                        "conflict_kept_existing",
                        "recovery code already set; existing kept".to_owned(),
                    );
                }
            }
        }
    }

    fn decide_follow(
        &mut self,
        follow: &Value,
        snapshot: &DatabaseSnapshot,
        known_users: &HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let user_id = follow
            .get("user_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mbid_lower = follow
            .get("artist_mbid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let key = format!("{user_id}|{mbid_lower}");
        if !known_users.contains(user_id) {
            report.record(
                "follow",
                key,
                "dropped_unknown_user",
                "user is not imported; never invented".to_owned(),
            );
            return;
        }
        if mbid_lower.is_empty() {
            report.record(
                "follow",
                key,
                "dropped_invalid",
                "follow without an MBID".to_owned(),
            );
            return;
        }
        let lookup = (user_id.to_owned(), mbid_lower);
        match snapshot.follows.get(&lookup) {
            None => {
                report.record("follow", key, "imported", String::new());
                self.writes.push(Planned::InsertFollow {
                    record: follow.clone(),
                });
            }
            Some(existing) => {
                let merged_auto =
                    existing.auto_download != 0 || bool_or(follow, "auto_download", false);
                let merged_followed =
                    existing
                        .followed_at
                        .min(approval_ms(num_or(follow, "followed_at", 0.0)));
                let merged_updated =
                    existing
                        .updated_at
                        .max(approval_ms(num_or(follow, "updated_at", 0.0)));
                if existing.auto_download == i64::from(merged_auto)
                    && existing.followed_at == merged_followed
                    && existing.updated_at == merged_updated
                {
                    report.record("follow", key, "skipped_identical", String::new());
                } else {
                    report.record(
                        "follow",
                        key,
                        "conflict_kept_existing",
                        "merged: auto_download OR, followed_at min, updated_at max".to_owned(),
                    );
                    self.writes.push(Planned::MergeFollow {
                        key: lookup,
                        auto_download: merged_auto,
                        followed_at: merged_followed,
                        updated_at: merged_updated,
                    });
                }
            }
        }
    }

    /// A city the user already has (same coordinates) keeps its saved
    /// name, radius and order. A new one goes after the user's other
    /// cities, in export order, with its radius clamped; past the per-user
    /// limit it is dropped.
    fn decide_event_city(
        &mut self,
        city: &Value,
        snapshot: &DatabaseSnapshot,
        known_users: &HashSet<String>,
        slots: &mut HashMap<String, CitySlots>,
        report: &mut ReportBuilder,
    ) {
        let user_id = str_or(city, "user_id", "");
        let latitude = num_or(city, "latitude", 0.0);
        let longitude = num_or(city, "longitude", 0.0);
        let key = format!("{user_id}|{latitude},{longitude}");
        if !known_users.contains(&user_id) {
            report.record(
                "event_city",
                key,
                "dropped_unknown_user",
                "user is not imported; never invented".to_owned(),
            );
            return;
        }
        let radius_km = event_city_radius(city);
        match snapshot.event_cities.get(&(
            user_id.clone(),
            coord_bits(latitude),
            coord_bits(longitude),
        )) {
            None => {
                let slot = slots.entry(user_id).or_default();
                if slot.count >= MAX_EVENT_CITIES {
                    report.record(
                        "event_city",
                        key,
                        "dropped_invalid",
                        format!("user already has {MAX_EVENT_CITIES} cities"),
                    );
                    return;
                }
                report.record("event_city", key, "imported", String::new());
                self.writes.push(Planned::InsertEventCity {
                    record: city.clone(),
                    radius_km,
                    position: slot.next_position,
                });
                slot.count += 1;
                slot.next_position += 1;
            }
            Some(existing) => {
                let identical = existing.city_name == str_or(city, "city_name", "")
                    && existing.country_code == opt_str(city, "country_code")
                    && existing.radius_km == radius_km;
                if identical {
                    report.record("event_city", key, "skipped_identical", String::new());
                } else {
                    report.record(
                        "event_city",
                        key,
                        "conflict_kept_existing",
                        "same coordinates already saved; existing row kept".to_owned(),
                    );
                }
            }
        }
    }

    /// Seen markers merge to the later time, so the badge never counts
    /// gigs the user already saw on either side.
    fn decide_event_seen(
        &mut self,
        seen: &Value,
        snapshot: &DatabaseSnapshot,
        known_users: &HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let user_id = str_or(seen, "user_id", "");
        if !known_users.contains(&user_id) {
            report.record(
                "event_seen",
                user_id,
                "dropped_unknown_user",
                "user is not imported; never invented".to_owned(),
            );
            return;
        }
        let imported = num_or(seen, "seen_at", 0.0);
        match snapshot.event_seen.get(&user_id) {
            None => {
                report.record("event_seen", user_id.clone(), "imported", String::new());
                self.writes.push(Planned::SetEventSeen {
                    user_id,
                    seen_at: imported,
                });
            }
            Some(&existing) if existing >= imported => {
                report.record("event_seen", user_id, "skipped_identical", String::new());
            }
            Some(_) => {
                report.record(
                    "event_seen",
                    user_id.clone(),
                    "conflict_kept_existing",
                    "merged: seen_at max".to_owned(),
                );
                self.writes.push(Planned::SetEventSeen {
                    user_id,
                    seen_at: imported,
                });
            }
        }
    }

    fn decide_approval(
        &mut self,
        approval: &Value,
        snapshot: &DatabaseSnapshot,
        known_users: &HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let user_id = approval
            .get("user_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let mbid_lower = approval
            .get("artist_mbid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_lowercase();
        let key = format!("{user_id}|{mbid_lower}");
        if !known_users.contains(user_id) {
            report.record(
                "approval",
                key,
                "dropped_unknown_user",
                "user is not imported; never invented".to_owned(),
            );
            return;
        }
        if mbid_lower.is_empty() {
            report.record(
                "approval",
                key,
                "dropped_invalid",
                "approval without an MBID".to_owned(),
            );
            return;
        }
        let reviewer = opt_str(approval, "reviewed_by_id");
        // Only the id is nulled; the display name stays verbatim as audit
        // residue (pinned by a test).
        let reviewer_nulled = reviewer
            .as_ref()
            .is_some_and(|id| !known_users.contains(id));
        if reviewer_nulled {
            report.record(
                "approval",
                key.clone(),
                "nulled_field",
                "reviewer is not an imported user; reviewed_by_id nulled".to_owned(),
            );
        }
        let lookup = (user_id.to_owned(), mbid_lower);
        match snapshot.approvals.get(&lookup) {
            None => {
                report.record("approval", key, "imported", String::new());
                self.writes.push(Planned::InsertApproval {
                    record: approval.clone(),
                    reviewer_nulled,
                });
            }
            Some(existing) => {
                let incoming_state = str_or(approval, "state", "pending");
                let winner_incoming =
                    permissiveness(&incoming_state) > permissiveness(&existing.state);
                let (state, batch_id, source, by_id, by_name, at) = if winner_incoming {
                    (
                        incoming_state,
                        opt_str(approval, "batch_id"),
                        opt_str(approval, "source"),
                        opt_str(approval, "reviewed_by_id").filter(|_| !reviewer_nulled),
                        opt_str(approval, "reviewed_by_name"),
                        opt_num(approval, "reviewed_at").map(approval_ms),
                    )
                } else {
                    (
                        existing.state.clone(),
                        existing.batch_id.clone(),
                        existing.source.clone(),
                        existing.reviewed_by_id.clone(),
                        existing.reviewed_by_name.clone(),
                        existing.reviewed_at,
                    )
                };
                // Earliest request wins. The spec does not name this field; min
                // keeps re-imports convergent.
                let requested_at =
                    existing
                        .requested_at
                        .min(approval_ms(num_or(approval, "requested_at", 0.0)));
                if existing.state == state
                    && existing.batch_id == batch_id
                    && existing.source == source
                    && existing.reviewed_by_id == by_id
                    && existing.reviewed_by_name == by_name
                    && existing.reviewed_at == at
                    && existing.requested_at == requested_at
                {
                    report.record("approval", key, "skipped_identical", String::new());
                } else {
                    report.record(
                        "approval",
                        key,
                        "conflict_kept_existing",
                        "merged: most-permissive state wins".to_owned(),
                    );
                    self.writes.push(Planned::MergeApproval {
                        key: lookup,
                        state,
                        requested_at,
                        reviewed_by_id: by_id,
                        reviewed_by_name: by_name,
                        reviewed_at: at,
                        batch_id,
                        source,
                    });
                }
            }
        }
    }
}

/// Approval-state permissiveness, as the export spec defines it: approved beats pending beats
/// the rest. A pending import therefore reopens a rejected row, and unknown
/// states never win a merge. Pinned by a test, not just convergent.
fn permissiveness(state: &str) -> u8 {
    match state {
        "approved" => 2,
        "pending" => 1,
        _ => 0,
    }
}

/// v2 served uploaded avatars at `/api/v1/profile/avatar/{id}`; v3 serves
/// the same file at `/api/v3/users/{id}/avatar`. The cache-busting query
/// is kept. Any other URL (a Plex picture, say) passes through.
pub(super) fn v3_avatar_url(user_id: &str, raw: Option<String>) -> Option<String> {
    let raw = raw?;
    let Some(rest) = raw.strip_prefix(&format!("/api/v1/profile/avatar/{user_id}")) else {
        return Some(raw);
    };
    let version = rest
        .strip_prefix("?v=")
        .filter(|version| !version.is_empty())
        .unwrap_or("0");
    Some(droppedneedle_auth::users::services::avatar_url(
        user_id, version,
    ))
}

fn users_identical(existing: &UserRow, imported: &Value) -> bool {
    existing.display_name == str_or(imported, "display_name", "")
        && existing.email == opt_str(imported, "email")
        && existing.avatar_url == v3_avatar_url(&existing.id, opt_str(imported, "avatar_url"))
        && existing.role == role_of(imported)
        && existing.username == opt_str(imported, "username")
        && existing.username_display == opt_str(imported, "username_display")
        && existing.created_at == time_ms(imported, "created_at")
        && existing.last_login_at == opt_time_ms(imported, "last_login_at")
}

/// A v2 time as the auth tables store it: integer milliseconds, or none
/// when it does not read as one (migration 0101's rule).
pub(super) fn opt_time_ms(record: &Value, field: &str) -> Option<i64> {
    record
        .get(field)
        .and_then(Value::as_str)
        .and_then(parse_iso_ms)
}

/// A v2 time that must have a value: one that does not read stores 0.
pub(super) fn time_ms(record: &Value, field: &str) -> i64 {
    opt_time_ms(record, field).unwrap_or(0)
}

/// A v2 role as stored: an unknown role is `user`, as it always read.
pub(super) fn role_of(record: &Value) -> &'static str {
    Role::from_stored(&str_or(record, "role", "user")).as_str()
}

/// A v2 provider row as stored: `(provider_data, (scheme, hash))`. A
/// local row's `{"password_hash", "scheme"}` document becomes the password
/// columns, an untagged one is bcrypt, and one that does not read holds no
/// password; any other provider keeps `provider_data` as it is.
pub(super) fn provider_columns(record: &Value) -> (Option<String>, Option<(String, String)>) {
    let data = opt_str(record, "provider_data");
    if str_or(record, "provider", "") != "local" {
        return (data, None);
    }
    let document: Option<Value> = data.and_then(|text| serde_json::from_str(&text).ok());
    let password = document.as_ref().and_then(|document| {
        let hash = document.get("password_hash")?.as_str()?;
        let tag = document
            .get("scheme")
            .and_then(Value::as_str)
            .unwrap_or(HashScheme::Bcrypt.as_tag());
        let scheme = HashScheme::from_tag(tag)?;
        Some((scheme.as_tag().to_owned(), hash.to_owned()))
    });
    (None, password)
}

pub(super) fn str_or(record: &Value, field: &str, fallback: &str) -> String {
    record
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or(fallback)
        .to_owned()
}

pub(super) fn opt_str(record: &Value, field: &str) -> Option<String> {
    record.get(field).and_then(Value::as_str).map(str::to_owned)
}

pub(super) fn num_or(record: &Value, field: &str, fallback: f64) -> f64 {
    record
        .get(field)
        .and_then(Value::as_f64)
        .unwrap_or(fallback)
}

pub(super) fn opt_num(record: &Value, field: &str) -> Option<f64> {
    record.get(field).and_then(Value::as_f64)
}

pub(super) fn bool_or(record: &Value, field: &str, fallback: bool) -> bool {
    record
        .get(field)
        .and_then(Value::as_bool)
        .unwrap_or(fallback)
}

/// Lowercase hex SHA-256 of an app-password secret, matching the
/// `secret_sha256` column the compat clients authenticate against.
pub(super) fn sha_hex(plaintext: &str) -> String {
    let digest = Sha256::digest(plaintext.as_bytes());
    format!("{digest:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::import::report::ExportProvenance;

    fn empty_snapshot() -> DatabaseSnapshot {
        DatabaseSnapshot {
            users: HashMap::new(),
            emails: HashMap::new(),
            usernames: HashMap::new(),
            providers: HashMap::new(),
            app_passwords: HashMap::new(),
            app_password_shas: HashSet::new(),
            recoveries: HashMap::new(),
            follows: HashMap::new(),
            approvals: HashMap::new(),
            event_cities: HashMap::new(),
            event_seen: HashMap::new(),
        }
    }

    fn builder() -> ReportBuilder {
        ReportBuilder::new(
            false,
            ExportProvenance {
                format_version: 1,
                exported_at: String::new(),
                instance_id: String::new(),
            },
        )
    }

    /// Defense in depth, unreachable through `run_import`: validation
    /// refuses dangling references before planning, so this branch only
    /// fires on an unvalidated document. A follow or approval whose user is
    /// unknown drops with a count, and no user row is ever invented for it.
    #[test]
    fn dangling_members_drop_without_inventing_users() {
        let root = serde_json::json!({
            "users": [],
            "follows": [{
                "user_id": "ghost",
                "artist_mbid": "01234567-89ab-cdef-0123-456789abcdef",
                "artist_name": "Ghost Artist",
                "auto_download": false,
                "followed_at": 1.0,
                "updated_at": 2.0,
            }],
            "approvals": [{
                "user_id": "ghost",
                "artist_mbid": "01234567-89ab-cdef-0123-456789abcdef",
                "artist_name": "Ghost Artist",
                "state": "pending",
                "requested_at": 1.0,
            }],
        });
        let mut report = builder();
        let plan = Plan::decide(&root, &empty_snapshot(), &HashMap::new(), &mut report);
        assert!(plan.writes.is_empty(), "drops plan no writes");
        let finished = report.finish_completed();
        assert_eq!(finished.entities["follow"].dropped_unknown_user, 1);
        assert_eq!(finished.entities["approval"].dropped_unknown_user, 1);
        assert_eq!(finished.entities["user"].imported, 0);
    }
}
