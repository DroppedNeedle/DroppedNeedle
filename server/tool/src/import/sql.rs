//! The importer's statements, written against the migrated target with the
//! export bundle attached as `bundle`.
//!
//! The schema test prepares them on a copy of the migrated template with a
//! bundle built from the section specs attached, so a renamed v3 column or
//! a section spec that names one fails there. A section's statements are
//! generated from its spec. Column and table names only ever come from the
//! spec, never from the bundle, so a crafted bundle cannot inject SQL. At
//! run time a section's statements cover the columns its bundle holds; the
//! families prepare them over every spec column, which names every column
//! a smaller list can.

use droppedneedle_kernel::db::statement::{Choice, Statement};
use rusqlite::{Connection, Row};

use crate::export::sections::requests::now_millis;
use crate::export::sections::{ALL, Column, LinkKind, LinkMode, TableSection, Target};
use droppedneedle_library::edition::choice::RemapOrigin;

droppedneedle_kernel::sql! {
    /// Every account in the target.
    pub(super) USERS = "SELECT id, display_name, email, avatar_url, role, created_at, \
             last_login_at, username, username_display FROM auth_users";
    /// Every sign-in provider link in the target.
    pub(super) PROVIDERS = "SELECT user_id, provider, provider_uid, provider_data, password_hash, \
             hash_scheme, created_at FROM auth_providers";
    /// Every app password in the target.
    pub(super) APP_PASSWORDS = "SELECT user_id, name, secret_sha256, created_at, last_used_at, \
             last_client, revoked FROM auth_app_passwords";
    /// Every recovery code in the target.
    pub(super) RECOVERY_CODES = "SELECT user_id, code_hash, created_at, expires_at \
             FROM auth_password_recovery_codes";
    /// Every followed artist in the target.
    pub(super) FOLLOWS = "SELECT user_id, artist_mbid_lower, auto_download, followed_at, \
             updated_at FROM acquire_followed_artists";
    /// Every auto-download approval in the target.
    pub(super) APPROVALS = "SELECT user_id, artist_mbid_lower, state, requested_at, \
             reviewed_by_id, reviewed_by_name, reviewed_at, batch_id, source \
             FROM auto_download_approvals";
    /// Every saved concert city in the target.
    pub(super) EVENT_CITIES = "SELECT user_id, city_name, country_code, latitude, longitude, radius_km, \
             position FROM user_event_cities";
    /// When each user in the target last looked at their concerts.
    pub(super) EVENT_SEEN = "SELECT user_id, seen_at FROM user_event_seen";

    /// An account in the target.
    #[derive(Debug, Clone)]
    pub(super) row UserRow for [USERS] {
        id: String,
        display_name: String,
        email: Option<String>,
        avatar_url: Option<String>,
        role: String,
        created_at: i64,
        last_login_at: Option<i64>,
        username: Option<String>,
        username_display: Option<String>,
    }
    /// A sign-in provider link in the target.
    #[derive(Debug, Clone)]
    pub(super) row ProviderRow for [PROVIDERS] {
        user_id: String,
        provider: String,
        provider_uid: String,
        provider_data: Option<String>,
        password_hash: Option<String>,
        hash_scheme: Option<String>,
        created_at: i64,
    }
    /// An app password in the target.
    #[derive(Debug, Clone)]
    pub(super) row AppPasswordRow for [APP_PASSWORDS] {
        user_id: String,
        name: String,
        secret_sha256: String,
        created_at: i64,
        last_used_at: Option<i64>,
        last_client: Option<String>,
        revoked: i64,
    }
    /// A recovery code in the target.
    #[derive(Debug, Clone)]
    pub(super) row RecoveryRow for [RECOVERY_CODES] {
        user_id: String,
        code_hash: String,
        created_at: i64,
        expires_at: i64,
    }
    /// A followed artist in the target.
    #[derive(Debug, Clone)]
    pub(super) row FollowRow for [FOLLOWS] {
        user_id: String,
        artist_mbid_lower: String,
        auto_download: i64,
        followed_at: i64,
        updated_at: i64,
    }
    /// An auto-download approval in the target.
    #[derive(Debug, Clone)]
    pub(super) row ApprovalRow for [APPROVALS] {
        user_id: String,
        artist_mbid_lower: String,
        state: String,
        requested_at: i64,
        reviewed_by_id: Option<String>,
        reviewed_by_name: Option<String>,
        reviewed_at: Option<i64>,
        batch_id: Option<String>,
        source: Option<String>,
    }
    /// A saved concert city in the target.
    #[derive(Debug, Clone)]
    pub(super) row EventCityRow for [EVENT_CITIES] {
        user_id: String,
        city_name: String,
        country_code: Option<String>,
        latitude: f64,
        longitude: f64,
        radius_km: f64,
        position: i64,
    }
    /// When a user in the target last looked at their concerts.
    pub(super) row EventSeenRow for [EVENT_SEEN] {
        user_id: String,
        seen_at: f64,
    }

    /// Add an account.
    pub(super) INSERT_USER = "INSERT INTO auth_users (id, display_name, email, avatar_url, role, \
             created_at, last_login_at, username, username_display) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)";
    /// Add a sign-in provider link.
    pub(super) INSERT_PROVIDER = "INSERT INTO auth_providers (id, user_id, provider, provider_uid, \
             provider_data, password_hash, hash_scheme, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)";
    /// Add an app password.
    pub(super) INSERT_APP_PASSWORD = "INSERT INTO auth_app_passwords (id, user_id, name, secret_sha256, \
             secret_encrypted, created_at, last_used_at, last_client, revoked) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)";
    /// Add a recovery code.
    pub(super) INSERT_RECOVERY_CODE = "INSERT INTO auth_password_recovery_codes (user_id, code_hash, \
             created_at, expires_at) VALUES (?, ?, ?, ?)";
    /// Add a followed artist.
    pub(super) INSERT_FOLLOW = "INSERT INTO acquire_followed_artists (user_id, artist_mbid, \
             artist_mbid_lower, artist_name, auto_download, followed_at, \
             updated_at) VALUES (?, ?, ?, ?, ?, ?, ?)";
    /// Write a follow's merged values.
    pub(super) MERGE_FOLLOW = "UPDATE acquire_followed_artists SET auto_download = ?, followed_at = ?, \
             updated_at = ? WHERE user_id = ? AND artist_mbid_lower = ?";
    /// Add a concert city.
    pub(super) INSERT_EVENT_CITY = "INSERT INTO user_event_cities (user_id, city_name, country_code, latitude, \
             longitude, radius_km, position) VALUES (?, ?, ?, ?, ?, ?, ?)";
    /// Set when a user last looked at their concerts.
    pub(super) SET_EVENT_SEEN = "INSERT INTO user_event_seen (user_id, seen_at) VALUES (?, ?) \
             ON CONFLICT(user_id) DO UPDATE SET seen_at = excluded.seen_at";
    /// Add an auto-download approval.
    pub(super) INSERT_APPROVAL = "INSERT INTO auto_download_approvals (user_id, artist_mbid, \
             artist_mbid_lower, artist_name, state, requested_at, reviewed_by_id, \
             reviewed_by_name, reviewed_at, batch_id, source) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)";
    /// Write an approval's merged values.
    pub(super) MERGE_APPROVAL = "UPDATE auto_download_approvals SET state = ?, requested_at = ?, \
             reviewed_by_id = ?, reviewed_by_name = ?, reviewed_at = ?, \
             batch_id = ?, source = ? WHERE user_id = ? AND artist_mbid_lower = ?";
    /// Add the audit row of an import run.
    pub(super) INSERT_IMPORT_RUN = "INSERT INTO import_runs (id, instance_id, exported_at, exit_code, \
             entity_counts, applied_at) VALUES (?, ?, ?, ?, ?, ?)";
    /// Refresh an import run's audit row.
    pub(super) UPDATE_IMPORT_RUN = "UPDATE import_runs SET exit_code = ?, entity_counts = ? WHERE id = ?";

    /// How many rows an earlier import of this instance wrote for a section.
    pub(super) SECTION_CARRIED = "SELECT rows_written FROM import_progress \
             WHERE instance_id = ?1 AND section = ?2";
    /// Mark a section carried.
    pub(super) MARK_SECTION_CARRIED = "INSERT INTO import_progress (instance_id, section, export_digest, \
             rows_written, applied_at) VALUES (?1, ?2, ?3, ?4, ?5)";

    /// Attach the bundle file (?1, a read-only URI).
    pub(super) ATTACH_BUNDLE = "ATTACH DATABASE ?1 AS bundle";
    /// Detach the bundle.
    pub(super) DETACH_BUNDLE = "DETACH DATABASE bundle";
    /// The columns of bundle table ?1.
    pub(super) BUNDLE_COLUMNS = "SELECT name FROM pragma_table_info(?1, 'bundle')";
    /// The columns of v3 table ?1 that have no default.
    pub(super) REQUIRED_COLUMNS = "SELECT name FROM pragma_table_info(?1, 'main') \
             WHERE \"notnull\" = 1 AND dflt_value IS NULL";
    /// Whether the bundle has table ?1.
    pub(super) BUNDLE_HAS_TABLE = "SELECT EXISTS(SELECT 1 FROM bundle.sqlite_master \
             WHERE type = 'table' AND name = ?1)";

    /// A section's rows sorted into what the import does with them: all of
    /// them, those whose parent is not in v3, those that land, and those
    /// v3 already holds whole.
    family SECTION_COUNTS = every_column_count;
    /// Insert a section's new rows.
    family SECTION_INSERTS = every_column_insert;
    /// Record a section's unresolved library references.
    family LINK_INSERTS = every_link_insert;
    /// Count a section's unresolved library references.
    family LINK_COUNTS = every_link_count;
    /// The rows in one bundle section.
    family SECTION_ROWS = section_rows;
    /// A bundle section's rows that name a user outside the list in ?1.
    family SECTION_DANGLING = section_dangling;

    /// What a section's rows come to.
    pub(super) row SectionCounts for family [every_column_count] {
        total: i64,
        orphans: i64,
        new: i64,
        identical: i64,
    }

    /// Avatar images in the bundle.
    pub(super) AVATARS = "SELECT user_id, ext, image FROM bundle.\"avatar\" ORDER BY user_id";
    /// One carried avatar.
    pub(super) row AvatarRow for [AVATARS] {
        user_id: String,
        ext: String,
        image: Vec<u8>,
    }

    /// Every per-user service link in the target.
    pub(super) CONNECTIONS = "SELECT user_id, service, connection_data, enabled, created_at, updated_at \
             FROM user_connections";
    /// A stored per-user service link.
    pub(super) row ConnectionRow for [CONNECTIONS] {
        user_id: String,
        service: String,
        connection_data: String,
        enabled: i64,
        created_at: String,
        updated_at: String,
    }
    /// Add a per-user service link.
    pub(super) INSERT_CONNECTION = "INSERT INTO user_connections (user_id, service, connection_data, enabled, \
             created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)";

    /// Every carried v2 baseline.
    pub(super) BASELINE_CANDIDATES = "SELECT local_track_id, original_root_id, original_relative_path, format, \
             semantic_snapshot_blob_sha256, CAST(created_at AS INTEGER) AS created_at, \
             ancillary_snapshot_json FROM bundle.\"management_baseline\" ORDER BY id";
    /// One carried v2 baseline.
    pub(super) row BaselineCandidate for [BASELINE_CANDIDATES] {
        local_track_id: String,
        original_root_id: String,
        original_relative_path: String,
        format: String,
        semantic_snapshot_blob_sha256: String,
        created_at: i64,
        ancillary_snapshot_json: Option<String>,
    }
    /// Whether a baseline's track (?1) would land, on a dry run.
    pub(super) BASELINE_TRACK_WOULD_LAND = "SELECT EXISTS (SELECT 1 FROM main.local_tracks WHERE id = ?1) \
             OR EXISTS (SELECT 1 FROM bundle.\"library_track\" WHERE id = ?1)";
    /// Whether a baseline's track (?1) landed: its v2 baseline row is there.
    pub(super) BASELINE_TRACK_LANDED = "SELECT EXISTS (SELECT 1 FROM main.library_management_baselines \
             WHERE local_track_id = ?1)";
    /// The bytes of one blob in the bundle.
    pub(super) BUNDLE_BLOB = "SELECT bytes FROM bundle.\"management_blob_bytes\" WHERE sha256 = ?1";
    /// The blob of a track's v3 baseline.
    pub(super) BASELINE_BLOB = "SELECT blob_sha256 FROM main.library_publish_baselines WHERE track_id = ?1";
    /// Store a blob once.
    pub(super) INSERT_BLOB = "INSERT OR IGNORE INTO main.library_publish_blobs (sha256, bytes) VALUES (?1, ?2)";
    /// Add a track's v3 baseline.
    pub(super) INSERT_BASELINE = "INSERT INTO main.library_publish_baselines \
             (track_id, blob_sha256, original_root, original_rel, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)";
    /// Reference a blob that is there.
    pub(super) INSERT_BLOB_REF = "INSERT OR IGNORE INTO main.library_publish_blob_refs (sha256, owner_kind, owner_id) \
             SELECT ?1, ?2, ?3 WHERE EXISTS \
             (SELECT 1 FROM main.library_publish_blobs WHERE sha256 = ?1)";

    /// The carried albums, as the right side of a test.
    fragment CARRIED_ALBUM = "IN (SELECT id FROM bundle.\"library_album\")";
    /// Identities an automatic pass may overwrite.
    fragment REVISABLE = "('automatic', 'embedded')";
    /// The release group of the custom edition active on identity `e`'s album.
    fragment ACTIVE_MANIFEST_GROUP = "(SELECT m.\"release_group_mbid\" FROM main.library_custom_edition_active a \
             JOIN main.library_custom_edition_manifests m ON m.id = a.manifest_id \
             WHERE a.local_album_id = e.local_album_id)";
    /// Move a pinned album's revisable identity to its pin.
    family PIN_IDENTITY_UPDATES = pin_identity_update;
    /// Give a pinned album with no identity one from its pin.
    family PIN_IDENTITY_INSERTS = pin_identity_insert;
    /// Pins naming another release than their album's manual identity.
    family PIN_MANUAL_DIFFERS = pin_manual_differs;
    /// Queue pinned albums for their files to be placed on the pinned release.
    family PIN_REMAP_QUEUE = pin_remap_queue;
    /// Move a revisable identity to its album's active custom edition.
    pub(super) CUSTOM_IDENTITY_UPDATE = "UPDATE main.local_album_external_identities AS e SET \
             release_group_mbid = " + ACTIVE_MANIFEST_GROUP + ", release_mbid = NULL, \
             decision_source = 'legacy_import', row_revision = e.row_revision + 1 \
             WHERE e.local_album_id " + CARRIED_ALBUM + " AND e.provider = 'musicbrainz' \
             AND e.decision_source IN " + REVISABLE + " AND " + ACTIVE_MANIFEST_GROUP + " IS NOT NULL";
    /// Give an album with an active custom edition and no identity one from it.
    pub(super) CUSTOM_IDENTITY_INSERT = "INSERT INTO main.local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
             selected_at) \
             SELECT a.local_album_id, 'musicbrainz', m.release_group_mbid, NULL, \
             'legacy_import', (SELECT u.id FROM main.auth_users u \
             WHERE u.id = m.sealed_by_user_id), m.sealed_at \
             FROM main.library_custom_edition_active a \
             JOIN main.library_custom_edition_manifests m ON m.id = a.manifest_id \
             WHERE a.local_album_id " + CARRIED_ALBUM + " \
             AND NOT EXISTS (SELECT 1 FROM main.local_album_external_identities e \
             WHERE e.local_album_id = a.local_album_id AND e.provider = 'musicbrainz')";

    /// The ids of the held rows in the bundle.
    pub(super) HELD_IDS = "SELECT id FROM bundle.\"held_import\" ORDER BY id";
    /// One held row's owner, time, name and bytes.
    pub(super) HELD_ROW = "SELECT user_id, created_at, held_path, \"file\" \
             FROM bundle.\"held_import\" WHERE id = ?1";
    /// One carried held row.
    pub(super) row HeldRow for [HELD_ROW] {
        user_id: String,
        created_at: f64,
        held_path: String,
        file: Option<Vec<u8>>,
    }
    /// Who already holds a file under this name, and since when.
    pub(super) HELD_EXISTING = "SELECT e.user_id, f.created_at FROM main.acquire_held_files f \
             JOIN main.acquire_held_entries e ON e.id = f.entry_id \
             WHERE f.root = 'held' AND f.path = ?1";
    /// A file already in the held list.
    pub(super) row HeldExisting for [HELD_EXISTING] {
        user_id: String,
        created_at: i64,
    }
    /// The reasons a v3 file is held for, kept as they are; the same closed
    /// list the database upgrade (0085) maps by.
    fragment KEPT_REASONS = "('weak_match', 'no_artist_evidence', 'mostly_unmatched', \
             'tag_mismatch', 'fingerprint_mismatch', 'fingerprint_unconfirmed', 'album_version_only', \
             'no_release', 'no_tracks', 'version_lower_quality', 'no_better_than_yours', \
             'no_album_match', 'album_not_found', 'tags_disagree', 'not_chosen_edition', 'samples', \
             'too_many_files', 'interrupted', 'import_failed', 'held_by_earlier_version', \
             'target_occupied', 'upgrade_blocked', 'library_changed', 'library_unavailable', \
             'naming_script_failed', 'library_folder_missing', 'local_fault')";
    /// When a carried entry expires: 90 days from now or from ?2, the later.
    fragment HELD_EXPIRY = "MAX(?2, CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)) + 7776000000";
    /// Open a held entry for a carried row (?1) unless its user has one.
    family HELD_ENTRY_INSERTS = held_entry_insert_holding;
    /// Add a carried row's (?1) file (?2), held since ?3, to its entry.
    family HELD_FILE_INSERTS = held_file_insert_holding;
    /// Keep the entry of a carried file (?1) long enough for it.
    pub(super) HELD_EXTEND_ENTRY = "UPDATE main.acquire_held_entries \
             SET expires_at = MAX(expires_at, " + HELD_EXPIRY + ") \
             WHERE id = (SELECT entry_id FROM main.acquire_held_files \
                 WHERE root = 'held' AND path = ?1)";

    /// True when pending link `p` names an item the catalog has. A
    /// tombstone link an earlier import left has nothing to wait for any more.
    fragment LINK_RESOLVED = "CASE p.ref_kind \
             WHEN 'track' THEN EXISTS (SELECT 1 FROM main.local_tracks c WHERE c.id = p.v2_id) \
             WHEN 'album' THEN EXISTS (SELECT 1 FROM main.local_albums c WHERE c.id = p.v2_id) \
             WHEN 'artist' THEN EXISTS (SELECT 1 FROM main.local_artists c WHERE c.id = p.v2_id) \
             WHEN 'tombstone' THEN 1 \
             ELSE 0 END";
    /// How many pending links the catalog now answers.
    pub(super) LINKS_RESOLVABLE = "SELECT COUNT(*) FROM main.import_pending_links p WHERE " + LINK_RESOLVED;
    /// Write the resolved ids into one column that waited for them.
    family LINK_SETTLES = link_settle;
    /// Forget the pending links the catalog now answers.
    pub(super) LINKS_SETTLED = "DELETE FROM main.import_pending_links WHERE rowid IN \
             (SELECT p.rowid FROM main.import_pending_links p WHERE " + LINK_RESOLVED + ")";
    /// How many pending links are left.
    pub(super) LINKS_WAITING = "SELECT COUNT(*) FROM main.import_pending_links";
}

