//! The merge plan: conflict rules, deleted-ID rules and idempotent re-import.
//! The decisions are pure functions over the typed document and a snapshot, so
//! dry-run and real import share them by construction. The plan decides new,
//! merged or identical; the areas offer plain row writes, and a re-import
//! calls none of them.
//!
//! Deleted IDs fail closed: the validator refuses the whole file on a
//! dangling user reference, and the operator repair is dropping the orphan
//! rows from v2 and re-exporting. The `dropped_unknown_user` branches below
//! are defense in depth unreachable through [`super::run_import`]. Merges
//! follow the export spec exactly: most-permissive state wins (a pending
//! import reopens a rejected row), `requested_at` takes the min, and a nulled
//! reviewer keeps its display name as audit residue.
//!
//! A v2 value an area would not take is never a failed import and never a
//! silent default: it is converted or clamped with a report line, or its
//! record is left behind with one.
//!
//! Auth rows store times through typed forms that take whole seconds, and no
//! auth reader sees finer, so times there are compared at whole seconds.

use std::collections::{HashMap, HashSet};

use droppedneedle_acquire::follows::rows::{ApprovalFields, FollowRow as NewFollow};
use droppedneedle_acquire::requests::ledger::ApprovalState;
use droppedneedle_auth::Role;
use droppedneedle_auth::federated::password_import::HashScheme;
use droppedneedle_auth::times::{ms_to_secs, parse_iso};
use droppedneedle_auth::users::models::UserRecord as NewUser;
use droppedneedle_catalog::concerts::models::{EventCity, MAX_CITIES};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::report::ReportBuilder;
use super::snapshot::{DatabaseSnapshot, coord_bits};
use super::sql::UserRow;
use crate::export::envelope::{
    AppPasswordRecord, ApprovalRecord, EventCityRecord, EventSeenRecord, ExportDoc, FollowRecord,
    ProviderRecord, UserRecord,
};

/// Report outcome for a v2 value that was changed so v3 could take it.
const CONVERTED: &str = "converted";

/// Per user: cities already saved or planned, and the next free position.
#[derive(Debug, Default, Clone, Copy)]
struct CitySlots {
    count: usize,
    next_position: i64,
}

/// A time from the export (REAL epoch seconds) as the tables store it:
/// integer milliseconds.
pub(super) fn approval_ms(seconds: f64) -> i64 {
    (seconds * 1000.0).round() as i64
}

/// A record's optional REAL epoch seconds as stored milliseconds.
fn opt_approval_ms(seconds: Option<f64>) -> Option<i64> {
    seconds.map(approval_ms)
}

