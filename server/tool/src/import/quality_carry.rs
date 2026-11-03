//! What an import makes of a v2 download policy. v2 could hold a bare tier
//! range; v3 always holds a recipe, so a range is converted here, once, by
//! the kernel's one conversion. Every step is a pure function of the file, so
//! a re-import writes nothing and a dry run reports the same lines.

use droppedneedle_kernel::runtime_config::sections::{
    DownloadPolicy, QualityRecipeEntry, RecipeFormat, RecipeQuality, Section, derive_default_order,
    order_fits, validate_quality_recipe,
};
use serde_json::{Map, Value};

use super::apply::ImportError;
use super::report::{DROPPED_SETTING, ReportBuilder};

/// The policy fields v2 read from `download_client` while it had no
/// `download_policy` section. Once it had one, these copies were unread.
/// Names and ranges match v3's section.
const CLIENT_POLICY_FIELDS: &[&str] = &[
    "quality_min",
    "quality_max",
    "flac_mp3_only",
    "preflight_score_auto_accept",
    "preflight_score_manual_min",
    "download_stall_timeout_minutes",
    "download_queued_timeout_minutes",
    "preferred_quality_wait_minutes",
    "max_failover_attempts",
    "max_concurrent_downloads",
    "auto_retry_enabled",
    "auto_retry_max_attempts",
    "auto_retry_base_interval_minutes",
];

/// v2 quality settings with no field in v3, with v2's default (none when the
/// default is unset).
const NOT_CARRIED: &[(&str, Option<&str>)] = &[
    ("preferred_lossy_bitrate_kbps", None),
    ("lossy_min_bitrate_kbps", None),
    ("lossy_max_bitrate_kbps", None),
    (LOSSLESS_PREFERENCE, Some("highest")),
    (MAX_BIT_DEPTH, None),
    (MAX_SAMPLE_RATE, None),
    ("unknown_quality_behavior", Some("allow_as_fallback")),
];

const PREFERENCE_ORDER: &str = "quality_preference_order";
const LOSSLESS_PREFERENCE: &str = "lossless_preference";
const MAX_BIT_DEPTH: &str = "lossless_max_bit_depth";
const MAX_SAMPLE_RATE: &str = "lossless_max_sample_rate_hz";

/// A v2 policy's quality settings that end at the import.
pub(super) struct V2Quality {
    preference: Vec<String>,
    changed: Vec<&'static str>,
    capped_to_cd: bool,
    held_by_cd: Vec<&'static str>,
}

/// The `download_policy` a v2 install with no such section ran on, built as
/// v2 built it from `download_client`.
pub(super) fn policy_from_client(exported: &Map<String, Value>) -> Option<Value> {
    if exported.contains_key("download_policy") {
        return None;
    }
    let client = exported.get("download_client")?.as_object()?;
    let carried: Map<String, Value> = CLIENT_POLICY_FIELDS
        .iter()
        .filter_map(|field| Some(((*field).to_owned(), client.get(*field)?.clone())))
        .collect();
    (!carried.is_empty()).then_some(Value::Object(carried))
}

/// Take the policy fields out of `download_client`, which v3 keeps none in.
pub(super) fn strip_client_policy(section: &mut Value) {
    if let Some(object) = section.as_object_mut() {
        for field in CLIENT_POLICY_FIELDS {
            object.remove(*field);
        }
    }
}

/// Take the preference order and the settings a recipe cannot hold out of a
/// policy, noting which of them differ from v2's defaults.
pub(super) fn take_v2_quality(section: &mut Value) -> V2Quality {
    let mut v2 = V2Quality {
        preference: Vec::new(),
        changed: Vec::new(),
        capped_to_cd: false,
        held_by_cd: Vec::new(),
    };
    let Some(object) = section.as_object_mut() else {
        return v2;
    };
    // v2 refused a FLAC whose known depth or rate passed its cap, each cap on
    // its own axis. Only the two together fit a FLAC step: 16-bit alone also
    // let through 16-bit above 48 kHz, 48 kHz alone also let through 24-bit.
    // This is v2's seed for a new install since 2026-08-27 (16-bit, 48 kHz,
    // CD quality preferred). A stricter pair is only partly held by the step,
    // so it stays named in the report.
    let number = |field: &str| object.get(field).and_then(Value::as_i64);
    if let (Some(depth), Some(rate)) = (number(MAX_BIT_DEPTH), number(MAX_SAMPLE_RATE))
        && depth <= 16
        && rate <= 48_000
    {
        v2.capped_to_cd = true;
        if depth == 16 && rate == 48_000 {
            v2.held_by_cd.extend([MAX_BIT_DEPTH, MAX_SAMPLE_RATE]);
        }
        if object.get(LOSSLESS_PREFERENCE).and_then(Value::as_str) == Some("cd") {
            v2.held_by_cd.push(LOSSLESS_PREFERENCE);
        }
    }
    if let Some(Value::Array(order)) = object.remove(PREFERENCE_ORDER) {
        v2.preference = order
            .iter()
            .filter_map(|tier| tier.as_str().map(str::to_owned))
            .collect();
    }
    for (field, default) in NOT_CARRIED {
        let Some(value) = object.remove(*field) else {
            continue;
        };
        if !value.is_null() && default.is_none_or(|default| value.as_str() != Some(default)) {
            v2.changed.push(field);
        }
    }
    v2
}