/// Every row a statement with no parameters answers.
pub(super) fn all_rows<T>(
    conn: &Connection,
    statement: Statement,
    read: fn(&Row<'_>) -> rusqlite::Result<T>,
) -> rusqlite::Result<Vec<T>> {
    let mut prepared = conn.prepare(statement.text())?;
    let rows = prepared.query_map([], read)?;
    rows.collect()
}

/// Any carried section.
#[derive(Debug, Clone, Copy)]
pub(super) struct AnySection(pub &'static TableSection);

/// Listed by [`ALL`], so there is no second list to forget.
impl Choice for AnySection {
    fn all() -> Vec<Self> {
        ALL.iter().copied().map(Self).collect()
    }
}

/// A carried section whose rows belong to a user, and the column naming them.
#[derive(Debug, Clone, Copy)]
pub(super) struct UserSection {
    pub section: &'static TableSection,
    pub user: &'static str,
}

impl Choice for UserSection {
    fn all() -> Vec<Self> {
        ALL.iter()
            .copied()
            .filter_map(|section| section.user_column.map(|user| Self { section, user }))
            .collect()
    }
}

/// A column that waits for its library item, with its section's v3 table.
#[derive(Debug, Clone, Copy)]
pub(super) struct WaitingColumn {
    section: &'static TableSection,
    table: &'static str,
    column: &'static Column,
}

impl Choice for WaitingColumn {
    fn all() -> Vec<Self> {
        let mut all = Vec::new();
        for section in ALL.iter().copied() {
            let Target::Table(table) = section.target else {
                continue;
            };
            for column in section.columns {
                if matches!(column.link, Some(link) if link.mode == LinkMode::UntilResolved) {
                    all.push(Self {
                        section,
                        table,
                        column,
                    });
                }
            }
        }
        all
    }
}

/// A section that lands in a v3 table.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy)]
struct TableTarget {
    section: &'static TableSection,
    table: &'static str,
}

#[cfg(any(test, feature = "test-support"))]
impl Choice for TableTarget {
    fn all() -> Vec<Self> {
        ALL.iter()
            .copied()
            .filter_map(|section| match section.target {
                Target::Table(table) => Some(Self { section, table }),
                Target::AvatarFiles | Target::HeldFiles | Target::BundleOnly => None,
            })
            .collect()
    }
}

/// A library reference column of a section that lands in a v3 table.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Clone, Copy)]
struct LinkColumn {
    target: TableTarget,
    column: &'static Column,
}

#[cfg(any(test, feature = "test-support"))]
impl Choice for LinkColumn {
    fn all() -> Vec<Self> {
        let mut all = Vec::new();
        for target in TableTarget::all() {
            for column in target.section.columns {
                if column.link.is_some() {
                    all.push(Self { target, column });
                }
            }
        }
        all
    }
}

#[cfg(any(test, feature = "test-support"))]
fn every_column_count(target: TableTarget, dry_run: bool) -> String {
    let every: Vec<&Column> = target.section.columns.iter().collect();
    section_count(target.section, target.table, &every, dry_run)
}

#[cfg(any(test, feature = "test-support"))]
fn every_column_insert(target: TableTarget, dry_run: bool) -> String {
    let every: Vec<&Column> = target.section.columns.iter().collect();
    section_insert(target.section, target.table, &every, dry_run)
}

#[cfg(any(test, feature = "test-support"))]
fn every_link_insert(link: LinkColumn, dry_run: bool) -> String {
    let LinkColumn { target, column } = link;
    link_insert(target.section, target.table, column, dry_run).unwrap_or_default()
}

#[cfg(any(test, feature = "test-support"))]
fn every_link_count(link: LinkColumn, dry_run: bool) -> String {
    let LinkColumn { target, column } = link;
    link_count(target.section, column, dry_run).unwrap_or_default()
}

#[cfg(any(test, feature = "test-support"))]
fn held_entry_insert_holding(every_column: bool) -> String {
    held_entry_insert(&|_| every_column)
}

#[cfg(any(test, feature = "test-support"))]
fn held_file_insert_holding(every_column: bool) -> String {
    held_file_insert(&|_| every_column)
}

/// How many rows a bundle section holds.
pub(super) fn section_rows(section: AnySection) -> String {
    format!("SELECT COUNT(*) FROM bundle.\"{}\"", section.0.name)
}

/// How many of a bundle section's rows name a user outside the JSON list ?1.
pub(super) fn section_dangling(section: UserSection) -> String {
    format!(
        "SELECT COUNT(*) FROM bundle.\"{name}\" WHERE \"{user}\" IS NOT NULL \
         AND \"{user}\" NOT IN (SELECT value FROM json_each(?1))",
        name = section.section.name,
        user = section.user,
    )
}

fn quote(name: &str) -> String {
    format!("\"{name}\"")
}

/// The v3 catalog table holding one link kind.
fn catalog_table(kind: &str) -> &'static str {
    match kind {
        "album" => "local_albums",
        "artist" => "local_artists",
        _ => "local_tracks",
    }
}

/// SQL for the reference kind of `kind` on bundle row `b`.
fn kind_sql(kind: LinkKind) -> String {
    match kind {
        LinkKind::Track => "'track'".to_owned(),
        LinkKind::Album => "'album'".to_owned(),
        LinkKind::Artist => "'artist'".to_owned(),
        LinkKind::ByColumn(column) => format!("b.{}", quote(column)),
    }
}

/// SQL that is true when the v3 catalog already holds `id`.
fn resolved_sql(kind: LinkKind, id: &str) -> String {
    let exists = |kind: &str| {
        format!(
            "EXISTS (SELECT 1 FROM main.{} c WHERE c.id = {id})",
            catalog_table(kind)
        )
    };
    match kind {
        LinkKind::Track => exists("track"),
        LinkKind::Album => exists("album"),
        LinkKind::Artist => exists("artist"),
        LinkKind::ByColumn(column) => format!(
            "CASE b.{} WHEN 'track' THEN {} WHEN 'album' THEN {} WHEN 'artist' THEN {} ELSE 0 END",
            quote(column),
            exists("track"),
            exists("album"),
            exists("artist"),
        ),
    }
}

/// The value a column takes in v3 for bundle row `b`.
fn value_sql(column: &Column) -> String {
    let raw = format!("b.{}", quote(column.name));
    match column.link {
        Some(link) if link.mode == LinkMode::UntilResolved => {
            format!("CASE WHEN {} THEN {raw} END", resolved_sql(link.kind, &raw))
        }
        _ => raw,
    }
}

fn joined(parts: impl Iterator<Item = String>, separator: &str) -> String {
    parts.collect::<Vec<_>>().join(separator)
}

/// The bundle section that fills one v3 table.
fn section_for(table: &str) -> Option<&'static TableSection> {
    ALL.iter()
        .copied()
        .find(|section| matches!(section.target, Target::Table(name) if name == table))
}

