//! Edition preferences as a ranking: which of several editions of one
//! album a person would rather have when nothing else decides.
//!
//! The order of precedence is fixed: a person's choice, then the best fit
//! for the files, then these preferences. Identification uses them as the
//! last tie-break among editions that fit the files equally well; the
//! album page and acquisition use them to pick the edition of an album
//! the library does not hold yet.
//!
//! Within the preferences the order is: release types to avoid, status,
//! media format, country, date, then standard against deluxe.

use std::cmp::Reverse;

use crate::runtime_config::sections::{
    EditionDatePreference, EditionPreferences, EditionVersionPreference,
};

/// Words that mark an expanded edition in a title or disambiguation.
const EXPANDED: [&str; 7] = [
    "deluxe",
    "expanded",
    "anniversary",
    "bonus",
    "special edition",
    "collector",
    "super deluxe",
];

/// The preferences, ready to rank editions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preferences {
    status: Vec<String>,
    formats: Vec<String>,
    countries: Vec<String>,
    date: EditionDatePreference,
    version: EditionVersionPreference,
    avoid: Vec<String>,
}

impl Default for Preferences {
    fn default() -> Self {
        Self::from_settings(&EditionPreferences::default(), "US")
    }
}

/// What the preferences look at on one edition.
#[derive(Debug, Clone, Default)]
pub struct EditionFacts<'a> {
    pub status: Option<&'a str>,
    pub formats: Vec<&'a str>,
    pub country: Option<&'a str>,
    pub date: Option<&'a str>,
    /// Title and disambiguation, for telling deluxe editions apart.
    pub text: Vec<&'a str>,
    /// Release-group secondary types (`Live`, `Compilation`).
    pub types: Vec<&'a str>,
}

/// Comparable rank: lower is preferred.
pub type PreferenceKey = (bool, usize, usize, usize, DateRank, usize);

/// Date part of the rank: known dates first, then earlier or later.
pub type DateRank = (bool, Reverse<i64>);

impl Preferences {
    /// The saved preferences. An empty country list means the store
    /// region, then worldwide.
    pub fn from_settings(settings: &EditionPreferences, store_region: &str) -> Self {
        let mut countries = settings.countries.clone();
        if countries.is_empty() {
            let region = store_region.trim().to_ascii_uppercase();
            if region.len() == 2 {
                countries.push(region);
            }
            if !countries.iter().any(|code| code == "XW") {
                countries.push("XW".to_owned());
            }
        }
        Self {
            status: lower(&settings.status_order),
            formats: lower(&settings.format_order),
            countries,
            date: settings.date,
            version: settings.version,
            avoid: lower(&settings.avoid_types),
        }
    }

    /// The edition's rank under these preferences; lower is better.
    pub fn key(&self, facts: &EditionFacts<'_>) -> PreferenceKey {
        let avoided = facts.types.iter().any(|kind| {
            self.avoid
                .iter()
                .any(|avoid| kind.trim().eq_ignore_ascii_case(avoid))
        });
        let status = facts
            .status
            .and_then(|status| {
                self.status
                    .iter()
                    .position(|wanted| status.trim().eq_ignore_ascii_case(wanted))
            })
            .unwrap_or(self.status.len());
        let format = facts
            .formats
            .iter()
            .filter_map(|format| {
                let format = format.to_lowercase();
                self.formats
                    .iter()
                    .position(|wanted| format.contains(wanted.as_str()))
            })
            .min()
            .unwrap_or(self.formats.len());
        let country = facts
            .country
            .and_then(|country| {
                self.countries
                    .iter()
                    .position(|wanted| country.trim().eq_ignore_ascii_case(wanted))
            })
            .unwrap_or(self.countries.len());
        let expanded = facts.text.iter().any(|text| {
            let text = text.to_lowercase();
            EXPANDED.iter().any(|word| text.contains(word))
        });
        let version = match (self.version, expanded) {
            (EditionVersionPreference::Standard, true)
            | (EditionVersionPreference::Deluxe, false) => 1,
            _ => 0,
        };
        (
            avoided,
            status,
            format,
            country,
            self.date_rank(facts.date),
            version,
        )
    }

    fn date_rank(&self, date: Option<&str>) -> DateRank {
        let ordinal = date.and_then(date_ordinal);
        match (self.date, ordinal) {
            (EditionDatePreference::Any, _) | (_, None) => {
                (self.date != EditionDatePreference::Any, Reverse(0))
            }
            // Reverse turns "larger first" into "smaller first".
            (EditionDatePreference::Earliest, Some(day)) => (false, Reverse(-day)),
            (EditionDatePreference::Latest, Some(day)) => (false, Reverse(day)),
        }
    }
}

fn lower(list: &[String]) -> Vec<String> {
    list.iter()
        .map(|entry| entry.trim().to_lowercase())
        .collect()
}

/// `YYYY`, `YYYY-MM` or `YYYY-MM-DD` as one comparable number. Unknown
/// month and day count as the middle of the year, so vague and precise
/// dates of one year stay together.
fn date_ordinal(date: &str) -> Option<i64> {
    let mut parts = date.trim().split('-');
    let year: i64 = parts.next().filter(|part| part.len() == 4)?.parse().ok()?;
    let month: i64 = parts.next().and_then(|part| part.parse().ok()).unwrap_or(6);
    let day: i64 = parts
        .next()
        .and_then(|part| part.parse().ok())
        .unwrap_or(15);
    Some(year * 10_000 + month * 100 + day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts<'a>(
        status: &'a str,
        format: &'a str,
        country: &'a str,
        date: &'a str,
    ) -> EditionFacts<'a> {
        EditionFacts {
            status: Some(status),
            formats: vec![format],
            country: Some(country),
            date: Some(date),
            ..EditionFacts::default()
        }
    }

    #[test]
    fn preferences_rank_in_their_documented_order() {
        let prefs = Preferences::from_settings(&EditionPreferences::default(), "gb");
        let official_cd = facts("Official", "CD", "GB", "1997");
        let official_digital = facts("Official", "Digital Media", "XW", "2015");
        let promo_digital = facts("Promotion", "Digital Media", "GB", "1997");
        // Status first, then format: digital beats CD.
        assert!(prefs.key(&official_digital) < prefs.key(&official_cd));
        assert!(prefs.key(&official_cd) < prefs.key(&promo_digital));
        // Same status and format: the store region beats worldwide.
        let uk = facts("Official", "CD", "GB", "2001");
        let world = facts("Official", "CD", "XW", "1997");
        assert!(prefs.key(&uk) < prefs.key(&world));
        // Then the earliest date, then the standard edition.
        let early = facts("Official", "CD", "GB", "1997-06");
        assert!(prefs.key(&early) < prefs.key(&uk));
        let mut deluxe = facts("Official", "CD", "GB", "1997-06");
        deluxe.text = vec!["Deluxe Edition"];
        assert!(prefs.key(&early) < prefs.key(&deluxe));
        // Avoided types lose to anything else.
        let mut live = facts("Official", "Digital Media", "GB", "1990");
        live.types = vec!["Live"];
        assert!(prefs.key(&promo_digital) < prefs.key(&live));
    }
}
