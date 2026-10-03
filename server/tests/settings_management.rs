//! Briefs for Library Management profile sharing: the v2 golden
//! bundle pins the export bytes, and the rejection briefs pin every
//! documented import failure.

mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;

use common::FixedIdGenerator;
use droppedneedle::settings::management::{
    StructuralCompiler, export_profile_bundle, materialize_profile_bundle, parse_profile_bundle,
    preview_materialized_profile, profile_aspects, profile_bundle_filename,
    profile_import_warnings, resolve_import_names, unique_import_name,
};
use droppedneedle::settings::models::{
    LibraryManagementProfileDto, LibraryManagementSettingsDto, NamingScriptDto, TaggingScriptDto,
};

fn fixture(name: &str) -> String {
    let path: PathBuf = [
        env!("CARGO_MANIFEST_DIR"),
        "tests",
        "fixtures",
        "settings",
        name,
    ]
    .iter()
    .collect();
    std::fs::read_to_string(&path).expect("golden fixture reads")
}

/// The exact profile input the v2 golden was minted from (same dict,
/// same ids). DTO defaults fill the rest, so this also pins default
/// parity with the v2 schema.
fn golden_profile_input() -> serde_json::Value {
    serde_json::json!({
        "id": "11111111-1111-4111-8111-111111111111",
        "name": "Golden Export",
        "description": "Café test — unicode ✓",
        "revision": "abc123",
        "metadata": {
            "enabled": true,
            "tagging_script_ids": ["22222222-2222-4222-8222-222222222222"],
            "fields": [
                {"field": "title", "mode": "replace", "clear_when_canonical_missing": true}
            ]
        },
        "organization": {
            "rename_enabled": true,
            "move_enabled": true,
            "naming_script_id": "33333333-3333-4333-8333-333333333333",
            "multi_disc_naming_script_id": null
        },
        "artwork": {
            "external_enabled": true,
            "external_naming_script_id": null,
            "overwrite_external_files": true
        },
        "enrichment": {"lyrics": {"enabled": true, "preserve_existing": false}}
    })
}

fn golden_profile() -> LibraryManagementProfileDto {
    serde_json::from_value(golden_profile_input()).expect("golden input deserializes")
}

fn golden_naming() -> Vec<NamingScriptDto> {
    vec![
        serde_json::from_value(serde_json::json!({
            "id": "33333333-3333-4333-8333-333333333333",
            "name": "Standard",
            "source": "{title}"
        }))
        .expect("naming script deserializes"),
    ]
}

fn golden_tagging() -> Vec<TaggingScriptDto> {
    vec![
        serde_json::from_value(serde_json::json!({
            "id": "22222222-2222-4222-8222-222222222222",
            "name": "Tag",
            "source": "set genre = \"Rock\""
        }))
        .expect("tagging script deserializes"),
    ]
}

#[test]
fn export_matches_v2_golden_bytes() {
    let ids = FixedIdGenerator::new(common::FIXED_ID);
    let bundle =
        export_profile_bundle(&golden_profile(), &golden_naming(), &golden_tagging(), &ids)
            .expect("golden profile exports");
    assert_eq!(
        bundle.bundle_hash,
        "c901f186baa1376804a62f00d254a781967a46682fe60f42bff0f57275896045"
    );
    assert_eq!(
        bundle.document,
        fixture("management_bundle_golden.document.txt")
    );
    // Share codes are zlib streams: v2 (CPython zlib) and v3 (miniz)
    // emit different but equally valid bytes, so the share code pins
    // a round-trip to the same hash instead of exact bytes.
    assert!(bundle.share_code.starts_with("DNLP1:"));
    let reparsed = parse_profile_bundle(&bundle.share_code).expect("our share code parses");
    assert_eq!(reparsed.bundle_hash, bundle.bundle_hash);
}

#[test]
fn v2_share_code_parses_with_same_hash() {
    let from_code = parse_profile_bundle(&fixture("management_bundle_golden.share_code.txt"))
        .expect("v2 share code parses");
    let from_document = parse_profile_bundle(&fixture("management_bundle_golden.document.txt"))
        .expect("v2 document parses");
    assert_eq!(from_code.bundle_hash, from_document.bundle_hash);
    assert_eq!(
        from_code.bundle_hash,
        "c901f186baa1376804a62f00d254a781967a46682fe60f42bff0f57275896045"
    );
}

#[test]
fn preview_is_deterministic_and_names_aspects_and_warnings() {
    let compiler = StructuralCompiler;
    let parsed = parse_profile_bundle(&fixture("management_bundle_golden.document.txt"))
        .expect("golden parses");
    let first = preview_materialized_profile(&parsed, &compiler).expect("preview materializes");
    let second =
        preview_materialized_profile(&parsed, &compiler).expect("preview materializes again");
    // Deterministic preview ids, byte-equal to v2's UUIDv5 values.
    assert_eq!(first.profile.id, "7bc82f44-e135-5c2f-879e-9c407d49248f");
    assert_eq!(first.profile.id, second.profile.id);
    assert_eq!(
        first
            .naming_scripts
            .iter()
            .map(|script| &script.id)
            .collect::<Vec<_>>(),
        second
            .naming_scripts
            .iter()
            .map(|script| &script.id)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        profile_aspects(&first.profile),
        vec![
            "Metadata tags",
            "Genres",
            "Artwork",
            "Lyrics",
            "Rename files",
            "Move files"
        ]
    );
    let warnings = profile_import_warnings(&first.profile);
    let codes: Vec<&str> = warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect();
    assert!(codes.contains(&"clear_missing_metadata"));
    assert!(codes.contains(&"remove_sources"));
    assert!(codes.contains(&"overwrite_external_artwork"));
    assert!(codes.contains(&"replace_enrichment"));
}