/// SQL that is true when every parent row of bundle row `row` is in v3.
/// On a dry run nothing is written, so a parent also counts when its own
/// bundle row would land.
fn has_parent_sql(section: &TableSection, row: &str, dry_run: bool) -> String {
    if section.parents.is_empty() {
        return "1".to_owned();
    }
    joined(
        section.parents.iter().map(|parent| {
            let matched = |alias: &str| {
                joined(
                    parent.columns.iter().map(|(own, theirs)| {
                        format!("{alias}.{} = {row}.{}", quote(theirs), quote(own))
                    }),
                    " AND ",
                )
            };
            let in_v3 = format!(
                "EXISTS (SELECT 1 FROM main.{} {row}p WHERE {})",
                quote(parent.table),
                matched(&format!("{row}p"))
            );
            let present = match section_for(parent.table).filter(|_| dry_run) {
                Some(source) => {
                    let alias = format!("{row}q");
                    format!(
                        "({in_v3} OR EXISTS (SELECT 1 FROM bundle.{} {alias} WHERE {} AND {}))",
                        quote(source.name),
                        matched(&alias),
                        lands_sql(source, &alias, dry_run)
                    )
                }
                None => in_v3,
            };
            if parent.optional {
                let empty = joined(
                    parent
                        .columns
                        .iter()
                        .map(|(own, _)| format!("{row}.{} IS NULL", quote(own))),
                    " OR ",
                );
                format!("({empty} OR {present})")
            } else {
                present
            }
        }),
        " AND ",
    )
}

