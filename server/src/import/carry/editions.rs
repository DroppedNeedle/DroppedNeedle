//! The edition step: every edition a curator chose in v2 becomes the
//! album's sticky edition in v3, so automatic identification never
//! replaces it.
//!
//! v3 keeps an album's edition in its accepted identity, and only
//! automatic identities may be overwritten by an automatic pass. A v2
//! manual identity already lands as `manual`. This step covers the other
//! two ways v2 recorded a choice, for carried albums only:
//!
//! - an edition pin: the album's identity becomes the pinned release
//!   group and release, chosen by the person who pinned it (`manual`). A
//!   manual identity is the curator's own later word and is left alone.
//!   When the pinned release differs from the identity the album had, that
//!   identity's automatic track mappings belonged to another release and
//!   go; the album is queued so the library places its files on the pinned
//!   release once MusicBrainz is reachable. The pin rows are then removed:
//!   the identity row is the album's one record of its edition;
//! - an active custom edition: the album's identity becomes the custom
//!   edition's release group with no exact release, as v2 sealed it, and
//!   is protected the same way.
//!
//! The step reports what it did as notes, so a dry run and a real run
//! count the same.

use sqlx::SqliteConnection;

use super::bundle::SCHEMA;
use super::{CarryError, SectionResult};
use crate::export::sections::library::{ALBUM_PINS, ALBUMS};
use crate::export::sections::management::EDITION_ACTIVE;

/// Report entity and section marker name.
pub(crate) const ENTITY: &str = "sticky_edition";

/// Identities an automatic pass may overwrite.
const REVISABLE: &str = "('automatic', 'embedded')";

async fn bundle_rows(conn: &mut SqliteConnection, table: &str) -> Result<i64, sqlx::Error> {
    let present: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS(SELECT 1 FROM {SCHEMA}.sqlite_master WHERE type = 'table' AND name = ?1)"
    ))
    .bind(table)
    .persistent(false)
    .fetch_one(&mut *conn)
    .await?;
    if !present {
        return Ok(0);
    }
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {SCHEMA}.\"{table}\""))
        .persistent(false)
        .fetch_one(&mut *conn)
        .await
}

async fn run(conn: &mut SqliteConnection, sql: &str) -> Result<u64, CarryError> {
    Ok(sqlx::query(sql)
        .persistent(false)
        .execute(&mut *conn)
        .await?
        .rows_affected())
}

