//! UTC ISO-8601 conversion for the auth TEXT time columns.
//!
//! The 0001 baseline keeps v2's TEXT time columns, and v2 wrote them as
//! ISO-8601 with an explicit offset (`datetime.now(timezone.utc).isoformat()`).
//! The stage-11 importer carries those values verbatim, so the v3 adapters read
//! and write the same shape. Writes use whole seconds (`...+00:00`); reads
//! accept the v2 fractional form plus `Z` and numeric offsets, truncating
//! sub-second precision. Years 0000-9999 round-trip; anything else fails
//! closed to `None`. No datetime crate is needed: the civil-date math below is
//! the standard days-from-civil pair.

/// Seconds per day.
const DAY_SECS: i64 = 86_400;

/// Format unix epoch seconds as UTC `YYYY-MM-DDTHH:MM:SS+00:00`.
pub fn to_iso(secs: i64) -> String {
    let days = secs.div_euclid(DAY_SECS);
    let rest = secs.rem_euclid(DAY_SECS);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}+00:00",
        rest / 3_600,
        (rest % 3_600) / 60,
        rest % 60
    )
}

/// Parse v2/v3 ISO-8601 text into unix epoch seconds, truncating fractions.
///
/// Accepts `YYYY-MM-DDTHH:MM:SS` with an optional `.fraction` and an optional
/// offset (`Z`, `+HH:MM`, `+HHMM`, `+HH`, or nothing meaning UTC). A space may
/// stand in for the `T`. Returns `None` for anything else, including
/// impossible calendar dates and out-of-range fields.
pub fn parse_iso(raw: &str) -> Option<i64> {
    let bytes = raw.trim().as_bytes();
    if bytes.len() < 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !matches!(bytes[10], b'T' | b't' | b' ')
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year = digits(&bytes[0..4])?;
    let month = digits(&bytes[5..7])?;
    let day = digits(&bytes[8..10])?;
    let hour = digits(&bytes[11..13])?;
    let minute = digits(&bytes[14..16])?;
    let second = digits(&bytes[17..19])?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    // Reject impossible dates (Feb 30 and friends) via a round trip.
    let days = days_from_civil(year, month, day);
    let (back_y, back_m, back_d) = civil_from_days(days);
    if back_y != year || i64::from(back_m) != month || i64::from(back_d) != day {
        return None;
    }
    let mut rest = &bytes[19..];
    if rest.first() == Some(&b'.') {
        let frac_len = rest[1..].iter().take_while(|b| b.is_ascii_digit()).count();
        if frac_len == 0 || frac_len > 9 {
            return None;
        }
        rest = &rest[1 + frac_len..];
    }
    let offset = parse_offset(rest)?;
    let day_secs = hour * 3_600 + minute * 60 + second;
    days.checked_mul(DAY_SECS)?
        .checked_add(day_secs)?
        .checked_sub(offset)
}

/// Parse the optional trailing offset into seconds east of UTC.
fn parse_offset(rest: &[u8]) -> Option<i64> {
    if rest.is_empty() {
        return Some(0);
    }
    if rest.len() == 1 && matches!(rest[0], b'Z' | b'z') {
        return Some(0);
    }
    let sign = match rest.first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits_only: Vec<u8> = rest[1..].iter().filter(|b| **b != b':').copied().collect();
    if digits_only.len() != 2 && digits_only.len() != 4 {
        return None;
    }
    if rest[1..].iter().filter(|b| **b == b':').count() > 1 {
        return None;
    }
    let hours = digits(&digits_only[0..2])?;
    let minutes = if digits_only.len() == 4 {
        digits(&digits_only[2..4])?
    } else {
        0
    };
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3_600 + minutes * 60))
}

/// Parse up to 4 ASCII digits into a small integer.
fn digits(bytes: &[u8]) -> Option<i64> {
    if bytes.is_empty() || bytes.len() > 4 || !bytes.iter().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let mut value: i64 = 0;
    for byte in bytes {
        value = value.checked_mul(10)?.checked_add(i64::from(byte - b'0'))?;
    }
    Some(value)
}

/// Days from the civil date to the unix epoch (negative before 1970).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let adjusted = if month <= 2 { year - 1 } else { year };
    let era = if adjusted >= 0 {
        adjusted
    } else {
        adjusted - 399
    } / 400;
    let year_of_era = adjusted - era * 400;
    let month_prime = (month + 9) % 12;
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Civil date from days since the unix epoch.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    (
        if month <= 2 { year + 1 } else { year },
        month as u32,
        day as u32,
    )
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
        ] {
            assert_eq!(parse_iso(raw), None, "{raw:?}");
        }
        // Trailing space trims clean, so this one parses.
        assert!(parse_iso("  2026-01-01T00:00:00+00:00  ").is_some());
    }
}