/// One decided record: what to write (real runs) and what to report.
pub(super) enum Planned {
    InsertUser(NewUser),
    InsertProvider {
        user_id: String,
        provider: String,
        provider_uid: String,
        provider_data: Option<String>,
        /// `(scheme, hash)` of a local row.
        password: Option<(String, String)>,
        created_at: i64,
    },
    InsertAppPassword {
        user_id: String,
        name: String,
        secret_path: String,
        created_at: i64,
        last_used_at: Option<i64>,
        last_client: Option<String>,
        revoked: bool,
    },
    InsertFollow(NewFollow),
    MergeFollow {
        user_id: String,
        artist_mbid_lower: String,
        auto_download: bool,
        followed_at: i64,
        updated_at: i64,
    },
    InsertApproval {
        user_id: String,
        artist_mbid: String,
        artist_name: String,
        fields: ApprovalFields,
    },
    MergeApproval {
        user_id: String,
        artist_mbid_lower: String,
        fields: ApprovalFields,
    },
    InsertEventCity {
        user_id: String,
        city: EventCity,
        position: i64,
    },
    SetEventSeen {
        user_id: String,
        /// Unix milliseconds.
        seen_at: i64,
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
        doc: &ExportDoc,
        snapshot: &DatabaseSnapshot,
        unsealed: &HashMap<String, String>,
        report: &mut ReportBuilder,
    ) -> Self {
        let mut plan = Self { writes: Vec::new() };
        let mut known_users: HashSet<String> = snapshot.users.keys().cloned().collect();
        let mut batch_emails: HashMap<String, String> = HashMap::new();
        let mut batch_usernames: HashMap<String, String> = HashMap::new();
        let mut batch_shas: HashSet<String> = HashSet::new();

        for user in &doc.users {
            plan.decide_user(
                user,
                snapshot,
                &mut batch_emails,
                &mut batch_usernames,
                report,
            );
            known_users.insert(user.id.clone());
        }
        for (user_index, user) in doc.users.iter().enumerate() {
            plan.decide_user_children(
                user,
                user_index,
                snapshot,
                unsealed,
                &mut batch_shas,
                report,
            );
        }
        if doc.users.iter().any(|user| user.recovery_code.is_some()) {
            report.note(
                "user",
                "recovery_code".to_owned(),
                "not_carried",
                "recovery codes from v2 were not carried; make a new one".to_owned(),
            );
        }
        for follow in &doc.follows {
            plan.decide_follow(follow, snapshot, &known_users, report);
        }
        for approval in &doc.approvals {
            plan.decide_approval(approval, snapshot, &known_users, report);
        }
        let mut slots: HashMap<String, CitySlots> = HashMap::new();
        for city in snapshot.event_cities.values() {
            let slot = slots.entry(city.user_id.clone()).or_default();
            slot.count += 1;
            slot.next_position = slot.next_position.max(city.position + 1);
        }
        let mut event_cities: Vec<&EventCityRecord> = doc.event_cities.iter().collect();
        event_cities.sort_by_key(|city| city.position);
        for city in event_cities {
            plan.decide_event_city(city, snapshot, &known_users, &mut slots, report);
        }
        for seen in &doc.event_seen {
            plan.decide_event_seen(seen, snapshot, &known_users, report);
        }
        plan
    }

    fn decide_user(
        &mut self,
        user: &UserRecord,
        snapshot: &DatabaseSnapshot,
        batch_emails: &mut HashMap<String, String>,
        batch_usernames: &mut HashMap<String, String>,
        report: &mut ReportBuilder,
    ) {
        let id = user.id.as_str();
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
        let mut email = user.email.clone();
        if let Some(wanted) = user.email.as_ref() {
            let taken_elsewhere = snapshot.emails.get(wanted).is_some_and(|owner| owner != id)
                || batch_emails.get(wanted).is_some_and(|owner| owner != id);
            if taken_elsewhere {
                email = None;
                report.record(
                    "user",
                    id.to_owned(),
                    "nulled_field",
                    "email collides across ids; imported nulled".to_owned(),
                );
            } else {
                batch_emails.insert(wanted.clone(), id.to_owned());
            }
        }
        let mut username = user.username.clone();
        if let Some(wanted) = user.username.as_ref() {
            let taken_elsewhere = snapshot
                .usernames
                .get(wanted)
                .is_some_and(|owner| owner != id)
                || batch_usernames.get(wanted).is_some_and(|owner| owner != id);
            if taken_elsewhere {
                username = None;
                report.record(
                    "user",
                    id.to_owned(),
                    "nulled_field",
                    "username collides across ids; imported nulled".to_owned(),
                );
            } else {
                batch_usernames.insert(wanted.clone(), id.to_owned());
            }
        }
        note_empty_name("user", id, "display_name", &user.display_name, report);
        if Role::parse(&user.role).is_none() {
            report.note(
                "user",
                id.to_owned(),
                CONVERTED,
                "v2's role is not one v3 has; imported as user".to_owned(),
            );
        }
        let created_at = required_secs("user", id, "created_at", &user.created_at, report);
        let last_login_at = opt_secs(
            "user",
            id,
            "last_login_at",
            user.last_login_at.as_deref(),
            report,
        );
        report.record("user", id.to_owned(), "imported", String::new());
        self.writes.push(Planned::InsertUser(NewUser {
            id: id.to_owned(),
            username,
            username_display: user.username_display.clone(),
            display_name: user.display_name.clone(),
            email,
            avatar_url: v3_avatar_url(id, user.avatar_url.clone()),
            role: Role::from_stored(&user.role),
            created_at,
            last_login_at,
        }));
    }

