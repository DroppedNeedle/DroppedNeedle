//! The `fold()` search function: accent- and case-insensitive matching.
//!
//! v2 registers a deterministic `fold()` SQL function on every connection and
//! applies it to both column and pattern in library searches, so a keyboard
//! that cannot type an accent still finds the artist. The v3 baseline schema
//! keeps no SQL-side `fold()` calls, so only the writer connection registers
//! it today; the sqlx reader pool cannot register scalar functions, and the
//! checkpoint and backup connections never run text searches. Application-side
//! matching uses [`fold_text`] directly.

use unicode_normalization::UnicodeNormalization;

/// Fold text for forgiving search: NFKD, strip combining marks, Unicode
/// default casefold, collapse whitespace. Empty stays empty.
///
/// Matches v2's `_fold_text`, same order of steps, so the shapes agree:
/// `Straße` folds to `strasse`, final sigma `ς` to `σ`, and dotted capital
/// `İ` (decomposed, dot stripped, then folded) to `i`.
pub fn fold_text(value: &str) -> String {
    let stripped: String = value
        .nfkd()
        .filter(|character| !unicode_normalization::char::is_combining_mark(*character))
        .collect();
    let folded = caseless::default_case_fold_str(&stripped);
    folded.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Register deterministic `fold()` on a rusqlite connection. NULL and
/// non-text values pass through unchanged so surrounding predicates keep
/// their normal semantics.
pub fn register_fold(connection: &rusqlite::Connection) -> rusqlite::Result<()> {
    use rusqlite::functions::FunctionFlags;

    connection.create_scalar_function(
        "fold",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |context| {
            let value = context.get_raw(0);
            match value {
                rusqlite::types::ValueRef::Text(text) => {
                    let input = String::from_utf8_lossy(text);
                    Ok(rusqlite::types::Value::Text(fold_text(&input)))
                }
                other => Ok(rusqlite::types::Value::from(other)),
            }
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_matches_v1_semantics() {
        assert_eq!(fold_text("Beyoncé"), "beyonce");
        assert_eq!(fold_text("  Sigur   Rós  "), "sigur ros");
        assert_eq!(fold_text("Æther"), "æther");
        assert_eq!(fold_text("ﬁsh"), "fish");
        assert_eq!(fold_text(""), "");
    }

    #[test]
    fn fold_matches_v2_casefold() {
        assert_eq!(fold_text("Straße"), "strasse");
        assert_eq!(fold_text("ς"), "σ");
        assert_eq!(fold_text("Σ"), "σ");
        assert_eq!(fold_text("Οδυσσέας"), "οδυσσεασ");
        assert_eq!(fold_text("İ"), "i");
        assert_eq!(fold_text("ﬁŒß"), "fiœss");
    }

    #[test]
    fn fold_registers_on_rusqlite_connections() {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        register_fold(&connection).unwrap();
        let folded: String = connection
            .query_row("SELECT fold('Crème Brûlée')", [], |row| row.get(0))
            .unwrap();
        assert_eq!(folded, "creme brulee");
        let null: Option<String> = connection
            .query_row("SELECT fold(NULL)", [], |row| row.get(0))
            .unwrap();
        assert_eq!(null, None);
    }
}
