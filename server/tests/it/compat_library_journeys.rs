//! Compat client journeys through the whole app.

use crate::common::compat::bless_requested;

#[test]
fn bless_is_refused_in_ci() {
    assert_eq!(bless_requested(None, None), Ok(false));
    assert_eq!(bless_requested(None, Some("true")), Ok(false));
    assert_eq!(bless_requested(Some("1"), None), Ok(true));
    assert_eq!(bless_requested(Some("1"), Some("")), Ok(true));
    let refusal = bless_requested(Some("1"), Some("true")).expect_err("refused");
    assert!(
        refusal.contains("COMPAT_BLESS") && refusal.contains("CI"),
        "{refusal}"
    );
}
