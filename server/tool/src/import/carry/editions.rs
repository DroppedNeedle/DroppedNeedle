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
//!   The album is queued so the library places its files on the pinned
//!   release once MusicBrainz is reachable; until then its track mappings
//!   stay as they were. The pins stay in the bundle: the identity row is
//!   the album's one record of its edition;
//! - an active custom edition: the album's identity becomes the custom
//!   edition's release group with no exact release, as v2 sealed it, and
//!   is protected the same way.
//!
//! The step reports what it did as notes, so a dry run and a real run
//! count the same.

use rusqlite::{Connection, params};

use super::bundle;
use super::{CarryError, SectionResult};
use crate::export::sections::TableSection;
use crate::export::sections::library::{ALBUM_PINS, ALBUMS};
use crate::export::sections::management::EDITION_ACTIVE;
use crate::import::sql::{self, AnySection};

/// Report entity and section marker name.
pub(crate) const ENTITY: &str = "sticky_edition";

fn bundle_rows(conn: &Connection, section: &'static TableSection) -> rusqlite::Result<i64> {
    let present: bool =
        conn.query_row(sql::BUNDLE_HAS_TABLE.text(), params![section.name], |row| {
            row.get(0)
        })?;
    if !present {
        return Ok(0);
    }
    conn.query_row(&sql::section_rows(AnySection(section)), [], |row| {
        row.get(0)
    })
}

/// Make every carried edition choice sticky. On a dry run, count only.
pub(crate) fn apply(conn: &Connection, dry_run: bool) -> Result<SectionResult, CarryError> {
    let mut result = SectionResult::default();
    if bundle_rows(conn, &ALBUMS)? == 0 {
        return Ok(result);
    }
    let pins = bundle_rows(conn, &ALBUM_PINS)?;
    let customs = bundle_rows(conn, &EDITION_ACTIVE)?;
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
    let has_setter = bundle::present_columns(conn, &ALBUM_PINS)?
        .iter()
        .any(|column| column.name == "set_by_user_id");
    let mut sticky = conn.execute(&sql::pin_identity_update(has_setter), [])?;
    sticky += conn.execute(&sql::pin_identity_insert(has_setter), [])?;
    sticky += conn.execute(sql::CUSTOM_IDENTITY_UPDATE.text(), [])?;
    sticky += conn.execute(sql::CUSTOM_IDENTITY_INSERT.text(), [])?;
    result.written = sticky as u64;
    result.note(
        String::new(),
        "pinned",
        format!(
            "{sticky} album identity row(s) now hold the edition chosen in v2 and are \
             protected from automatic identification"
        ),
    );
    let manual_differs: i64 =
        conn.query_row(&sql::pin_manual_differs(has_setter), [], |row| row.get(0))?;
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
    // library does that when MusicBrainz is reachable, and changes nothing
    // else (who chose it, a carried "leave unmanaged").
    conn.execute(&sql::pin_remap_queue(has_setter), [])?;
    Ok(result)
}