    fn decide_user_children(
        &mut self,
        user: &UserRecord,
        user_index: usize,
        snapshot: &DatabaseSnapshot,
        unsealed: &HashMap<String, String>,
        batch_shas: &mut HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        for provider in &user.providers {
            self.decide_provider(provider, &user.id, snapshot, report);
        }
        for (index, password) in user.app_passwords.iter().enumerate() {
            let secret_path = format!("users[{user_index}].app_passwords[{index}].secret");
            self.decide_app_password(
                password,
                &user.id,
                &secret_path,
                snapshot,
                unsealed,
                batch_shas,
                report,
            );
        }
    }

    fn decide_provider(
        &mut self,
        provider: &ProviderRecord,
        user_id: &str,
        snapshot: &DatabaseSnapshot,
        report: &mut ReportBuilder,
    ) {
        let name = provider.provider.as_str();
        let uid = provider.provider_uid.as_str();
        let key = format!("{name}|{uid}");
        let (provider_data, password) = provider_columns(provider);
        match snapshot.providers.get(&(name.to_owned(), uid.to_owned())) {
            None => {
                let created_at =
                    required_secs("provider", &key, "created_at", &provider.created_at, report);
                report.record("provider", key, "imported", String::new());
                self.writes.push(Planned::InsertProvider {
                    user_id: user_id.to_owned(),
                    provider: name.to_owned(),
                    provider_uid: uid.to_owned(),
                    provider_data,
                    password,
                    created_at,
                });
            }
            Some(existing) => {
                let (scheme, hash) = password.unzip();
                if existing.user_id == user_id
                    && existing.provider_data == provider_data
                    && existing.hash_scheme == scheme
                    && existing.password_hash == hash
                    && ms_to_secs(existing.created_at) == secs_or_zero(&provider.created_at)
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
        password: &AppPasswordRecord,
        user_id: &str,
        secret_path: &str,
        snapshot: &DatabaseSnapshot,
        unsealed: &HashMap<String, String>,
        batch_shas: &mut HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let name = password.name.clone();
        if name.is_empty() {
            report.record(
                "app_password",
                format!("{user_id}|"),
                "dropped_invalid",
                "app password without a name".to_owned(),
            );
            return;
        }
        let created_at = password.created_at.as_str();
        let created_secs = secs_or_zero(created_at);
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
                existing.iter().any(|row| {
                    row.secret_sha256 == *sha && ms_to_secs(row.created_at) == created_secs
                })
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
        let last_used = password.last_used_at.as_deref();
        let final_name = match rows {
            None => {
                report.record("app_password", key.clone(), "imported", String::new());
                name
            }
            Some(existing) => {
                let identical = existing.iter().any(|row| {
                    ms_to_secs(row.created_at) == created_secs
                        && secret_sha
                            .as_ref()
                            .is_some_and(|sha| *sha == row.secret_sha256)
                        && row.last_used_at.map(ms_to_secs) == last_used.and_then(parse_iso)
                        && row.last_client == password.last_client
                        && row.revoked == i64::from(password.revoked)
                });
                if identical {
                    report.record("app_password", key, "skipped_identical", String::new());
                    return;
                }
                if existing
                    .iter()
                    .any(|row| ms_to_secs(row.created_at) == created_secs)
                {
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
                    key.clone(),
                    "conflict_kept_existing",
                    format!("name collides; imported as {candidate:?}"),
                );
                candidate
            }
        };
        report.secret_reencrypted();
        // No per-user cap here, unlike a new password made in the app: a
        // client's working password must not be lost at upgrade.
        self.writes.push(Planned::InsertAppPassword {
            user_id: user_id.to_owned(),
            name: final_name,
            secret_path: secret_path.to_owned(),
            created_at: required_secs("app_password", &key, "created_at", created_at, report),
            last_used_at: opt_secs("app_password", &key, "last_used_at", last_used, report),
            last_client: password.last_client.clone(),
            revoked: password.revoked,
        });
    }

    fn decide_follow(
        &mut self,
        follow: &FollowRecord,
        snapshot: &DatabaseSnapshot,
        known_users: &HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let user_id = follow.user_id.as_str();
        let mbid = follow.artist_mbid.trim();
        let mbid_lower = mbid.to_lowercase();
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
        let lookup = (user_id.to_owned(), mbid_lower);
        match snapshot.follows.get(&lookup) {
            None => {
                for (field, value) in [
                    ("followed_at", follow.followed_at),
                    ("updated_at", follow.updated_at),
                ] {
                    note_missing_time("follow", &key, field, value, report);
                }
                note_empty_name("follow", &key, "artist_name", &follow.artist_name, report);
                report.record("follow", key, "imported", String::new());
                self.writes.push(Planned::InsertFollow(NewFollow {
                    user_id: user_id.to_owned(),
                    artist_mbid: mbid.to_owned(),
                    artist_name: follow.artist_name.clone(),
                    auto_download: follow.auto_download,
                    followed_at: approval_ms(follow.followed_at),
                    updated_at: approval_ms(follow.updated_at),
                }));
            }
            Some(existing) => {
                let merged_auto = existing.auto_download != 0 || follow.auto_download;
                let merged_followed = existing.followed_at.min(approval_ms(follow.followed_at));
                let merged_updated = existing.updated_at.max(approval_ms(follow.updated_at));
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
                    let (user_id, artist_mbid_lower) = lookup;
                    self.writes.push(Planned::MergeFollow {
                        user_id,
                        artist_mbid_lower,
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
        city: &EventCityRecord,
        snapshot: &DatabaseSnapshot,
        known_users: &HashSet<String>,
        slots: &mut HashMap<String, CitySlots>,
        report: &mut ReportBuilder,
    ) {
        let user_id = city.user_id.as_str();
        let key = format!("{user_id}|{},{}", city.latitude, city.longitude);
        if !known_users.contains(user_id) {
            report.record(
                "event_city",
                key,
                "dropped_unknown_user",
                "user is not imported; never invented".to_owned(),
            );
            return;
        }
        let name = city.city_name.trim().to_owned();
        let Ok(new_city) = EventCity::new(
            name.clone(),
            city.latitude,
            city.longitude,
            city.radius_km,
            city.country_code.clone(),
        ) else {
            report.record(
                "event_city",
                key,
                "dropped_invalid",
                "coordinates or radius v3 refuses".to_owned(),
            );
            return;
        };
        match snapshot.event_cities.get(&(
            user_id.to_owned(),
            coord_bits(city.latitude),
            coord_bits(city.longitude),
        )) {
            None => {
                let slot = slots.entry(user_id.to_owned()).or_default();
                if slot.count >= MAX_CITIES {
                    report.record(
                        "event_city",
                        key,
                        "dropped_invalid",
                        format!("user already has {MAX_CITIES} cities"),
                    );
                    return;
                }
                if new_city.radius_km() != city.radius_km {
                    report.note(
                        "event_city",
                        key.clone(),
                        CONVERTED,
                        format!(
                            "radius {} km is outside what v3 allows; now {} km",
                            city.radius_km,
                            new_city.radius_km()
                        ),
                    );
                }
                report.record("event_city", key, "imported", String::new());
                self.writes.push(Planned::InsertEventCity {
                    user_id: user_id.to_owned(),
                    city: new_city,
                    position: slot.next_position,
                });
                slot.count += 1;
                slot.next_position += 1;
            }
            Some(existing) => {
                let identical = existing.city_name == name
                    && existing.country_code == city.country_code
                    && existing.radius_km == new_city.radius_km();
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
        seen: &EventSeenRecord,
        snapshot: &DatabaseSnapshot,
        known_users: &HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let user_id = seen.user_id.clone();
        if !known_users.contains(&user_id) {
            report.record(
                "event_seen",
                user_id,
                "dropped_unknown_user",
                "user is not imported; never invented".to_owned(),
            );
            return;
        }
        let imported = approval_ms(seen.seen_at);
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
        approval: &ApprovalRecord,
        snapshot: &DatabaseSnapshot,
        known_users: &HashSet<String>,
        report: &mut ReportBuilder,
    ) {
        let user_id = approval.user_id.as_str();
        let mbid = approval.artist_mbid.trim();
        let mbid_lower = mbid.to_lowercase();
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
        let incoming_state = if approval.state.is_empty() {
            report.note(
                "approval",
                key.clone(),
                CONVERTED,
                "the record names no state; imported as pending".to_owned(),
            );
            ApprovalState::Pending
        } else if let Some(state) = ApprovalState::parse(&approval.state) {
            state
        } else {
            report.record(
                "approval",
                key,
                "dropped_invalid",
                "the record's state is not one v3 has".to_owned(),
            );
            return;
        };
        // Only the id is nulled; the display name stays verbatim as audit
        // residue (pinned by a test).
        let reviewer_nulled = approval
            .reviewed_by_id
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
        let incoming = ApprovalFields {
            state: incoming_state,
            requested_at: approval_ms(approval.requested_at),
            reviewed_by_id: approval.reviewed_by_id.clone().filter(|_| !reviewer_nulled),
            reviewed_by_name: approval.reviewed_by_name.clone(),
            reviewed_at: opt_approval_ms(approval.reviewed_at),
            batch_id: approval.batch_id.clone(),
            source: approval.source.clone(),
        };
        let lookup = (user_id.to_owned(), mbid_lower);
        match snapshot.approvals.get(&lookup) {
            None => {
                note_missing_time(
                    "approval",
                    &key,
                    "requested_at",
                    approval.requested_at,
                    report,
                );
                note_empty_name(
                    "approval",
                    &key,
                    "artist_name",
                    &approval.artist_name,
                    report,
                );
                report.record("approval", key, "imported", String::new());
                self.writes.push(Planned::InsertApproval {
                    user_id: user_id.to_owned(),
                    artist_mbid: mbid.to_owned(),
                    artist_name: approval.artist_name.clone(),
                    fields: incoming,
                });
            }
            Some(existing) => {
                let stored_state = ApprovalState::parse(&existing.state);
                let winner_incoming =
                    permissiveness(incoming_state) > stored_state.map_or(0, permissiveness);
                let mut merged = if winner_incoming {
                    incoming
                } else {
                    ApprovalFields {
                        state: stored_state.unwrap_or(incoming_state),
                        requested_at: 0,
                        reviewed_by_id: existing.reviewed_by_id.clone(),
                        reviewed_by_name: existing.reviewed_by_name.clone(),
                        reviewed_at: existing.reviewed_at,
                        batch_id: existing.batch_id.clone(),
                        source: existing.source.clone(),
                    }
                };
                // Earliest request wins. The spec does not name this field; min
                // keeps re-imports convergent.
                merged.requested_at = existing
                    .requested_at
                    .min(approval_ms(approval.requested_at));
                if existing.state == merged.state.as_str()
                    && existing.batch_id == merged.batch_id
                    && existing.source == merged.source
                    && existing.reviewed_by_id == merged.reviewed_by_id
                    && existing.reviewed_by_name == merged.reviewed_by_name
                    && existing.reviewed_at == merged.reviewed_at
                    && existing.requested_at == merged.requested_at
                {
                    report.record("approval", key, "skipped_identical", String::new());
                } else {
                    report.record(
                        "approval",
                        key,
                        "conflict_kept_existing",
                        "merged: most-permissive state wins".to_owned(),
                    );
                    let (user_id, artist_mbid_lower) = lookup;
                    self.writes.push(Planned::MergeApproval {
                        user_id,
                        artist_mbid_lower,
                        fields: merged,
                    });
                }
            }
        }
    }
}

/// Approval-state permissiveness, as the export spec defines it: approved beats pending beats
/// the rest. A pending import therefore reopens a rejected row. Pinned by a test, not just
/// convergent.
fn permissiveness(state: ApprovalState) -> u8 {
    match state {
        ApprovalState::Approved => 2,
        ApprovalState::Pending => 1,
        ApprovalState::Rejected | ApprovalState::Revoked => 0,
    }
}

/// v2 served uploaded avatars at `/api/v1/profile/avatar/{id}`; v3 serves
/// the same file at `/api/v3/users/{id}/avatar`. The cache-busting query
/// is kept. Any other URL (a Plex picture, say) passes through.
fn v3_avatar_url(user_id: &str, raw: Option<String>) -> Option<String> {
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

fn users_identical(existing: &UserRow, imported: &UserRecord) -> bool {
    existing.display_name == imported.display_name
        && existing.email == imported.email
        && existing.avatar_url == v3_avatar_url(&existing.id, imported.avatar_url.clone())
        && existing.role == Role::from_stored(&imported.role).as_str()
        && existing.username == imported.username
        && existing.username_display == imported.username_display
        && ms_to_secs(existing.created_at) == secs_or_zero(&imported.created_at)
        && existing.last_login_at.map(ms_to_secs)
            == imported.last_login_at.as_deref().and_then(parse_iso)
}

/// A v2 time that must have a value, in whole seconds: 0 when it does not
/// read.
fn secs_or_zero(raw: &str) -> i64 {
    parse_iso(raw).unwrap_or(0)
}

/// A required time for a row about to be written, with a report line when
/// it does not read.
fn required_secs(
    entity: &str,
    key: &str,
    field: &str,
    raw: &str,
    report: &mut ReportBuilder,
) -> i64 {
    parse_iso(raw).unwrap_or_else(|| {
        report.note(
            entity,
            key.to_owned(),
            CONVERTED,
            format!("{field} does not read as a time; stored as 1970"),
        );
        0
    })
}

/// An optional time for a row about to be written, with a report line when
/// it is there and does not read.
fn opt_secs(
    entity: &str,
    key: &str,
    field: &str,
    raw: Option<&str>,
    report: &mut ReportBuilder,
) -> Option<i64> {
    let raw = raw?;
    let parsed = parse_iso(raw);
    if parsed.is_none() {
        report.note(
            entity,
            key.to_owned(),
            CONVERTED,
            format!("{field} does not read as a time; left empty"),
        );
    }
    parsed
}

/// A report line for an epoch time a record did not hold.
fn note_missing_time(entity: &str, key: &str, field: &str, value: f64, report: &mut ReportBuilder) {
    if value == 0.0 {
        report.note(
            entity,
            key.to_owned(),
            CONVERTED,
            format!("the record holds no {field}; stored as 1970"),
        );
    }
}

/// A report line for a name a record did not hold.
fn note_empty_name(entity: &str, key: &str, field: &str, value: &str, report: &mut ReportBuilder) {
    if value.is_empty() {
        report.note(
            entity,
            key.to_owned(),
            CONVERTED,
            format!("the record holds no {field}; it is empty"),
        );
    }
}

/// A v2 provider row as stored: `(provider_data, (scheme, hash))`. A
/// local row's `{"password_hash", "scheme"}` document becomes the password
/// columns, an untagged one is bcrypt, and one that does not read holds no
/// password; any other provider keeps `provider_data` as it is.
fn provider_columns(record: &ProviderRecord) -> (Option<String>, Option<(String, String)>) {
    let data = record.provider_data.clone();
    if record.provider != "local" {
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
        let doc: ExportDoc = serde_json::from_value(serde_json::json!({
            "format": "droppedneedle-export",
            "format_version": 1,
            "exported_at": "",
            "instance_id": "",
            "secret_envelope": {
                "scheme": "",
                "kdf": {"algo": "", "m": 0, "t": 0, "p": 0, "salt_b64": ""},
                "nonce_b64": "",
            },
            "settings": {},
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
        }))
        .unwrap();
        let mut report = builder();
        let plan = Plan::decide(&doc, &empty_snapshot(), &HashMap::new(), &mut report);
        assert!(plan.writes.is_empty(), "drops plan no writes");
        let finished = report.finish_completed();
        assert_eq!(finished.entities["follow"].dropped_unknown_user, 1);
        assert_eq!(finished.entities["approval"].dropped_unknown_user, 1);
        assert_eq!(finished.entities["user"].imported, 0);
    }
}