#[test]
fn import_renames_collisions_and_resolves() {
    let compiler = StructuralCompiler;
    let parsed = parse_profile_bundle(&fixture("management_bundle_golden.document.txt"))
        .expect("golden parses");
    let preview = preview_materialized_profile(&parsed, &compiler).expect("preview materializes");
    let mut settings = LibraryManagementSettingsDto::default();
    settings.profiles.push(preview.profile.clone());
    settings
        .naming_scripts
        .extend(preview.naming_scripts.clone());
    settings
        .tagging_scripts
        .extend(preview.tagging_scripts.clone());
    let resolved = resolve_import_names(&preview, &settings, &compiler).expect("names resolve");
    assert_eq!(resolved.profile.name, "Golden Export (imported)");
    assert_eq!(resolved.naming_scripts[0].name, "Standard (imported)");
    assert_eq!(resolved.tagging_scripts[0].name, "Tag (imported)");
}

#[test]
fn unique_import_name_counts_up_case_insensitively() {
    let used: BTreeSet<String> = ["mix", "mix (imported)"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    assert_eq!(unique_import_name("Fresh", &used), "Fresh");
    assert_eq!(unique_import_name("MIX", &used), "MIX (imported 2)");
}

#[test]
fn bundle_filename_slugs_unicode() {
    assert_eq!(
        profile_bundle_filename("Golden Export"),
        "golden-export.dnprofile"
    );
    assert_eq!(
        profile_bundle_filename("Café — Mix ✓"),
        "cafe-mix.dnprofile"
    );
    assert_eq!(profile_bundle_filename("✓✓✓"), "library-profile.dnprofile");
}

#[test]
fn unknown_profile_field_names_its_path() {
    let mut document: serde_json::Value =
        serde_json::from_str(&fixture("management_bundle_golden.document.txt"))
            .expect("golden document is JSON");
    document["payload"]["profile"]["metadata"]["bogus"] = serde_json::json!(1);
    let error = parse_profile_bundle(&document.to_string()).expect_err("bogus field refused");
    assert_eq!(
        format!("{error:?}"),
        "InvalidInput { message: \"Unknown or unsupported profile field: metadata.bogus.\" }"
    );
}

#[test]
fn tampered_checksum_is_refused() {
    let tampered = fixture("management_bundle_golden.document.txt").replacen(
        "Golden Export",
        "Golden Exporx",
        1,
    );
    let error = parse_profile_bundle(&tampered).expect_err("tampered bundle refused");
    assert!(format!("{error:?}").contains("checksum does not match"));
}

#[test]
fn wrong_format_and_empty_content_are_refused() {
    let mut document: serde_json::Value =
        serde_json::from_str(&fixture("management_bundle_golden.document.txt"))
            .expect("golden document is JSON");
    document["format"] = serde_json::json!("something-else");
    let error = parse_profile_bundle(&document.to_string()).expect_err("format refused");
    assert!(format!("{error:?}").contains("not a DroppedNeedle Library Management profile"));
    let error = parse_profile_bundle("   ").expect_err("empty refused");
    assert!(format!("{error:?}").contains("Paste a profile code"));
}

#[test]
fn truncated_share_code_is_refused() {
    let code = fixture("management_bundle_golden.share_code.txt");
    let truncated = &code[..code.len() / 2];
    assert!(parse_profile_bundle(truncated).is_err());
    assert!(parse_profile_bundle("DNLP1:").is_err());
}

#[test]
fn export_refuses_a_missing_script() {
    let ids = FixedIdGenerator::new(common::FIXED_ID);
    let error = export_profile_bundle(&golden_profile(), &[], &golden_tagging(), &ids)
        .expect_err("missing naming script refused");
    assert!(format!("{error:?}").contains("no longer available"));
}

#[test]
fn materialize_refuses_legacy_settings() {
    let compiler = StructuralCompiler;
    let parsed = parse_profile_bundle(&fixture("management_bundle_golden.document.txt"))
        .expect("golden parses");
    // A PortablePayload with a Preserve field trips the legacy refusal
    // before any id mapping runs.
    let mut legacy = parsed.clone();
    legacy.payload.profile["metadata"]["fields"] =
        serde_json::json!([{"field": "title", "mode": "preserve"}]);
    let naming: std::collections::BTreeMap<String, String> =
        [("naming-1".to_owned(), uuid::Uuid::new_v4().to_string())]
            .into_iter()
            .collect();
    let tagging: std::collections::BTreeMap<String, String> =
        [("tagging-1".to_owned(), uuid::Uuid::new_v4().to_string())]
            .into_iter()
            .collect();
    let profile_id = uuid::Uuid::new_v4().to_string();
    let error = materialize_profile_bundle(&legacy, &profile_id, &naming, &tagging, &compiler)
        .expect_err("legacy Preserve refused");
    assert!(format!("{error:?}").contains("Legacy Preserve"));
}