/// Make every carried edition choice sticky. On a dry run, count only.
pub(crate) async fn apply(
    conn: &mut SqliteConnection,
    dry_run: bool,
) -> Result<SectionResult, CarryError> {
    let mut result = SectionResult::default();
    if bundle_rows(conn, ALBUMS.name).await? == 0 {
        return Ok(result);
    }
    let pins = bundle_rows(conn, ALBUM_PINS.name).await?;
    let customs = bundle_rows(conn, EDITION_ACTIVE.name).await?;
    result.rows = u64::try_from(pins + customs).unwrap_or(0);
    if result.rows == 0 {
        return Ok(result);
    }
    if dry_run {
        result.note(
            String::new(),
            "would_pin",
            format!(
                "{pins} edition pin(s) and {customs} custom edition(s) would become their \
                 album's sticky edition"
            ),
        );
        return Ok(result);
    }
    let carried = format!("IN (SELECT id FROM {SCHEMA}.\"{}\")", ALBUMS.name);
    let pin = |column: &str| {
        format!(
            "(SELECT p.\"{column}\" FROM main.library_album_release_pins p \
             WHERE p.local_album_id = e.local_album_id)"
        )
    };
    let pinned = format!(
        "e.local_album_id {carried} AND e.provider = 'musicbrainz' \
         AND e.decision_source IN {REVISABLE} \
         AND EXISTS (SELECT 1 FROM main.library_album_release_pins p \
         WHERE p.local_album_id = e.local_album_id)"
    );
    // Automatic track mappings made for a release the pin replaces.
    run(
        conn,
        &format!(
            "DELETE FROM main.local_track_external_identities \
             WHERE decision_source IN {REVISABLE} AND local_track_id IN \
             (SELECT t.id FROM main.local_tracks t \
              JOIN main.local_album_external_identities e ON e.local_album_id = t.local_album_id \
              WHERE {pinned} AND e.release_mbid IS NOT {release})",
            release = pin("release_mbid"),
        ),
    )
    .await?;
    let mut sticky = run(
        conn,
        &format!(
            "UPDATE main.local_album_external_identities AS e SET \
             release_group_mbid = {group}, release_mbid = {release}, \
             decision_source = 'manual', selected_by_user_id = (SELECT u.id FROM \
             main.auth_users u WHERE u.id = {user}), row_revision = e.row_revision + 1 \
             WHERE {pinned}",
            group = pin("release_group_mbid"),
            release = pin("release_mbid"),
            user = pin("set_by_user_id"),
        ),
    )
    .await?;
    sticky += run(
        conn,
        &format!(
            "INSERT INTO main.local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
             selected_at) \
             SELECT p.local_album_id, 'musicbrainz', p.release_group_mbid, p.release_mbid, \
             'manual', (SELECT u.id FROM main.auth_users u \
             WHERE u.id = p.set_by_user_id), CAST(strftime('%s', 'now') AS REAL) \
             FROM main.library_album_release_pins p WHERE p.local_album_id {carried} \
             AND NOT EXISTS (SELECT 1 FROM main.local_album_external_identities e \
             WHERE e.local_album_id = p.local_album_id AND e.provider = 'musicbrainz')"
        ),
    )
    .await?;
    let manifest = |column: &str| {
        format!(
            "(SELECT m.\"{column}\" FROM main.library_custom_edition_active a \
             JOIN main.library_custom_edition_manifests m ON m.id = a.manifest_id \
             WHERE a.local_album_id = e.local_album_id)"
        )
    };
    sticky += run(
        conn,
        &format!(
            "UPDATE main.local_album_external_identities AS e SET \
             release_group_mbid = {group}, release_mbid = NULL, \
             decision_source = 'legacy_import', row_revision = e.row_revision + 1 \
             WHERE e.local_album_id {carried} AND e.provider = 'musicbrainz' \
             AND e.decision_source IN {REVISABLE} AND {group} IS NOT NULL",
            group = manifest("release_group_mbid"),
        ),
    )
    .await?;
    sticky += run(
        conn,
        &format!(
            "INSERT INTO main.local_album_external_identities (local_album_id, provider, \
             release_group_mbid, release_mbid, decision_source, selected_by_user_id, \
             selected_at) \
             SELECT a.local_album_id, 'musicbrainz', m.release_group_mbid, NULL, \
             'legacy_import', (SELECT u.id FROM main.auth_users u \
             WHERE u.id = m.sealed_by_user_id), m.sealed_at \
             FROM main.library_custom_edition_active a \
             JOIN main.library_custom_edition_manifests m ON m.id = a.manifest_id \
             WHERE a.local_album_id {carried} \
             AND NOT EXISTS (SELECT 1 FROM main.local_album_external_identities e \
             WHERE e.local_album_id = a.local_album_id AND e.provider = 'musicbrainz')"
        ),
    )
    .await?;
    result.written = sticky;
    result.note(
        String::new(),
        "pinned",
        format!(
            "{sticky} album identity row(s) now hold the edition chosen in v2 and are \
             protected from automatic identification"
        ),
    );
    let manual_differs: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM main.library_album_release_pins p \
         JOIN main.local_album_external_identities e ON e.local_album_id = p.local_album_id \
         WHERE p.local_album_id {carried} AND e.decision_source = 'manual' \
         AND e.release_mbid IS NOT p.release_mbid"
    ))
    .persistent(false)
    .fetch_one(&mut *conn)
    .await?;
    if manual_differs > 0 {
        result.note(
            String::new(),
            "kept_manual",
            format!(
                "{manual_differs} album(s) had a pin naming another release than their \
                 manual match; the manual match is kept. To use the pinned edition, pick it \
                 on the album page"
            ),
        );
    }
    // The chosen release still needs the files placed on its tracks; the
    // library does that when MusicBrainz is reachable. The pin rows have
    // done their job: the identity row is the album's edition now.
    run(
        conn,
        &format!(
            "INSERT OR IGNORE INTO main.library_edition_remap_queue (local_album_id, \
             release_mbid, chosen_by_user_id, queued_at) \
             SELECT p.local_album_id, lower(p.release_mbid), \
             (SELECT u.id FROM main.auth_users u WHERE u.id = p.set_by_user_id), \
             CAST(strftime('%s', 'now') AS REAL) \
             FROM main.library_album_release_pins p \
             JOIN main.local_album_external_identities e ON e.local_album_id = p.local_album_id \
             WHERE p.local_album_id {carried} AND e.provider = 'musicbrainz' \
             AND e.decision_source = 'manual' AND e.release_mbid = p.release_mbid"
        ),
    )
    .await?;
    run(conn, "DELETE FROM main.library_album_release_pins").await?;
    run(conn, "DELETE FROM main.album_release_pins").await?;
    Ok(result)
}