/// SQL that is true when bundle row `row` clashes with no v3 row on its
/// key or another unique rule. Unique columns compare with `=`: rows
/// holding a NULL there never clash, as in SQLite.
fn v3_free_sql(section: &TableSection, row: &str, target_table: &str) -> String {
    let key_match = joined(
        section
            .key
            .iter()
            .map(|key| format!("{row}m.{0} IS {row}.{0}", quote(key))),
        " AND ",
    );
    let mut tests = vec![format!(
        "NOT EXISTS (SELECT 1 FROM {target_table} {row}m WHERE {key_match})"
    )];
    for rule in section.unique {
        tests.push(format!(
            "NOT EXISTS (SELECT 1 FROM {target_table} {row}m WHERE {})",
            rule_match(rule, &format!("{row}m"), row)
        ));
    }
    tests.join(" AND ")
}

/// `alias` and `row` hold the same values on every column of `rule`.
fn rule_match(rule: &[&str], alias: &str, row: &str) -> String {
    joined(
        rule.iter()
            .map(|name| format!("{alias}.{0} = {row}.{0}", quote(name))),
        " AND ",
    )
}

/// SQL that is true when bundle row `row` clashes with no v3 row on its
/// key or another unique rule, nor with an earlier bundle row on such a
/// rule that lands itself (the first landing row wins). The exporter
/// indexes every rule's columns in the bundle, so the earlier-row look-up
/// is an index probe, not a scan.
///
/// "Lands itself" checks the earlier row's parents and its clashes with
/// v3, not its own clashes with rows before it: each section has at most
/// one unique rule, so a row before it on that rule shares the same
/// values and clashes with this row too.
fn is_new_sql(section: &TableSection, row: &str, dry_run: bool) -> String {
    let target_table = match section.target {
        Target::Table(table) => format!("main.{}", quote(table)),
        Target::AvatarFiles | Target::HeldFiles | Target::BundleOnly => return "1".to_owned(),
    };
    let mut tests = vec![v3_free_sql(section, row, &target_table)];
    let earlier = format!("{row}e");
    for rule in section.unique {
        tests.push(format!(
            "NOT EXISTS (SELECT 1 FROM bundle.{name} {earlier} WHERE {matched} \
             AND {earlier}.rowid < {row}.rowid AND ({parents}) AND {free})",
            name = quote(section.name),
            matched = rule_match(rule, &earlier, row),
            parents = has_parent_sql(section, &earlier, dry_run),
            free = v3_free_sql(section, &earlier, &target_table),
        ));
    }
    tests.join(" AND ")
}

