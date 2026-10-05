//! Pure contribution rules: parsing, validation, draft derivation, duplicate
//! ordering, retry delays, and seed-field building.
//!
//! Each function ports one v2 `LibraryContributionService` helper; the doc
//! comment names it plus any quirk it preserves.

use std::collections::{HashMap, HashSet};

use super::models::*;

// ---------------------------------------------------------------------------
// ID parsing (v2 `parse_discogs_release_id` / `parse_musicbrainz_release_id`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseIdError {
    Discogs,
    DiscogsMaster,
    MusicBrainz,
}

impl ParseIdError {
    pub fn message(&self) -> &'static str {
        match self {
            Self::Discogs => "Enter a valid Discogs release URL or numeric ID.",
            // Quirk (v2): master URLs get their own message, not the generic one.
            Self::DiscogsMaster => "Enter an exact Discogs release URL, not a master URL.",
            Self::MusicBrainz => "Enter a MusicBrainz release MBID or release URL.",
        }
    }
}

/// Accept a bare positive integer or an exact `https://[www.]discogs.com/release/<id>`
/// URL. Rejects userinfo, ports, queries, fragments, and non-release paths.
pub fn parse_discogs_release_id(value: &str) -> Result<String, ParseIdError> {
    let candidate = value.trim();
    if !candidate.is_empty() && candidate.bytes().all(|b| b.is_ascii_digit()) {
        // Quirk (v2): int() normalizes ("007" -> "7") with no upper bound.
        let stripped = candidate.trim_start_matches('0');
        if stripped.is_empty() {
            return Err(ParseIdError::Discogs);
        }
        return Ok(stripped.to_string());
    }
    let rest = candidate
        .strip_prefix("https://")
        .ok_or(ParseIdError::Discogs)?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => return Err(ParseIdError::Discogs),
    };
    if authority.contains('@')
        || authority.contains(':')
        || authority.contains('?')
        || authority.contains('#')
    {
        return Err(ParseIdError::Discogs);
    }
    let host = authority.to_ascii_lowercase();
    if host != "discogs.com" && host != "www.discogs.com" {
        return Err(ParseIdError::Discogs);
    }
    if path.contains('?') || path.contains('#') {
        return Err(ParseIdError::Discogs);
    }
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segments.len() != 2 || segments[0] != "release" {
        // Quirk (v2): anything shaped like a master URL (or any other
        // non-release path) reports the master-specific message.
        return Err(ParseIdError::DiscogsMaster);
    }
    let slug = segments[1];
    let id_part = slug.split('-').next().unwrap_or("");
    // Quirk (v2): `[1-9]\d*` - leading zeros rejected in URL form (unlike
    // the bare numeric form, which int() normalizes).
    if id_part.is_empty()
        || !id_part.bytes().all(|b| b.is_ascii_digit())
        || id_part.starts_with('0')
    {
        return Err(ParseIdError::DiscogsMaster);
    }
    Ok(id_part.to_string())
}

/// Accept a bare MBID or an exact `https://musicbrainz.org/release/<mbid>/`
/// URL. Quirk (v2): http is rejected, port must be absent/443, and any query
/// or fragment is rejected; the returned MBID is lowercased via UUID parse.
pub fn parse_musicbrainz_release_id(value: &str) -> Result<String, ParseIdError> {
    let candidate = value.trim();
    if candidate.contains("://") {
        let rest = candidate
            .strip_prefix("https://")
            .ok_or(ParseIdError::MusicBrainz)?;
        let (authority, path) = match rest.find('/') {
            Some(i) => (&rest[..i], &rest[i..]),
            None => return Err(ParseIdError::MusicBrainz),
        };
        if authority.contains('@') {
            return Err(ParseIdError::MusicBrainz);
        }
        let (host, port) = match authority.rfind(':') {
            Some(i) => (&authority[..i], Some(&authority[i + 1..])),
            None => (authority, None),
        };
        if !host.eq_ignore_ascii_case("musicbrainz.org") {
            return Err(ParseIdError::MusicBrainz);
        }
        // Quirk (v2): explicit :443 is allowed, any other port is not.
        if let Some(p) = port
            && p != "443"
        {
            return Err(ParseIdError::MusicBrainz);
        }
        if path.contains('?') || path.contains('#') {
            return Err(ParseIdError::MusicBrainz);
        }
        let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        if segments.len() != 2 || segments[0] != "release" {
            return Err(ParseIdError::MusicBrainz);
        }
        return normalize_mbid(segments[1]);
    }
    normalize_mbid(candidate)
}

fn normalize_mbid(candidate: &str) -> Result<String, ParseIdError> {
    candidate
        .parse::<uuid::Uuid>()
        .map(|u| u.to_string())
        .map_err(|_| ParseIdError::MusicBrainz)
}

