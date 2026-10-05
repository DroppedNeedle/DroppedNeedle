//! Case-insensitive query parsing, ported from v2's Jellyfin router (`_CIParams`
//! and friends). Real Jellyfin (ASP.NET Core) binds query strings
//! case-insensitively, so clients send mixed casing (`parentId` vs
//! `ParentId`).

/// Query params keyed case-insensitively, preserving repeats (v2 `_CIParams`).
#[derive(Debug, Clone, Default)]
pub struct CiParams {
    multi: Vec<(String, String)>,
}

impl CiParams {
    /// Parse a raw query string (`a=1&A=2`), percent-decoding names and
    /// values (`+` decodes to space, v2 Starlette parity).
    pub fn parse(raw: Option<&str>) -> Self {
        let mut multi = Vec::new();
        for pair in raw.unwrap_or("").split('&') {
            if pair.is_empty() {
                continue;
            }
            let (name, value) = match pair.find('=') {
                Some(i) => (&pair[..i], &pair[i + 1..]),
                None => (pair, ""),
            };
            multi.push((decode(name).to_lowercase(), decode(value)));
        }
        Self { multi }
    }

    /// First value for a case-insensitive key (v2 `_CIParams.get`).
    pub fn get(&self, key: &str) -> Option<&str> {
        let want = key.to_lowercase();
        self.multi
            .iter()
            .find(|(name, _)| *name == want)
            .map(|(_, value)| value.as_str())
    }

    /// Every value for a case-insensitive key (v2 `_CIParams.getlist`).
    pub fn getlist(&self, key: &str) -> Vec<&str> {
        let want = key.to_lowercase();
        self.multi
            .iter()
            .filter(|(name, _)| *name == want)
            .map(|(_, value)| value.as_str())
            .collect()
    }
}

/// Split every value of a key on commas, dropping empties (v2 `_csv_param`).
pub fn csv_param(params: &CiParams, key: &str) -> Vec<String> {
    let mut out = Vec::new();
    for value in params.getlist(key) {
        out.extend(
            value
                .split(',')
                .filter(|p| !p.is_empty())
                .map(str::to_owned),
        );
    }
    out
}

/// `?IsFavorite=true` or `Filters` containing `IsFavorite` (v2
/// `_wants_favorites`).
pub fn wants_favorites(params: &CiParams) -> bool {
    if params
        .get("isFavorite")
        .unwrap_or("")
        .eq_ignore_ascii_case("true")
    {
        return true;
    }
    csv_param(params, "Filters")
        .iter()
        .any(|f| f.eq_ignore_ascii_case("isfavorite"))
}

/// Lenient int query: missing/empty/unparseable falls back (v2 `_qint`).
pub fn qint(params: &CiParams, key: &str, default: i64) -> i64 {
    match params.get(key) {
        None | Some("") => default,
        Some(raw) => raw.parse::<i64>().unwrap_or(default),
    }
}

/// Deduplicated multi-spelling id collection: lookups are case-insensitive,
/// so spellings must be deduped or each id is collected once per spelling
/// (v2 `_ids_param`).
pub fn ids_param(params: &CiParams, keys: &[&str]) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for key in keys {
        if !seen.insert(key.to_lowercase()) {
            continue;
        }
        for value in params.getlist(key) {
            out.extend(
                value
                    .split(',')
                    .filter(|p| !p.is_empty())
                    .map(str::to_owned),
            );
        }
    }
    out
}

// ===== SortBy allowlist (v2 `_browse_sort`) =====

/// Resolved sort key: the catalog sorts plus the two play-history sorts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    /// `DateCreated`: newest first by default.
    Recent,
    /// `SortName`.
    Title,
    /// `ProductionYear`.
    Year,
    /// `PremiereDate`: Jellify sends this first; year desc by default.
    PremiereDate,
    /// `Random`.
    Random,
    /// `DatePlayed` history sort.
    DatePlayed,
    /// `PlayCount` history sort.
    PlayCount,
}

impl SortKey {
    fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_lowercase().as_str() {
            "datecreated" => Some(Self::Recent),
            "sortname" => Some(Self::Title),
            "productionyear" => Some(Self::Year),
            "premieredate" => Some(Self::PremiereDate),
            "random" => Some(Self::Random),
            "dateplayed" => Some(Self::DatePlayed),
            "playcount" => Some(Self::PlayCount),
            _ => None,
        }
    }

    /// History sorts page from play history rather than the catalog.
    pub fn is_history(self) -> bool {
        matches!(self, Self::DatePlayed | Self::PlayCount)
    }

    /// `SortOrder` omitted: these keys default to descending, because the
    /// legacy lists are newest-first (v2 `_SORT_DESC_DEFAULT`).
    fn desc_default(self) -> bool {
        matches!(
            self,
            Self::Recent | Self::DatePlayed | Self::PlayCount | Self::PremiereDate
        )
    }
}

/// `(SortBy key or None for legacy order, descending)`. Comma-separated
/// `SortBy` resolves first-known-wins; unknown values are ignored so the
/// legacy default order keeps working (v2 `_browse_sort`).
pub fn browse_sort(params: &CiParams) -> (Option<SortKey>, bool) {
    let mut key = None;
    for value in csv_param(params, "SortBy") {
        if let Some(candidate) = SortKey::from_name(&value) {
            key = Some(candidate);
            break;
        }
    }
    let Some(key) = key else {
        return (None, false);
    };
    let order = params.get("SortOrder").unwrap_or("").trim().to_lowercase();
    if order.starts_with("desc") {
        return (Some(key), true);
    }
    if order.starts_with("asc") {
        return (Some(key), false);
    }
    (Some(key), key.desc_default())
}

/// Minimal percent-decoder (`+` to space; malformed escapes pass through).
fn decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() + 1 => {
                if let (Some(h), Some(l)) = (hex(bytes.get(i + 1)), hex(bytes.get(i + 2))) {
                    out.push(h << 4 | l);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: Option<&u8>) -> Option<u8> {
    match byte.copied()? {
        b'0'..=b'9' => Some(byte.copied()? - b'0'),
        b'a'..=b'f' => Some(byte.copied()? - b'a' + 10),
        b'A'..=b'F' => Some(byte.copied()? - b'A' + 10),
        _ => None,
    }
}
