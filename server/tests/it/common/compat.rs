//! The golden files of the compat journeys.

/// Whether this run rewrites the goldens, from the values of `COMPAT_BLESS`
/// and `CI`. A CI run never rewrites one: with both set the answer is the
/// refusal, so a bless can neither pass as a compare nor commit itself.
pub fn bless_requested(compat_bless: Option<&str>, ci: Option<&str>) -> Result<bool, String> {
    let blessing = compat_bless.is_some();
    let in_ci = ci.is_some_and(|value| !value.is_empty());
    if blessing && in_ci {
        return Err(
            "COMPAT_BLESS and CI are both set: goldens are never rewritten in CI. \
             Unset COMPAT_BLESS here, or bless on a workstation and commit the diff."
                .to_owned(),
        );
    }
    Ok(blessing)
}

/// [`bless_requested`] for this process. Panics on the refusal.
pub fn blessing() -> bool {
    let read = |name: &str| std::env::var_os(name).map(|v| v.to_string_lossy().into_owned());
    match bless_requested(read("COMPAT_BLESS").as_deref(), read("CI").as_deref()) {
        Ok(blessing) => blessing,
        Err(refusal) => panic!("{refusal}"),
    }
}
