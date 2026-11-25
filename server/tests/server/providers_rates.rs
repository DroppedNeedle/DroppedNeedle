//! The providers' rate table, held to the kernel's MusicBrainz constants
//! and to the table in the contract. The text-only provider rules are in
//! the gates crate's `providers_gate`.

/// The kernel cannot read the providers' rate table, so its MusicBrainz
/// rates are held equal to the rows here.
#[test]
fn kernel_musicbrainz_rates_match_the_table() {
    use droppedneedle_kernel::runtime_config::sections::{
        BRAINZMASH_RATE_LIMIT, OFFICIAL_MB_RATE_LIMIT,
    };
    use droppedneedle_providers::rates::{Rate, row};

    let fixed = |source: &str| row(source).map(|row| row.per_second);
    assert_eq!(
        fixed("brainzmash"),
        Some(Rate::Fixed(BRAINZMASH_RATE_LIMIT))
    );
    assert_eq!(
        fixed("musicbrainz"),
        Some(Rate::Fixed(OFFICIAL_MB_RATE_LIMIT))
    );
}

/// `CLAUDE.md` lists every rate row with the day it was last read, and only
/// rows that exist.
#[test]
fn the_contract_lists_every_rate_with_its_date() {
    use droppedneedle_providers::rates::RATES;

    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../CLAUDE.md");
    let contract = std::fs::read_to_string(path).expect("CLAUDE.md is at the repo root");
    let listed: Vec<&str> = contract
        .lines()
        .map(str::trim_start)
        .filter(|line| line.starts_with("| `"))
        .filter(|line| line.ends_with(" |") && line.matches('|').count() == 5)
        .collect();
    for rate in RATES {
        let row = format!("| `{}` |", rate.source);
        let date = format!("| {} |", rate.verified_on);
        assert!(
            listed
                .iter()
                .any(|line| line.starts_with(&row) && line.ends_with(&date)),
            "CLAUDE.md has no row for {} read on {}",
            rate.source,
            rate.verified_on
        );
    }
    assert_eq!(listed.len(), RATES.len(), "CLAUDE.md lists other rate rows");
}