/// Callback tokens are 32-128 URL-safe characters (v2 `_CALLBACK_TOKEN`).
pub fn valid_callback_token(token: &str) -> bool {
    let len = token.len();
    (32..=128).contains(&len)
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

// ---------------------------------------------------------------------------
// Draft derivation (v2 `_draft_from_snapshot`)
// ---------------------------------------------------------------------------

/// A fresh draft copies local title/artist/date plus the full medium/track
/// skeleton; every other field starts empty with `local` provenance.
pub fn draft_from_snapshot(snapshot: &LocalReleaseSnapshot) -> ReleaseDraft {
    let date = snapshot
        .release_date
        .clone()
        .or_else(|| snapshot.year.map(|y| y.to_string()));
    ReleaseDraft {
        schema_version: 1,
        title: ReleaseTextField::local(non_empty(snapshot.title.clone())),
        artist_credit: ReleaseTextField::local(non_empty(snapshot.album_artist_name.clone())),
        release_date: ReleaseTextField::local(date),
        country: ReleaseTextField::default(),
        label: ReleaseTextField::default(),
        catalogue_number: ReleaseTextField::default(),
        barcode: ReleaseTextField::default(),
        packaging: ReleaseTextField::default(),
        media: snapshot
            .media
            .iter()
            .map(|medium| ReleaseMediumDraft {
                position: medium.position,
                title: ReleaseTextField::local(medium.title.clone()),
                format: ReleaseTextField::default(),
                tracks: medium
                    .tracks
                    .iter()
                    .map(|track| ReleaseTrackDraft {
                        local_track_id: track.local_track_id.clone(),
                        disc_number: track.disc_number,
                        track_number: track.track_number,
                        title: ReleaseTextField::local(non_empty(track.title.clone())),
                        artist_name: ReleaseTextField::local(track.artist_name.clone()),
                        duration_seconds: track.duration_seconds,
                    })
                    .collect(),
            })
            .collect(),
    }
}

fn non_empty(value: String) -> Option<String> {
    if value.is_empty() { None } else { Some(value) }
}

// ---------------------------------------------------------------------------
// Draft normalization + provenance (v2 `_normalize_draft`,
// `_validate_local_provenance`, `_validate_discogs_values`, `_trim_strings`)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftError(pub String);

/// Structural checks (schema version, exact track-list match, immutable
/// positions), provenance checks (local values unchanged unless marked
/// entered-here, Discogs values matching the selected release), then trim.
pub fn normalize_draft(
    draft: &ReleaseDraft,
    snapshot: &LocalReleaseSnapshot,
    selection: &ContributionSourceSelection,
    discogs_release: Option<&DiscogsRelease>,
) -> Result<ReleaseDraft, DraftError> {
    if draft.schema_version != 1 {
        return Err(DraftError("Unsupported contribution draft version.".into()));
    }
    let snapshot_tracks: HashMap<&str, &ReleaseTrackSnapshot> = snapshot
        .media
        .iter()
        .flat_map(|m| m.tracks.iter())
        .map(|t| (t.local_track_id.as_str(), t))
        .collect();
    let draft_tracks: Vec<&ReleaseTrackDraft> =
        draft.media.iter().flat_map(|m| m.tracks.iter()).collect();
    let draft_ids: HashSet<&str> = draft_tracks
        .iter()
        .map(|t| t.local_track_id.as_str())
        .collect();
    if draft_tracks.len() != snapshot_tracks.len()
        || draft_ids.len() != draft_tracks.len()
        || draft_ids != snapshot_tracks.keys().copied().collect::<HashSet<_>>()
    {
        if draft_ids.len() != draft_tracks.len() {
            return Err(DraftError(
                "A contribution track may only appear once.".into(),
            ));
        }
        return Err(DraftError(
            "The contribution track list does not match the album.".into(),
        ));
    }
    for track in &draft_tracks {
        let source = snapshot_tracks[track.local_track_id.as_str()];
        if track.disc_number != source.disc_number || track.track_number != source.track_number {
            return Err(DraftError(
                "Track positions cannot be changed in this step.".into(),
            ));
        }
    }
    validate_local_provenance(
        draft,
        snapshot,
        &snapshot_tracks,
        discogs_release,
        selection,
    )?;
    let mut trimmed = draft.clone();
    trim_draft(&mut trimmed)?;
    Ok(trimmed)
}

fn validate_local_provenance(
    draft: &ReleaseDraft,
    snapshot: &LocalReleaseSnapshot,
    snapshot_tracks: &HashMap<&str, &ReleaseTrackSnapshot>,
    discogs_release: Option<&DiscogsRelease>,
    selection: &ContributionSourceSelection,
) -> Result<(), DraftError> {
    let local_date = snapshot
        .release_date
        .clone()
        .or_else(|| snapshot.year.map(|y| y.to_string()));
    for (field, local) in [
        (&draft.title, Some(snapshot.title.clone())),
        (
            &draft.artist_credit,
            Some(snapshot.album_artist_name.clone()),
        ),
        (&draft.release_date, local_date),
    ] {
        if field.source == ContributionFieldSource::Local && field.value != local {
            return Err(DraftError(
                "A changed value must be marked as entered here.".into(),
            ));
        }
    }
    for field in [
        &draft.country,
        &draft.label,
        &draft.catalogue_number,
        &draft.barcode,
        &draft.packaging,
    ] {
        // Quirk (v2): optional fields have no local value, so any value with
        // `local` provenance is rejected - even an empty string counts as set
        // only when it is Some; None passes.
        if field.source == ContributionFieldSource::Local && field.value.is_some() {
            return Err(DraftError(
                "This value was not present in local metadata.".into(),
            ));
        }
    }
    for medium in &draft.media {
        for track in &medium.tracks {
            let local = snapshot_tracks[track.local_track_id.as_str()];
            for (field, local_value) in [
                (&track.title, Some(local.title.clone())),
                (&track.artist_name, local.artist_name.clone()),
            ] {
                if field.source == ContributionFieldSource::Local && field.value != local_value {
                    return Err(DraftError(
                        "A changed track value must be marked as entered here.".into(),
                    ));
                }
            }
        }
    }
    if draft_fields(draft)
        .iter()
        .any(|f| f.source == ContributionFieldSource::Discogs)
    {
        let Some(release) = discogs_release else {
            return Err(DraftError(
                "Refresh the Discogs source before using its values.".into(),
            ));
        };
        validate_discogs_values(draft, selection, release)?;
    }
    Ok(())
}

fn validate_discogs_values(
    draft: &ReleaseDraft,
    selection: &ContributionSourceSelection,
    release: &DiscogsRelease,
) -> Result<(), DraftError> {
    let label = release.labels.first();
    for (field, expected) in [
        (&draft.title, Some(release.title.clone())),
        (&draft.artist_credit, Some(release.artist_name.clone())),
        (&draft.release_date, release.released_date.clone()),
        (&draft.country, release.country.clone()),
        (&draft.label, label.map(|l| l.name.clone())),
        (
            &draft.catalogue_number,
            label.and_then(|l| l.catalogue_number.clone()),
        ),
        (&draft.barcode, release.barcode.clone()),
    ] {
        if field.source == ContributionFieldSource::Discogs && field.value != expected {
            return Err(DraftError(
                "A Discogs value must match the selected Discogs release.".into(),
            ));
        }
    }
    // Quirk (v2): Discogs never verifies packaging, so that source is always wrong.
    if draft.packaging.source == ContributionFieldSource::Discogs {
        return Err(DraftError(
            "Discogs did not provide a verified packaging value.".into(),
        ));
    }
    let provider_media: HashMap<i64, &DiscogsMedium> =
        release.media.iter().map(|m| (m.position, m)).collect();
    let alignment_by_track: HashMap<&str, &TrackAlignment> = selection
        .alignments
        .iter()
        .map(|a| (a.local_track_id.as_str(), a))
        .collect();
    for medium in &draft.media {
        let provider_medium = provider_media.get(&medium.position).copied();
        if medium.title.source == ContributionFieldSource::Discogs {
            let expected = provider_medium.and_then(|m| m.title.clone());
            if medium.title.value != expected {
                return Err(DraftError(
                    "The Discogs medium title does not match.".into(),
                ));
            }
        }
        if medium.format.source == ContributionFieldSource::Discogs {
            let expected = provider_medium.and_then(|m| m.format.clone());
            if medium.format.value != expected {
                return Err(DraftError(
                    "The Discogs medium format does not match.".into(),
                ));
            }
        }
        for track in &medium.tracks {
            let provider_track = alignment_by_track
                .get(track.local_track_id.as_str())
                .and_then(|a| a.provider_position.as_deref())
                .and_then(|pos| {
                    provider_medium.and_then(|m| {
                        m.tracks
                            .iter()
                            .find(|t| t.source_position.as_deref() == Some(pos) && !t.heading)
                    })
                });
            if track.title.source == ContributionFieldSource::Discogs
                && (provider_track
                    .is_none_or(|t| track.title.value.as_deref() != Some(t.title.as_str())))
            {
                return Err(DraftError("The Discogs track title does not match.".into()));
            }
            if track.artist_name.source == ContributionFieldSource::Discogs {
                // Quirk (v2): falls back to the release artist when the
                // provider track has no per-track artists.
                let expected = provider_track
                    .and_then(|t| t.artists.first())
                    .map(|a| a.credited_name.clone().unwrap_or_else(|| a.name.clone()))
                    .unwrap_or_else(|| release.artist_name.clone());
                if track.artist_name.value.as_deref() != Some(expected.as_str()) {
                    return Err(DraftError(
                        "The Discogs track artist does not match.".into(),
                    ));
                }
            }
        }
    }
    Ok(())
}

fn draft_fields(draft: &ReleaseDraft) -> Vec<&ReleaseTextField> {
    let mut fields = vec![
        &draft.title,
        &draft.artist_credit,
        &draft.release_date,
        &draft.country,
        &draft.label,
        &draft.catalogue_number,
        &draft.barcode,
        &draft.packaging,
    ];
    for medium in &draft.media {
        fields.push(&medium.title);
        fields.push(&medium.format);
        for track in &medium.tracks {
            fields.push(&track.title);
            fields.push(&track.artist_name);
        }
    }
    fields
}

fn draft_fields_mut(draft: &mut ReleaseDraft) -> Vec<&mut ReleaseTextField> {
    let mut fields = vec![
        &mut draft.title,
        &mut draft.artist_credit,
        &mut draft.release_date,
        &mut draft.country,
        &mut draft.label,
        &mut draft.catalogue_number,
        &mut draft.barcode,
        &mut draft.packaging,
    ];
    for medium in &mut draft.media {
        fields.push(&mut medium.title);
        fields.push(&mut medium.format);
        for track in &mut medium.tracks {
            fields.push(&mut track.title);
            fields.push(&mut track.artist_name);
        }
    }
    fields
}

fn trim_draft(draft: &mut ReleaseDraft) -> Result<(), DraftError> {
    for field in draft_fields_mut(draft) {
        if let Some(value) = field.value.take() {
            let stripped = value.trim().to_string();
            // Quirk (v2): length is enforced after trimming, on every string.
            if stripped.len() > MAX_TEXT_LENGTH {
                return Err(DraftError("A contribution value is too long.".into()));
            }
            field.value = Some(stripped);
        }
    }
    Ok(())
}

/// Drop Discogs-sourced values back to their local defaults (v2
/// `_without_discogs_values`): used on source removal and provider-data purge.
pub fn without_discogs_values(
    draft: &ReleaseDraft,
    snapshot: &LocalReleaseSnapshot,
) -> ReleaseDraft {
    let mut result = draft.clone();
    let local = draft_from_snapshot(snapshot);
    let pairs = [
        (&mut result.title, &local.title),
        (&mut result.artist_credit, &local.artist_credit),
        (&mut result.release_date, &local.release_date),
        (&mut result.country, &local.country),
        (&mut result.label, &local.label),
        (&mut result.catalogue_number, &local.catalogue_number),
        (&mut result.barcode, &local.barcode),
        (&mut result.packaging, &local.packaging),
    ];
    for (field, fallback) in pairs {
        if field.source == ContributionFieldSource::Discogs {
            field.value = fallback.value.clone();
            field.source = ContributionFieldSource::Local;
        }
    }
    let local_media: HashMap<i64, &ReleaseMediumDraft> =
        local.media.iter().map(|m| (m.position, m)).collect();
    for medium in &mut result.media {
        if let Some(local_medium) = local_media.get(&medium.position) {
            for (field, fallback) in [
                (&mut medium.title, &local_medium.title),
                (&mut medium.format, &local_medium.format),
            ] {
                if field.source == ContributionFieldSource::Discogs {
                    field.value = fallback.value.clone();
                    field.source = ContributionFieldSource::Local;
                }
            }
            let local_tracks: HashMap<&str, &ReleaseTrackDraft> = local_medium
                .tracks
                .iter()
                .map(|t| (t.local_track_id.as_str(), t))
                .collect();
            for track in &mut medium.tracks {
                if let Some(local_track) = local_tracks.get(track.local_track_id.as_str()) {
                    for (field, fallback) in [
                        (&mut track.title, &local_track.title),
                        (&mut track.artist_name, &local_track.artist_name),
                    ] {
                        if field.source == ContributionFieldSource::Discogs {
                            field.value = fallback.value.clone();
                            field.source = ContributionFieldSource::Local;
                        }
                    }
                }
            }
        }
    }
    result
}

/// Expired Discogs values are redacted to None but keep their source marker
/// (v2 `_redact_expired_discogs`), so the UI can offer a refresh.
pub fn redact_expired_discogs(draft: &ReleaseDraft) -> ReleaseDraft {
    let mut result = draft.clone();
    for field in draft_fields_mut(&mut result) {
        if field.source == ContributionFieldSource::Discogs {
            field.value = None;
        }
    }
    result
}

// ---------------------------------------------------------------------------
// Validation (v2 `_validate`)
// ---------------------------------------------------------------------------

fn issue(code: &str, field: String, message: &str) -> ContributionValidationIssue {
    ContributionValidationIssue {
        code: code.to_string(),
        field,
        message: message.to_string(),
    }
}

/// Draft completeness rules with v2's exact codes and messages.
pub fn validate_draft(
    draft: &ReleaseDraft,
    snapshot: &LocalReleaseSnapshot,
) -> Vec<ContributionValidationIssue> {
    let mut issues = Vec::new();
    if draft.title.text().is_empty() {
        issues.push(issue(
            "RELEASE_TITLE_REQUIRED",
            "title".into(),
            "Add a release title.",
        ));
    }
    if draft.artist_credit.text().is_empty() {
        issues.push(issue(
            "ARTIST_CREDIT_REQUIRED",
            "artist_credit".into(),
            "Add an artist credit.",
        ));
    } else if snapshot.artist_kind == "unknown"
        && draft.artist_credit.text().to_lowercase() == snapshot.album_artist_name.to_lowercase()
    {
        // Quirk (v2): the placeholder check compares case-insensitively
        // against the snapshot artist name, not a fixed "Unknown Artist".
        issues.push(issue(
            "ARTIST_CREDIT_PLACEHOLDER",
            "artist_credit".into(),
            "Replace the Unknown Artist placeholder with a real artist credit.",
        ));
    }
    if snapshot.artist_kind == "various_artists" && snapshot.musicbrainz_artist_id.is_none() {
        issues.push(issue(
            "VARIOUS_ARTISTS_IDENTITY_REQUIRED",
            "artist_credit".into(),
            "Link Various Artists to MusicBrainz before contributing this release.",
        ));
    }
    if draft.media.is_empty() {
        issues.push(issue(
            "TRACKS_REQUIRED",
            "media".into(),
            "At least one track is required.",
        ));
    }
    for (medium_index, medium) in draft.media.iter().enumerate() {
        if medium.tracks.is_empty() {
            issues.push(issue(
                "MEDIUM_TRACKS_REQUIRED",
                format!("media.{medium_index}.tracks"),
                "Each medium needs at least one track.",
            ));
        }
        for (track_index, track) in medium.tracks.iter().enumerate() {
            if track.title.text().is_empty() {
                issues.push(issue(
                    "TRACK_TITLE_REQUIRED",
                    format!("media.{medium_index}.tracks.{track_index}.title"),
                    "Add a track title.",
                ));
            }
        }
    }
    issues
}

// ---------------------------------------------------------------------------
// Next actions (v2 `_record` derivation)
// ---------------------------------------------------------------------------

/// Which follow-ups the UI may offer for a record in `state`.
pub fn next_actions(
    state: ContributionState,
    has_validation_issues: bool,
    discogs_selected: bool,
    discogs_expired: bool,
    duplicate: Option<&DuplicateCheckResult>,
    has_result: bool,
    album_active: bool,
) -> Vec<ContributionNextAction> {
    use ContributionNextAction as A;
    use ContributionState as S;
    if state == S::Stale {
        // Quirk (v2): a stale contribution for a deleted album offers
        // nothing, not even cancel.
        return if album_active {
            vec![A::Rebuild]
        } else {
            Vec::new()
        };
    }
    if state == S::Seeded {
        return vec![A::SeedMusicbrainz, A::Cancel];
    }
    if state == S::NeedsReview && has_result {
        return vec![A::RetryVerification, A::Cancel];
    }
    if matches!(state, S::Linked | S::Cancelled) {
        return Vec::new();
    }
    let mut actions = Vec::new();
    if matches!(state, S::Draft | S::Ready | S::NeedsReview) {
        actions.push(A::EditDraft);
        if discogs_selected && discogs_expired {
            actions.push(A::RefreshDiscogs);
        } else if !has_validation_issues {
            actions.push(A::RunDuplicateCheck);
        }
        if let Some(duplicate) = duplicate {
            let exact = duplicate.candidates.iter().filter(|c| c.exact).count();
            if exact == 1 {
                actions.push(A::AttachExisting);
            } else if state == S::Ready && exact == 0 {
                actions.push(A::SeedMusicbrainz);
            }
        }
    }
    actions.push(A::Cancel);
    actions
}

// ---------------------------------------------------------------------------
// Duplicate candidates (v2 `_duplicate_candidate` + `check_duplicates`)
// ---------------------------------------------------------------------------

/// Field-by-field diff between the draft and a verified MusicBrainz release;
/// only fields set on both sides can disagree (v2 `_duplicate_candidate`).
pub fn duplicate_candidate(
    draft: &ReleaseDraft,
    verified: Option<&MusicBrainzVerifiedRelease>,
    release_mbid: String,
    evidence_kind: DuplicateEvidenceKind,
    exact: bool,
) -> DuplicateCandidate {
    let mut differences = Vec::new();
    let (title, artist_name, release_group_mbid) = match verified {
        Some(v) => {
            for (name, proposed, existing) in [
                ("title", draft.title.text(), v.title.as_str()),
                ("artist", draft.artist_credit.text(), v.artist_name.as_str()),
                (
                    "date",
                    draft.release_date.text(),
                    v.date.as_deref().unwrap_or(""),
                ),
                (
                    "country",
                    draft.country.text(),
                    v.country.as_deref().unwrap_or(""),
                ),
                (
                    "label",
                    draft.label.text(),
                    v.label.as_deref().unwrap_or(""),
                ),
                (
                    "catalogue number",
                    draft.catalogue_number.text(),
                    v.catalogue_number.as_deref().unwrap_or(""),
                ),
                (
                    "barcode",
                    draft.barcode.text(),
                    v.barcode.as_deref().unwrap_or(""),
                ),
            ] {
                if !proposed.is_empty()
                    && !existing.is_empty()
                    && proposed.to_lowercase() != existing.to_lowercase()
                {
                    differences.push(format!("Different {name}: {existing}"));
                }
            }
            let local_track_count: usize = draft.media.iter().map(|m| m.tracks.len()).sum();
            if !v.tracks.is_empty() && v.tracks.len() != local_track_count {
                differences.push(format!(
                    "Different track count: {} on MusicBrainz",
                    v.tracks.len()
                ));
            }
            (
                v.title.clone(),
                v.artist_name.clone(),
                Some(v.release_group_mbid.clone()),
            )
        }
        None => (
            draft.title.text().to_string(),
            draft.artist_credit.text().to_string(),
            None,
        ),
    };
    DuplicateCandidate {
        release_mbid: Some(release_mbid),
        release_group_mbid,
        title,
        artist_name,
        evidence_kind,
        exact,
        differences,
    }
}

/// v2 candidate order: evidence kind first, then MBID for stability.
pub fn sort_duplicate_candidates(candidates: &mut [DuplicateCandidate]) {
    candidates.sort_by(|a, b| {
        a.evidence_kind.cmp(&b.evidence_kind).then_with(|| {
            let a_key = a
                .release_mbid
                .as_deref()
                .or(a.release_group_mbid.as_deref());
            let b_key = b
                .release_mbid
                .as_deref()
                .or(b.release_group_mbid.as_deref());
            a_key.cmp(&b_key)
        })
    });
}

/// Post-check state (v2 `check_duplicates`): any exact Discogs-URL hit, an
/// ambiguous multi-group link, or unconfirmed serious similars force review.
pub fn duplicate_check_state(
    has_exact_release_ids: bool,
    group_count: usize,
    has_serious_similar: bool,
    different_edition_confirmed: bool,
) -> ContributionState {
    if has_exact_release_ids
        || group_count > 1
        || (has_serious_similar && !different_edition_confirmed)
    {
        ContributionState::NeedsReview
    } else {
        ContributionState::Ready
    }
}

// ---------------------------------------------------------------------------
// Track alignment (v2 `_align_tracks`)
// ---------------------------------------------------------------------------

/// Normalize a title for fuzzy comparison: casefold, collapse every
/// non-word run to a space (v2 `_normalized_title`).
pub fn normalized_title(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_gap = true;
    for ch in value.to_lowercase().chars() {
        if ch.is_alphanumeric() || ch == '_' {
            out.push(ch);
            in_gap = false;
        } else if !in_gap {
            out.push(' ');
            in_gap = true;
        }
    }
    out.trim_end().to_string()
}

/// difflib `SequenceMatcher(None, a, b).ratio()` on chars: 2*M/T where M is
/// the total size of non-overlapping matching blocks. v2 passes
/// `autojunk=True`, but that only kicks in for inputs >= 200 chars, so plain
/// matching is exact for titles.
pub fn sequence_matcher_ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() && b.is_empty() {
        return 1.0;
    }
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let mut b_index: HashMap<char, Vec<usize>> = HashMap::new();
    for (i, ch) in b.iter().enumerate() {
        b_index.entry(*ch).or_default().push(i);
    }
    let mut queue = vec![(0usize, a.len(), 0usize, b.len())];
    let mut matching = 0usize;
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = longest_match(&a, &b, &b_index, alo, ahi, blo, bhi);
        if k > 0 {
            matching += k;
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    2.0 * matching as f64 / (a.len() + b.len()) as f64
}

fn longest_match(
    a: &[char],
    _b: &[char],
    b_index: &HashMap<char, Vec<usize>>,
    alo: usize,
    ahi: usize,
    blo: usize,
    bhi: usize,
) -> (usize, usize, usize) {
    let mut best_i = alo;
    let mut best_j = blo;
    let mut best_size = 0usize;
    let mut j2len: HashMap<usize, usize> = HashMap::new();
    for (offset, ch) in a[alo..ahi].iter().enumerate() {
        let i = alo + offset;
        let mut new_j2len: HashMap<usize, usize> = HashMap::new();
        if let Some(indices) = b_index.get(ch) {
            for &j in indices {
                if j < blo || j >= bhi {
                    continue;
                }
                let k = j2len.get(&(j.wrapping_sub(1))).copied().unwrap_or(0) + 1;
                new_j2len.insert(j, k);
                if k > best_size {
                    best_i = i + 1 - k;
                    best_j = j + 1 - k;
                    best_size = k;
                }
            }
        }
        j2len = new_j2len;
    }
    // Quirk (difflib): among equal longest blocks the leftmost in a, then
    // the leftmost in b wins - the strict `>` above preserves exactly that.
    // No extension pass: the DP already yields maximal runs.
    (best_i, best_j, best_size)
}

/// Greedy one-to-one local->Discogs alignment. Score = position*0.45 +
/// title*0.45 + duration*0.1; below 0.45 stays unmatched; exact needs
/// position match plus identical normalized titles; >= 0.68 is partial,
/// otherwise conflicting (v2 `_align_tracks`).
pub fn align_tracks(
    snapshot: &LocalReleaseSnapshot,
    release: &DiscogsRelease,
) -> Vec<TrackAlignment> {
    let provider_tracks: Vec<(i64, &DiscogsTrack)> = release
        .media
        .iter()
        .flat_map(|m| m.tracks.iter().map(|t| (m.position, t)))
        .filter(|(_, t)| !t.heading)
        .collect();
    let mut used: HashSet<usize> = HashSet::new();
    let mut alignments = Vec::new();
    for local in snapshot.media.iter().flat_map(|m| m.tracks.iter()) {
        let mut best_index: Option<usize> = None;
        let mut best_score = -1.0f64;
        for (index, (medium_position, provider)) in provider_tracks.iter().enumerate() {
            if used.contains(&index) {
                continue;
            }
            let position_score = if *medium_position == local.disc_number
                && provider.number == Some(local.track_number)
            {
                1.0
            } else {
                0.0
            };
            let title_score = sequence_matcher_ratio(
                &normalized_title(&local.title),
                &normalized_title(&provider.title),
            );
            let mut duration_score = 0.0;
            if local.duration_reliable
                && let (Some(a), Some(b)) = (local.duration_seconds, provider.duration_seconds)
            {
                duration_score = (1.0 - (a - b).abs() / 10.0).max(0.0);
            }
            let score = position_score * 0.45 + title_score * 0.45 + duration_score * 0.1;
            if score > best_score {
                best_index = Some(index);
                best_score = score;
            }
        }
        let Some(matched) = best_index else {
            alignments.push(TrackAlignment {
                local_track_id: local.local_track_id.clone(),
                provider_position: None,
                classification: AlignmentClassification::Unmatched,
            });
            continue;
        };
        if best_score < 0.45 {
            alignments.push(TrackAlignment {
                local_track_id: local.local_track_id.clone(),
                provider_position: None,
                classification: AlignmentClassification::Unmatched,
            });
            continue;
        }
        used.insert(matched);
        let (medium_position, provider) = provider_tracks[matched];
        let position_matches =
            medium_position == local.disc_number && provider.number == Some(local.track_number);
        let titles_equal = normalized_title(&local.title) == normalized_title(&provider.title);
        let classification = if position_matches && titles_equal {
            AlignmentClassification::Exact
        } else if best_score >= 0.68 {
            AlignmentClassification::Partial
        } else {
            AlignmentClassification::Conflicting
        };
        alignments.push(TrackAlignment {
            local_track_id: local.local_track_id.clone(),
            provider_position: provider.source_position.clone(),
            classification,
        });
    }
    alignments
}

// ---------------------------------------------------------------------------
// Verification retry delay (v2 `_retry_or_review`)
// ---------------------------------------------------------------------------

/// Exponential backoff capped at 10 minutes: `min(600, 15 * 2^min(attempts-1, 6))`,
/// raised to the breaker's `retry_after` when that is a positive finite wait.
pub fn verification_retry_delay_seconds(attempts: u32, retry_after: Option<f64>) -> f64 {
    let shift = attempts.saturating_sub(1).min(6);
    let mut delay = (15.0 * 2u64.pow(shift) as f64).min(10.0 * 60.0);
    if let Some(candidate) = retry_after
        && candidate.is_finite()
        && candidate > 0.0
    {
        delay = delay.max(candidate);
    }
    delay
}

// ---------------------------------------------------------------------------
// Seed fields (v2 `_musicbrainz_seed_fields`)
// ---------------------------------------------------------------------------

/// Build the release-editor seed form fields in v2's exact order: name,
/// release_group (single id only), barcode, packaging, event date parts,
/// country, labels, artist credit, mediums/tracks, Discogs URL pair,
/// edit_note, and redirect_uri last.
pub fn musicbrainz_seed_fields(
    draft: &ReleaseDraft,
    snapshot: &LocalReleaseSnapshot,
    duplicate: Option<&DuplicateCheckResult>,
    discogs_release: Option<&DiscogsRelease>,
    recording_mbids: &HashMap<String, String>,
    redirect_uri: &str,
) -> Vec<MusicBrainzSeedField> {
    let mut fields = vec![field("name", draft.title.text())];
    let discovered: Vec<String> = duplicate
        .map(|d| {
            d.candidates
                .iter()
                .filter(|c| {
                    c.evidence_kind == DuplicateEvidenceKind::ReleaseGroup
                        && c.release_group_mbid.is_some()
                })
                .map(|c| c.release_group_mbid.clone().unwrap_or_default())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let mut seen = HashSet::new();
    let discovered: Vec<String> = discovered
        .into_iter()
        .filter(|id| seen.insert(id.clone()))
        .collect();
    // Quirk (v2): a known local group id wins over discovered ones entirely;
    // multiple discovered ids seed nothing (ambiguous).
    let group_ids: Vec<String> = snapshot
        .musicbrainz_release_group_id
        .clone()
        .map(|id| vec![id])
        .unwrap_or(discovered);
    if group_ids.len() == 1 {
        fields.push(field("release_group", &group_ids[0]));
    }
    if !draft.barcode.text().is_empty() {
        fields.push(field("barcode", draft.barcode.text()));
    }
    if !draft.packaging.text().is_empty() {
        fields.push(field("packaging", draft.packaging.text()));
    }
    // Quirk (v2): a bare "2024" still seeds the year; month/day seed
    // only when numeric and nonzero ("00" dropped silently, int() strips
    // leading zeros).
    let date_parts: Vec<&str> = draft.release_date.text().split('-').collect();
    if !date_parts.is_empty()
        && date_parts[0].len() == 4
        && date_parts[0].bytes().all(|b| b.is_ascii_digit())
    {
        fields.push(field("events.0.date.year", date_parts[0]));
        if date_parts.len() > 1
            && !date_parts[1].is_empty()
            && date_parts[1].bytes().all(|b| b.is_ascii_digit())
            && date_parts[1] != "00"
        {
            let month: u32 = date_parts[1].parse().unwrap_or(0);
            fields.push(field("events.0.date.month", &month.to_string()));
        }
        if date_parts.len() > 2
            && !date_parts[2].is_empty()
            && date_parts[2].bytes().all(|b| b.is_ascii_digit())
            && date_parts[2] != "00"
        {
            let day: u32 = date_parts[2].parse().unwrap_or(0);
            fields.push(field("events.0.date.day", &day.to_string()));
        }
    }
    let country = draft.country.text();
    if !country.is_empty() {
        // Quirk (v2): UK is rewritten to GB; anything not 2-alpha is dropped.
        let upper = country.to_ascii_uppercase();
        let normalized = if upper == "UK" {
            "GB".to_string()
        } else {
            upper
        };
        if normalized.len() == 2 && normalized.bytes().all(|b| b.is_ascii_alphabetic()) {
            fields.push(field("events.0.country", &normalized));
        }
    }
    if !draft.label.text().is_empty() || !draft.catalogue_number.text().is_empty() {
        if !draft.label.text().is_empty() {
            fields.push(field("labels.0.name", draft.label.text()));
        }
        if !draft.catalogue_number.text().is_empty() {
            fields.push(field(
                "labels.0.catalog_number",
                draft.catalogue_number.text(),
            ));
        }
    }
    let artist_name = draft.artist_credit.text();
    if let Some(mbid) = snapshot.musicbrainz_artist_id.as_deref() {
        fields.push(field("artist_credit.names.0.mbid", mbid));
    }
    fields.push(field("artist_credit.names.0.artist.name", artist_name));
    fields.push(field("artist_credit.names.0.name", artist_name));
    for (medium_index, medium) in draft.media.iter().enumerate() {
        if !medium.format.text().is_empty() {
            fields.push(field(
                &format!("mediums.{medium_index}.format"),
                medium.format.text(),
            ));
        }
        if !medium.title.text().is_empty() {
            fields.push(field(
                &format!("mediums.{medium_index}.name"),
                medium.title.text(),
            ));
        }
        for (track_index, track) in medium.tracks.iter().enumerate() {
            let prefix = format!("mediums.{medium_index}.track.{track_index}");
            fields.push(field(&format!("{prefix}.name"), track.title.text()));
            fields.push(field(
                &format!("{prefix}.number"),
                &track.track_number.to_string(),
            ));
            if track.duration_seconds.is_some_and(|d| d > 0.0) {
                // Quirk (v2): milliseconds are round(), not floor().
                let ms = (track.duration_seconds.unwrap_or(0.0) * 1000.0).round() as i64;
                fields.push(field(&format!("{prefix}.length"), &ms.to_string()));
            }
            if let Some(recording) = recording_mbids.get(&track.local_track_id) {
                fields.push(field(&format!("{prefix}.recording"), recording));
            }
            let track_artist = track.artist_name.text();
            if !track_artist.is_empty() && track_artist != artist_name {
                fields.push(field(
                    &format!("{prefix}.artist_credit.names.0.artist.name"),
                    track_artist,
                ));
                fields.push(field(
                    &format!("{prefix}.artist_credit.names.0.name"),
                    track_artist,
                ));
            }
        }
    }
    if let Some(release) = discogs_release {
        fields.push(field("urls.0.url", &release.canonical_release_url));
        fields.push(field(
            "urls.0.link_type",
            MUSICBRAINZ_DISCOGS_RELEASE_LINK_TYPE,
        ));
    }
    let discogs_url = discogs_release.map(|r| r.canonical_release_url.as_str());
    fields.push(field("edit_note", &seed_edit_note(discogs_url)));
    fields.push(field("redirect_uri", redirect_uri));
    fields
}

fn field(name: &str, value: &str) -> MusicBrainzSeedField {
    MusicBrainzSeedField {
        name: name.to_string(),
        value: value.to_string(),
    }
}