/// Give a policy its one form: the recipe it has when v3 can read it, else
/// the conversion of its range, with the range and cutoff that recipe sets.
/// A section that is not an object stays as the export has it.
pub(super) fn convert_policy(
    section: &mut Value,
    mut v2: V2Quality,
    report: &mut ReportBuilder,
) -> Result<(), ImportError> {
    let Some(object) = section.as_object_mut() else {
        return Ok(());
    };
    let formats_open = object.get("flac_mp3_only").and_then(Value::as_bool) == Some(false);
    let (kept, refusal) = readable_recipe(object.get("quality_recipe"), formats_open);
    let text = |field: &str| object.get(field).and_then(Value::as_str).map(str::to_owned);
    let defaults = DownloadPolicy::default();
    let old_min = text("quality_min").unwrap_or_else(|| defaults.quality_min.clone());
    let old_max = text("quality_max").unwrap_or_else(|| defaults.quality_max.clone());
    let mut policy = DownloadPolicy {
        quality_min: old_min.clone(),
        quality_max: old_max.clone(),
        quality_cutoff: text("quality_cutoff").unwrap_or_else(|| defaults.quality_cutoff.clone()),
        quality_recipe: kept,
        ..defaults
    };
    let converting = policy.quality_recipe.is_empty();
    if converting {
        policy.convert_range(&v2.preference);
        let accepted = derive_default_order(&policy.quality_min, &policy.quality_max);
        if !v2.preference.is_empty() && !order_fits(&v2.preference, &accepted) {
            v2.changed.push(PREFERENCE_ORDER);
        }
        if v2.capped_to_cd {
            for entry in &mut policy.quality_recipe {
                if entry.quality == RecipeQuality::HiRes {
                    entry.quality = RecipeQuality::Cd;
                }
            }
            v2.changed.retain(|field| !v2.held_by_cd.contains(field));
        }
    }
    policy.normalize();

    if converting {
        let recipe = recipe_words(&policy.quality_recipe);
        let detail = format!("the v2 quality range {old_min} to {old_max} is now {recipe}");
        let detail = match refusal {
            Some(reason) => format!("{reason}, so {detail}"),
            None => detail,
        };
        report.note(
            "settings",
            "download_policy.quality_recipe".to_owned(),
            "imported",
            detail,
        );
    }
    if formats_open {
        report.note(
            "settings",
            "download_policy.flac_mp3_only".to_owned(),
            "imported",
            "turned on. Other formats are no longer taken automatically. A person can \
             still pick one."
                .to_owned(),
        );
    }
    if !v2.changed.is_empty() {
        report.note(
            "settings",
            "download_policy.quality_settings".to_owned(),
            DROPPED_SETTING,
            format!(
                "these v2 settings are not applied: {}",
                v2.changed.join(", ")
            ),
        );
    }

    let recipe =
        serde_json::to_value(&policy.quality_recipe).map_err(|error| ImportError::Config {
            reason: error.to_string(),
        })?;
    object.insert("quality_recipe".to_owned(), recipe);
    object.insert("quality_min".to_owned(), Value::from(policy.quality_min));
    object.insert("quality_max".to_owned(), Value::from(policy.quality_max));
    object.insert(
        "quality_cutoff".to_owned(),
        Value::from(policy.quality_cutoff),
    );
    object.remove("flac_mp3_only");
    Ok(())
}

/// The recipe a v2 policy ran on when v3 can read it, else nothing and, when
/// it held one that goes unused, why. v2 ignored a recipe while formats other
/// than FLAC and MP3 were allowed, and ran on the range instead. The reasons
/// are fixed sentences, so no decoder text reaches the report.
fn readable_recipe(
    recipe: Option<&Value>,
    formats_open: bool,
) -> (Vec<QualityRecipeEntry>, Option<&'static str>) {
    const UNREADABLE: &str = "v3 cannot read the v2 recipe";
    let Some(recipe) = recipe.filter(|recipe| !recipe.is_null()) else {
        return (Vec::new(), None);
    };
    if recipe.as_array().is_some_and(Vec::is_empty) {
        return (Vec::new(), None);
    }
    if formats_open {
        return (
            Vec::new(),
            Some("v2 did not use its recipe while other formats were allowed"),
        );
    }
    match serde_json::from_value::<Vec<QualityRecipeEntry>>(recipe.clone()) {
        Ok(entries) if validate_quality_recipe(&entries).is_ok() => (entries, None),
        _ => (Vec::new(), Some(UNREADABLE)),
    }
}

/// A recipe as a report reads it: `FLAC, every resolution, then MP3 320_plus`.
fn recipe_words(recipe: &[QualityRecipeEntry]) -> String {
    let steps: Vec<String> = recipe
        .iter()
        .map(|entry| match (entry.format, entry.quality) {
            (RecipeFormat::Flac, RecipeQuality::HiRes) => "FLAC, every resolution".to_owned(),
            (RecipeFormat::Flac, RecipeQuality::Cd) => "FLAC, up to CD quality".to_owned(),
            (format, quality) => format!("{} {quality}", format.as_str().to_uppercase()),
        })
        .collect();
    format!("the recipe {}", steps.join(", then "))
}