/// SQL that is true when bundle row `row` lands in v3: its parents are
/// there and it clashes with nothing.
fn lands_sql(section: &TableSection, row: &str, dry_run: bool) -> String {
    format!(
        "({}) AND {}",
        has_parent_sql(section, row, dry_run),
        is_new_sql(section, row, dry_run)
    )
}

fn section_source(section: &TableSection) -> String {
    format!("bundle.{} b", quote(section.name))
}

/// One row, read as [`SectionCounts`]: total, orphans (their parent row is
/// not in v3), new (parent present, no key or uniqueness clash), identical
/// (every column equal in v3). `present` is the columns the bundle holds.
pub(super) fn section_count(
    section: &TableSection,
    table: &str,
    present: &[&Column],
    dry_run: bool,
) -> String {
    let all_match = joined(
        present
            .iter()
            .filter(|column| column.in_target())
            .map(|column| format!("m.{} IS {}", quote(column.name), value_sql(column))),
        " AND ",
    );
    let is_new = is_new_sql(section, "b", dry_run);
    let has_parent = has_parent_sql(section, "b", dry_run);
    let identical = format!(
        "EXISTS (SELECT 1 FROM main.{} m WHERE {all_match})",
        quote(table)
    );
    format!(
        "SELECT COUNT(*) AS total, \
         COALESCE(SUM(CASE WHEN NOT ({identical}) AND NOT ({has_parent}) THEN 1 ELSE 0 END), 0) \
         AS orphans, \
         COALESCE(SUM(CASE WHEN NOT ({identical}) AND ({has_parent}) AND {is_new} \
         THEN 1 ELSE 0 END), 0) AS new, \
         COALESCE(SUM(CASE WHEN {identical} THEN 1 ELSE 0 END), 0) AS identical FROM {source}",
        source = section_source(section),
    )
}

