//! Edits checked before a save, then resolved to their native homes in
//! the file's tag by its format.

use lofty::tag::items::Timestamp;

use super::{Refusal, TagEdit, TagFormat};
use crate::tags::fields::FieldKind;

// ---------------------------------------------------------------------------
// Pre-save scan: values each field can hold.
// ---------------------------------------------------------------------------

/// True when the field can hold these values (dates parse, counts are
/// numbers, single-valued fields have one value, nothing is blank).
#[must_use]
pub fn accepts(edit: &TagEdit) -> bool {
    check_edit(edit).is_ok()
}

pub(super) fn check_edit(edit: &TagEdit) -> Result<(), Refusal> {
    if edit.verbatim {
        return Ok(());
    }
    let unencodable = |reason: String| Refusal::UnencodableItem {
        field: edit.field,
        reason,
    };
    if edit.values.iter().any(|value| value.is_empty()) {
        return Err(unencodable("empty values are not expressible".to_owned()));
    }
    let kind = edit.field.kind();
    if kind != FieldKind::Text && edit.values.len() > 1 {
        return Err(unencodable("the field holds one value".to_owned()));
    }
    for value in &edit.values {
        match kind {
            FieldKind::Count if value.trim().parse::<u32>().is_err() => {
                return Err(unencodable(format!(
                    "non-numeric count '{value}' would be dropped"
                )));
            }
            FieldKind::Flag if value != "0" && value != "1" => {
                return Err(unencodable(format!("flag '{value}' is not 1 or 0")));
            }
            FieldKind::Date if value.parse::<Timestamp>().is_err() => {
                return Err(unencodable(format!(
                    "unparseable timestamp '{value}' would poison ID3v2 reads"
                )));
            }
            FieldKind::Gain
                if value
                    .strip_suffix(" dB")
                    .and_then(|number| number.parse::<f64>().ok())
                    .is_none_or(|number| !number.is_finite()) =>
            {
                return Err(unencodable(format!("gain '{value}' is not '<number> dB'")));
            }
            FieldKind::Peak
                if value
                    .parse::<f64>()
                    .ok()
                    .is_none_or(|number| !number.is_finite() || number < 0.0) =>
            {
                return Err(unencodable(format!("peak '{value}' is not a number")));
            }
            _ => {}
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Edits resolved to their native homes in this file's tag.
// ---------------------------------------------------------------------------

/// Resolve edits against the file's own spellings (an existing `TXXX`
/// description, every Vorbis spelling of a total the file uses, a
/// freeform name's case), fold track and disc halves into one pair with
/// the half that was not edited, split a v2.3 date over its three
/// frames, and let a later edit of the same target win.
pub(super) fn resolve_edits<F: TagFormat>(
    truth: Option<&F::Truth>,
    edits: &[TagEdit],
) -> Result<Vec<F::Edit>, Refusal> {
    let fields: Vec<TagEdit> = edits
        .iter()
        .filter(|edit| edit.native.is_none())
        .cloned()
        .collect();
    let mut resolved = F::resolve_fields(truth, &fields)?;
    for edit in edits {
        let Some(key) = &edit.native else {
            continue;
        };
        let native = F::resolve_native(truth, key, &edit.values, &resolved)?;
        supersede::<F>(&mut resolved, native);
    }
    Ok(resolved)
}

/// Add an edit, dropping any earlier one that writes the same home.
pub(super) fn supersede<F: TagFormat>(resolved: &mut Vec<F::Edit>, edit: F::Edit) {
    resolved.retain(|known| !F::same_target(known, &edit));
    resolved.push(edit);
}

/// A native key that names another format's item.
pub(super) fn foreign(key: &str) -> Refusal {
    Refusal::NoByteInventory {
        detail: format!("tag '{key}' does not belong to this file's format"),
    }
}
