//! Library Management: the profile routes (create, export, import
//! preview, import) over HTTP, and profile sharing against the v2 golden
//! bundle (export bytes and preview ids), name collisions on import, and
//! refusal of malformed bundles.

use crate::common;

use std::path::PathBuf;

use common::FixedIdGenerator;
use droppedneedle::runtime_config::sections::{
    LibraryManagement, LibraryManagementProfile, NamingScript, TaggingScript,
};
use droppedneedle::settings::management::{
    StructuralCompiler, export_profile_bundle, materialize_profile_bundle, parse_profile_bundle,
    preview_materialized_profile, profile_aspects, profile_import_warnings, resolve_import_names,
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
/// same ids). Section defaults fill the rest, so this also pins default
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

fn golden_profile() -> LibraryManagementProfile {
    serde_json::from_value(golden_profile_input()).expect("golden input deserializes")
}

fn golden_naming() -> Vec<NamingScript> {
    vec![
        serde_json::from_value(serde_json::json!({
            "id": "33333333-3333-4333-8333-333333333333",
            "name": "Standard",
            "source": "{title}"
        }))
        .expect("naming script deserializes"),
    ]
}

fn golden_tagging() -> Vec<TaggingScript> {
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
    let mut settings = LibraryManagement::default();
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

/// Every malformed import is refused with its documented reason.
#[test]
fn bad_bundles_are_refused() {
    let golden = fixture("management_bundle_golden.document.txt");
    let code = fixture("management_bundle_golden.share_code.txt");
    let mut unknown_field: serde_json::Value =
        serde_json::from_str(&golden).expect("golden document is JSON");
    unknown_field["payload"]["profile"]["metadata"]["bogus"] = serde_json::json!(1);
    let mut wrong_format: serde_json::Value =
        serde_json::from_str(&golden).expect("golden document is JSON");
    wrong_format["format"] = serde_json::json!("something-else");
    for (input, reason) in [
        (
            unknown_field.to_string(),
            "Unknown or unsupported profile field: metadata.bogus.",
        ),
        (
            golden.replacen("Golden Export", "Golden Exporx", 1),
            "checksum does not match",
        ),
        (
            wrong_format.to_string(),
            "not a DroppedNeedle Library Management profile",
        ),
        ("   ".to_owned(), "Paste a profile code"),
        (code[..code.len() / 2].to_owned(), ""),
        ("DNLP1:".to_owned(), ""),
    ] {
        let error = parse_profile_bundle(&input).expect_err("refused");
        assert!(format!("{error:?}").contains(reason), "{reason}: {error:?}");
    }
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

/// One admin creates a profile, exports it, previews the share code, and
/// imports it back under a new name. Imports never overwrite: the copy
/// lands beside the original, and a stale revision is refused.
#[tokio::test]
async fn profile_create_export_preview_import_round_trip() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use droppedneedle::auth::session::extract::Transport;
    use droppedneedle::auth::session::middleware::CurrentSession;
    use droppedneedle::auth::users::memory::TestRig;
    use droppedneedle::auth::users::roles::{Role, SessionKind};
    use droppedneedle::settings::wiring::SettingsSetup;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tower::ServiceExt as _;

    let rig = TestRig::new().expect("rig builds");
    let admin = rig.seed_user("brenda", Role::Admin).await;
    let settings = SettingsSetup::for_tests(
        Arc::new(FixedIdGenerator::new(common::FIXED_ID)),
        rig.deps.clone(),
    )
    .expect("settings bundle builds");
    let app = settings.gated_router().layer(axum::middleware::from_fn(
        move |mut req: Request<Body>, next: axum::middleware::Next| {
            let user_id = admin.id.clone();
            async move {
                req.extensions_mut().insert(CurrentSession {
                    user_id,
                    session_id: "sess-1".to_owned(),
                    kind: SessionKind::Standard,
                    transport: Transport::Bearer,
                });
                next.run(req).await
            }
        },
    ));
    let call = |method: &'static str, uri: String, body: Option<Value>| {
        let app = app.clone();
        async move {
            let mut builder = Request::builder().method(method).uri(uri);
            if body.is_some() {
                builder = builder.header("content-type", "application/json");
            }
            let request = builder
                .body(body.map_or_else(Body::empty, |json| Body::from(json.to_string())))
                .expect("request builds");
            let response = app.oneshot(request).await.expect("router answers");
            let status = response.status();
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body reads");
            let json: Value = serde_json::from_slice(&bytes).expect("body is json");
            (status, json)
        }
    };
    let base = "/settings/library-management";

    // First read seeds the presets.
    let (status, seeded) = call("GET", base.to_owned(), None).await;
    assert_eq!(status, StatusCode::OK, "{seeded}");
    assert_eq!(seeded["profiles"].as_array().map(Vec::len), Some(3));
    let revision = seeded["settings_revision"]
        .as_str()
        .expect("revision")
        .to_owned();

    let (status, created) = call(
        "POST",
        format!("{base}/profiles"),
        Some(json!({"name": "Mine", "expected_settings_revision": revision})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let profile_id = created["profile"]["id"].as_str().expect("id").to_owned();
    let revision = created["settings_revision"]
        .as_str()
        .expect("revision")
        .to_owned();

    let (status, exported) = call(
        "POST",
        format!("{base}/profiles/{profile_id}/export"),
        Some(json!({"expected_settings_revision": revision})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{exported}");
    let share_code = exported["share_code"]
        .as_str()
        .expect("share code")
        .to_owned();
    assert!(share_code.starts_with("DNLP1:"));

    let (status, preview) = call(
        "POST",
        format!("{base}/profile-imports/preview"),
        Some(json!({"content": share_code, "expected_settings_revision": revision})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{preview}");
    assert_eq!(preview["profile"]["name"], "Mine (imported)");
    assert_eq!(preview["bundle_hash"], exported["bundle_hash"]);

    let (status, stale) = call(
        "POST",
        format!("{base}/profile-imports"),
        Some(json!({
            "content": share_code,
            "reviewed_bundle_hash": preview["bundle_hash"],
            "name": "Mine again",
            "expected_settings_revision": "stale",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{stale}");

    let (status, imported) = call(
        "POST",
        format!("{base}/profile-imports"),
        Some(json!({
            "content": share_code,
            "reviewed_bundle_hash": preview["bundle_hash"],
            "name": "Mine again",
            "expected_settings_revision": revision,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{imported}");
    assert_eq!(imported["profile"]["name"], "Mine again");
    assert_ne!(imported["profile"]["id"], profile_id.as_str());

    let (_, after) = call("GET", base.to_owned(), None).await;
    let names: Vec<&str> = after["profiles"]
        .as_array()
        .expect("profiles")
        .iter()
        .filter_map(|profile| profile["name"].as_str())
        .collect();
    assert!(
        names.contains(&"Mine") && names.contains(&"Mine again"),
        "{names:?}"
    );
    assert_eq!(after["settings_revision"], imported["settings_revision"]);
}