/// The rows of a section that land, for the link statements.
fn pending_where(
    section: &TableSection,
    column: &Column,
    dry_run: bool,
) -> Option<(String, LinkKind, LinkMode)> {
    let link = column.link?;
    let raw = format!("b.{}", quote(column.name));
    let when = if link.when.is_empty() {
        String::new()
    } else {
        format!(" AND ({})", link.when)
    };
    let pending = format!(
        "{raw} IS NOT NULL AND {raw} <> '' AND NOT ({resolved}){when} AND {lands}",
        resolved = resolved_sql(link.kind, &raw),
        lands = lands_sql(section, "b", dry_run),
    );
    Some((pending, link.kind, link.mode))
}

/// Record the unresolved references of a section's new rows in
/// `import_pending_links`. `None` for a column that holds no reference.
pub(super) fn link_insert(
    section: &TableSection,
    table: &str,
    column: &Column,
    dry_run: bool,
) -> Option<String> {
    let (pending, kind, mode) = pending_where(section, column, dry_run)?;
    let key_json = format!(
        "json_array({})",
        joined(
            section.key.iter().map(|key| format!("b.{}", quote(key))),
            ", "
        )
    );
    Some(format!(
        "INSERT OR IGNORE INTO main.import_pending_links \
         (target_table, target_key, ref_column, ref_kind, v2_id, mode) \
         SELECT '{table}', {key_json}, '{name}', {kind}, b.{raw}, '{mode}' FROM {source} \
         WHERE {pending}",
        name = column.name,
        kind = kind_sql(kind),
        raw = quote(column.name),
        mode = mode.as_str(),
        source = section_source(section),
    ))
}

