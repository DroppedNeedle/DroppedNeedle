//! UTC ISO-8601 conversion, and the auth tables' millisecond times.
//!
//! v2 wrote its times as ISO-8601 with an explicit offset
//! (`datetime.now(timezone.utc).isoformat()`). The auth tables store integer
//! milliseconds; the v2 importer converts with [`parse_iso_ms`]. Reads
//! accept the v2 fractional form plus `Z` and numeric offsets. Years
//! 0000-9999 round-trip; anything else fails closed to `None`.

use time::format_description::BorrowedFormatItem;
use time::macros::format_description;
use time::parsing::Parsed;
use time::{OffsetDateTime, PrimitiveDateTime, UtcOffset};

/// The first and last second [`to_iso`] can write: years 0000-9999.
const FIRST_SECOND: i64 = -62_167_219_200;
const LAST_SECOND: i64 = 253_402_300_799;

/// `YYYY-MM-DDTHH:MM:SS[.fraction]`, then `Z`, `+HH:MM`, `+HHMM`, `+HH` or
/// nothing meaning UTC. A space may stand in for the `T`.
const ISO_TEXT: &[BorrowedFormatItem<'_>] = format_description!(
    version = 2,
    "[year]-[month]-[day][first [T] [t] [ ]][hour]:[minute]:[second]\
     [optional [.[subsecond digits:1+]]]\
     [optional [[first [Z] [z] [[offset_hour sign:mandatory][optional [:]][optional [[offset_minute]]]]]]]"
);

/// Format unix epoch seconds as UTC `YYYY-MM-DDTHH:MM:SS+00:00`. A time
/// outside years 0000-9999 is written as the nearest one inside them.
pub fn to_iso(secs: i64) -> String {
    let at = OffsetDateTime::from_unix_timestamp(secs.clamp(FIRST_SECOND, LAST_SECOND))
        .unwrap_or(OffsetDateTime::UNIX_EPOCH);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}+00:00",
        at.year(),
        u8::from(at.month()),
        at.day(),
        at.hour(),
        at.minute(),
        at.second()
    )
}

/// Unix seconds as the auth tables' integer milliseconds.
pub fn secs_to_ms(secs: i64) -> i64 {
    secs.saturating_mul(1_000)
}

/// The auth tables' integer milliseconds as whole unix seconds, rounded
/// down, as the ports carry them.
pub fn ms_to_secs(ms: i64) -> i64 {
    ms.div_euclid(1_000)
}

/// Parse v2/v3 ISO-8601 text into unix epoch seconds, truncating fractions.
pub fn parse_iso(raw: &str) -> Option<i64> {
    parse_iso_ms(raw).map(ms_to_secs)
}

/// Parse v2/v3 ISO-8601 text ([`ISO_TEXT`]) into unix epoch milliseconds,
/// keeping the first three fraction digits and truncating the rest. Returns
/// `None` for anything else, including impossible calendar dates, a signed
/// year and an offset of 24 hours or more.
pub fn parse_iso_ms(raw: &str) -> Option<i64> {
    let text = raw.trim();
    if !text.starts_with(|first: char| first.is_ascii_digit()) {
        return None;
    }
    let mut parsed = Parsed::new();
    if !parsed
        .parse_items(text.as_bytes(), ISO_TEXT)
        .ok()?
        .is_empty()
    {
        return None;
    }
    let local = PrimitiveDateTime::try_from(parsed).ok()?;
    let offset = UtcOffset::try_from(parsed).unwrap_or(UtcOffset::UTC);
    if offset.whole_hours().abs() > 23 {
        return None;
    }
    let at = local.assume_offset(offset);
    Some(at.unix_timestamp() * 1_000 + i64::from(at.millisecond()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_instants_format_exactly() {
        assert_eq!(to_iso(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(to_iso(-1), "1969-12-31T23:59:59+00:00");
        assert_eq!(to_iso(1_700_000_000), "2023-11-14T22:13:20+00:00");
        assert_eq!(to_iso(1_709_164_800), "2024-02-29T00:00:00+00:00");
        assert_eq!(to_iso(4_102_444_800), "2100-01-01T00:00:00+00:00");
    }

    #[test]
    fn v2_fractional_and_offset_forms_parse() {
        assert_eq!(
            parse_iso("2026-09-28T12:34:56.123456+00:00"),
            parse_iso("2026-09-28T12:34:56+00:00")
        );
        assert_eq!(
            parse_iso("2026-09-28T12:34:56Z"),
            parse_iso("2026-09-28T12:34:56+00:00")
        );
        assert_eq!(
            parse_iso("2026-09-28T14:34:56+02:00"),
            parse_iso("2026-09-28T12:34:56+00:00")
        );
        assert_eq!(
            parse_iso("2026-09-28 12:34:56"),
            parse_iso("2026-09-28T12:34:56+00:00")
        );
        assert_eq!(
            parse_iso("2026-09-28T10:04:56-02:30"),
            parse_iso("2026-09-28T12:34:56+00:00")
        );
    }

    #[test]
    fn milliseconds_keep_three_fraction_digits() {
        let whole = parse_iso_ms("2026-09-28T12:34:56+00:00").unwrap();
        assert_eq!(whole % 1_000, 0);
        assert_eq!(
            parse_iso_ms("2026-09-28T12:34:56.123456+00:00"),
            Some(whole + 123)
        );
        assert_eq!(parse_iso_ms("2026-09-28T12:34:56.5Z"), Some(whole + 500));
        assert_eq!(parse_iso_ms("1969-12-31T23:59:59.25Z"), Some(-750));
        assert_eq!(parse_iso("1969-12-31T23:59:59.25Z"), Some(-1));
    }

    #[test]
    fn round_trip_covers_edges() {
        for secs in [
            -62_167_219_200, // 0000-01-01T00:00:00+00:00, the range floor
            -86_400,
            -1,
            0,
            1,
            1_700_000_000,
            4_102_444_800,
            253_402_300_799, // 9999-12-31T23:59:59+00:00, the range ceiling
        ] {
            assert_eq!(parse_iso(&to_iso(secs)), Some(secs), "{secs}");
        }
    }

    #[test]
    fn garbage_fails_closed() {
        for raw in [
            "",
            "abc",
            "2026-01-01",
            "2026-13-01T00:00:00+00:00",
            "2026-00-10T00:00:00+00:00",
            "2026-02-30T00:00:00+00:00",
            "2023-02-29T00:00:00+00:00",
            "2026-01-01T24:00:00+00:00",
            "2026-01-01T00:00:60+00:00",
            "2026-01-01T00:00:00.",
            "2026-01-01T00:00:00+25:00",
            "2026-01-01T00:00:00+00:00x",
            "10000-01-01T00:00:00+00:00",
            "+2026-01-01T00:00:00+00:00",
        ] {
            assert_eq!(parse_iso(raw), None, "{raw:?}");
        }
        // Trailing space trims clean, so this one parses.
        assert!(parse_iso("  2026-01-01T00:00:00+00:00  ").is_some());
    }
}
