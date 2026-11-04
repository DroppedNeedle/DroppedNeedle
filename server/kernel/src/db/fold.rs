//! Text folding for accent- and case-insensitive matching, so a keyboard
//! that cannot type an accent still finds the artist. Searches fold in Rust
//! with [`fold_text`]; no SQL statement folds.

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

#[cfg(test)]
mod tests {
    use super::*;

    /// Folding keys names in matching and the v2 artist keys, so it must
    /// agree with v2 step for step.
    #[test]
    fn fold_matches_v2_casefold() {
        for (input, folded) in [
            ("Beyoncé", "beyonce"),
            ("  Sigur   Rós  ", "sigur ros"),
            ("Æther", "æther"),
            ("ﬁsh", "fish"),
            ("", ""),
            ("Straße", "strasse"),
            ("ς", "σ"),
            ("Σ", "σ"),
            ("Οδυσσέας", "οδυσσεασ"),
            ("İ", "i"),
            ("ﬁŒß", "fiœss"),
        ] {
            assert_eq!(fold_text(input), folded, "{input:?}");
        }
    }
}