/// [`link_insert`]'s references counted, for dry runs.
pub(super) fn link_count(section: &TableSection, column: &Column, dry_run: bool) -> Option<String> {
    let (pending, ..) = pending_where(section, column, dry_run)?;
    Some(format!(
        "SELECT COUNT(*) FROM {source} WHERE {pending}",
        source = section_source(section),
    ))
}

/// Insert a section's new rows. A plain insert: the new-row test already
/// excludes every key and uniqueness clash, so any other constraint failure
/// is a real error and fails the section.
pub(super) fn section_insert(
    section: &TableSection,
    table: &str,
    present: &[&Column],
    dry_run: bool,
) -> String {
    let target: Vec<&Column> = present
        .iter()
        .copied()
        .filter(|column| column.in_target())
        .collect();
    format!(
        "INSERT INTO main.{target_table} ({cols}) SELECT {values} FROM {source} \
         WHERE {lands} ORDER BY b.rowid",
        target_table = quote(table),
        cols = joined(target.iter().map(|column| quote(column.name)), ", "),
        values = joined(target.iter().map(|column| value_sql(column)), ", "),
        source = section_source(section),
        lands = lands_sql(section, "b", dry_run),
    )
}

/// Write the resolved ids into one column that waited for them.
pub(super) fn link_settle(waiting: WaitingColumn) -> String {
    let WaitingColumn {
        section,
        table,
        column,
    } = waiting;
    let key = joined(
        section.key.iter().map(|column| format!("t.\"{column}\"")),
        ", ",
    );
    let name = column.name;
    let pending = format!(
        "FROM main.import_pending_links p WHERE p.target_table = '{table}' \
         AND p.ref_column = '{name}' AND p.mode = 'until_resolved' \
         AND p.target_key = json_array({key}) AND {LINK_RESOLVED}"
    );
    format!(
        "UPDATE main.\"{table}\" AS t SET \"{name}\" = (SELECT p.v2_id {pending}) \
         WHERE t.\"{name}\" IS NULL AND EXISTS (SELECT 1 {pending})"
    )
}

/// The bundle's pins of albums that landed. An older exporter's bundle may
/// lack who set them.
fn pins_table(has_setter: bool) -> String {
    let setter = if has_setter {
        "set_by_user_id"
    } else {
        "NULL AS set_by_user_id"
    };
    format!(
        "(SELECT local_album_id, release_group_mbid, release_mbid, {setter} \
         FROM bundle.\"album_pin\" WHERE local_album_id IN (SELECT id FROM main.local_albums))"
    )
}

/// Move a pinned album's revisable identity to its pin, chosen by the
/// person who pinned it. Track mappings stay as they are until the files
/// are placed on the pinned release; nothing is wiped up front.
pub(super) fn pin_identity_update(has_setter: bool) -> String {
    let pins = pins_table(has_setter);
    let pin = |column: &str| {
        format!(
            "(SELECT p.\"{column}\" FROM {pins} p \
             WHERE p.local_album_id = e.local_album_id)"
        )
    };
    format!(
        "UPDATE main.local_album_external_identities AS e SET \
         release_group_mbid = {group}, release_mbid = {release}, \
         decision_source = 'manual', selected_by_user_id = (SELECT u.id FROM \
         main.auth_users u WHERE u.id = {user}), row_revision = e.row_revision + 1 \
         WHERE e.local_album_id {CARRIED_ALBUM} AND e.provider = 'musicbrainz' \
         AND e.decision_source IN {REVISABLE} \
         AND EXISTS (SELECT 1 FROM {pins} p \
         WHERE p.local_album_id = e.local_album_id)",
        group = pin("release_group_mbid"),
        release = pin("release_mbid"),
        user = pin("set_by_user_id"),
    )
}

/// Give a pinned album with no identity one from its pin.
pub(super) fn pin_identity_insert(has_setter: bool) -> String {
    format!(
        "INSERT INTO main.local_album_external_identities (local_album_id, provider, \
         release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
         selected_at) \
         SELECT p.local_album_id, 'musicbrainz', p.release_group_mbid, p.release_mbid, \
         'manual', (SELECT u.id FROM main.auth_users u \
         WHERE u.id = p.set_by_user_id), CAST(strftime('%s', 'now') AS INTEGER) * 1000 \
         FROM {pins} p WHERE p.local_album_id {CARRIED_ALBUM} \
         AND NOT EXISTS (SELECT 1 FROM main.local_album_external_identities e \
         WHERE e.local_album_id = p.local_album_id AND e.provider = 'musicbrainz')",
        pins = pins_table(has_setter),
    )
}

/// How many pins name another release than their album's manual identity.
pub(super) fn pin_manual_differs(has_setter: bool) -> String {
    format!(
        "SELECT COUNT(*) FROM {pins} p \
         JOIN main.local_album_external_identities e ON e.local_album_id = p.local_album_id \
         WHERE p.local_album_id {CARRIED_ALBUM} AND e.decision_source = 'manual' \
         AND e.release_mbid IS NOT p.release_mbid",
        pins = pins_table(has_setter),
    )
}

/// Queue the pinned albums whose files the library must place on the pinned
/// release. An album with an active custom edition lists its own tracks and
/// is not queued.
pub(super) fn pin_remap_queue(has_setter: bool) -> String {
    format!(
        "INSERT OR IGNORE INTO main.library_edition_remap_queue (local_album_id, \
         release_mbid, chosen_by_user_id, queued_at, origin) \
         SELECT p.local_album_id, lower(p.release_mbid), \
         (SELECT u.id FROM main.auth_users u WHERE u.id = p.set_by_user_id), \
         {now_ms}, '{place_only}' \
         FROM {pins} p \
         JOIN main.local_album_external_identities e ON e.local_album_id = p.local_album_id \
         WHERE p.local_album_id {CARRIED_ALBUM} AND e.provider = 'musicbrainz' \
         AND e.decision_source = 'manual' AND e.release_mbid = p.release_mbid \
         AND NOT EXISTS (SELECT 1 FROM main.library_custom_edition_active c \
         WHERE c.local_album_id = p.local_album_id)",
        pins = pins_table(has_setter),
        now_ms = now_millis!(),
        place_only = RemapOrigin::PlaceOnly.as_str(),
    )
}

/// A held column as SQL: one the bundle lacks reads as NULL.
fn held_column(has: &dyn Fn(&str) -> bool, column: &str) -> String {
    if has(column) {
        format!("h.\"{column}\"")
    } else {
        "NULL".to_owned()
    }
}

/// The entry of the file's song for a track download, else its album, else
/// its download, by the held list's one rule.
fn held_group_key(has: &dyn Fn(&str) -> bool) -> String {
    let col = |column: &str| held_column(has, column);
    format!(
        "CASE WHEN COALESCE({rec}, '') != '' AND EXISTS (SELECT 1 \
             FROM main.download_tasks t WHERE t.id = {task} \
             AND t.download_type = 'track') THEN 'track:' || lower({rec}) \
         WHEN COALESCE({rg}, '') != '' THEN 'album:' || lower({rg}) \
         ELSE 'task:' || COALESCE(NULLIF({task}, ''), 'v2-held-' || h.id) END",
        rec = col("recording_mbid"),
        rg = col("release_group_mbid"),
        task = col("source_task_id"),
    )
}

/// Open a held entry for a carried row (?1), held since ?2, unless its user
/// has one open. `has` says which held columns the bundle holds.
pub(super) fn held_entry_insert(has: &dyn Fn(&str) -> bool) -> String {
    let col = |column: &str| held_column(has, column);
    let key = held_group_key(has);
    format!(
        "INSERT INTO main.acquire_held_entries (user_id, origin, group_key, \
             release_group_mbid, recording_mbid, artist_name, album_title, state, \
             created_at, expires_at) \
         SELECT h.user_id, 'download', {key}, lower(NULLIF({rg}, '')), \
             CASE WHEN {key} LIKE 'track:%' THEN substr({key}, 7) END, {artist}, {album}, \
             'held', ?2, {HELD_EXPIRY} \
         FROM bundle.\"held_import\" h WHERE h.id = ?1 \
           AND NOT EXISTS (SELECT 1 FROM main.acquire_held_entries e \
             WHERE e.user_id = h.user_id AND e.group_key = {key} \
               AND e.state IN ('held', 'placing'))",
        rg = col("release_group_mbid"),
        artist = col("artist_name"),
        album = col("album_title"),
    )
}

/// Add a carried row's (?1) file (?2), held since ?3, to its entry. `has`
/// says which held columns the bundle holds.
pub(super) fn held_file_insert(has: &dyn Fn(&str) -> bool) -> String {
    let col = |column: &str| held_column(has, column);
    format!(
        "INSERT INTO main.acquire_held_files (entry_id, root, path, original_name, \
             release_mbid, release_track_mbid, recording_mbid, disc_number, track_number, \
             track_title, file_format, duration_seconds, expected_duration_seconds, \
             evidence_title, evidence_artist, evidence_score, source, source_task_id, \
             reason, reason_detail, state, created_at) \
         SELECT e.id, 'held', ?2, COALESCE(NULLIF({original}, ''), ?2), {release}, \
             {release_track}, lower({recording}), {disc}, {track}, {title}, {format}, \
             {duration}, {expected}, {evidence_title}, {evidence_artist}, {evidence_score}, \
             COALESCE({source}, 'soulseek'), {task}, \
             CASE WHEN h.reason LIKE 'management:%' THEN 'target_occupied' \
                 WHEN h.reason IN {KEPT_REASONS} THEN h.reason \
                 ELSE 'held_by_earlier_version' END, \
             {detail}, 'held', ?3 \
         FROM bundle.\"held_import\" h \
         JOIN main.acquire_held_entries e ON e.user_id = h.user_id \
           AND e.group_key = {key} AND e.state IN ('held', 'placing') \
         WHERE h.id = ?1",
        key = held_group_key(has),
        original = col("original_filename"),
        release = col("release_mbid"),
        release_track = col("release_track_mbid"),
        recording = col("recording_mbid"),
        disc = col("disc_number"),
        track = col("track_number"),
        title = col("track_title"),
        format = col("file_format"),
        duration = col("duration_seconds"),
        expected = col("expected_duration_seconds"),
        evidence_title = col("evidence_title"),
        evidence_artist = col("evidence_artist"),
        evidence_score = col("evidence_score"),
        source = col("source"),
        task = col("source_task_id"),
        detail = col("reason_detail"),
    )
}
