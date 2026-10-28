//! Library Management: the profile routes (create, export, import
//! preview, import) over HTTP, and profile sharing against the v2 golden
//! bundle (export bytes and preview ids), name collisions on import, and
//! refusal of malformed bundles.

use crate::common;

use std::path::PathBuf;

use common::FixedIdGenerator;
use droppedneedle_kernel::runtime_config::sections::{
    LibraryManagement, LibraryManagementProfile, NamingScript, TaggingScript,
};
use droppedneedle_library::management::settings::{
    export_profile_bundle, materialize_profile_bundle, parse_profile_bundle,
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
    let bundle = export_profile_bundle(&golden_profile(), &golden_naming(), &golden_tagging())
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
    let parsed = parse_profile_bundle(&fixture("management_bundle_golden.document.txt"))
        .expect("golden parses");
    let first = preview_materialized_profile(&parsed).expect("preview materializes");
    let second = preview_materialized_profile(&parsed).expect("preview materializes again");
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
    let parsed = parse_profile_bundle(&fixture("management_bundle_golden.document.txt"))
        .expect("golden parses");
    let preview = preview_materialized_profile(&parsed).expect("preview materializes");
    let mut settings = LibraryManagement::default();
    settings.profiles.push(preview.profile.clone());
    settings
        .naming_scripts
        .extend(preview.naming_scripts.clone());
    settings
        .tagging_scripts
        .extend(preview.tagging_scripts.clone());
    let resolved = resolve_import_names(&preview, &settings).expect("names resolve");
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
    let error = materialize_profile_bundle(&legacy, &profile_id, &naming, &tagging)
        .expect_err("legacy Preserve refused");
    assert!(format!("{error:?}").contains("Legacy Preserve"));
}

/// Stored settings holding a field entry no version names read again: the
/// entry is dropped and written back without it, and a save of what was
/// read succeeds.
#[test]
fn stored_unknown_field_is_dropped_on_read() {
    use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto};
    use droppedneedle_library::management::settings::service::LibraryManagementService;
    use droppedneedle_library::management::settings::{
        PICARD_ORGANIZER_PROFILE_ID, initial_settings,
    };
    use std::sync::Arc;

    let mut management = serde_json::to_value(initial_settings("")).expect("serializes");
    let profile = management["profiles"]
        .as_array_mut()
        .expect("profiles")
        .iter_mut()
        .find(|profile| profile["id"] == PICARD_ORGANIZER_PROFILE_ID)
        .expect("picard profile");
    profile["metadata"]["fields"]
        .as_array_mut()
        .expect("fields")
        .push(serde_json::json!({"field": "album_mood", "mode": "replace"}));
    let dir = common::ScratchDir::new("management-v2-field");
    let path = dir.join("config.json");
    std::fs::write(
        &path,
        serde_json::json!({ "library_management": management }).to_string(),
    )
    .expect("config writes");

    let store = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("store opens"),
    );
    let service = LibraryManagementService::new(store);
    let read = service.get_settings().expect("the section reads");
    let stored: String = rusqlite::Connection::open(dir.join("settings.db"))
        .expect("settings database opens")
        .query_row(
            "SELECT value_json FROM config_sections WHERE key = 'library_management'",
            [],
            |row| row.get(0),
        )
        .expect("the section is stored");
    assert!(
        !stored.contains("album_mood"),
        "the read writes the section back without the field"
    );
    let saved = service
        .save_settings(read.settings, &read.settings_revision)
        .expect("what was read saves");
    let has_unknown = saved
        .settings
        .profiles
        .iter()
        .flat_map(|profile| &profile.metadata.fields)
        .any(|field| field.field == "album_mood");
    assert!(!has_unknown);
}

/// Scripts are checked by the compiler management runs them with. A
/// stored script it refuses neither fails the read nor blocks a save that
/// leaves it untouched; a new or edited script it refuses is refused with
/// its line and column, without breaking later saves; a tagging script
/// using `else:` saves.
#[test]
fn scripts_validate_with_the_running_compiler() {
    use droppedneedle_kernel::runtime_config::settings_error::SettingsError;
    use droppedneedle_kernel::runtime_config::{ConfigStore, Crypto};
    use droppedneedle_library::management::settings::initial_settings;
    use droppedneedle_library::management::settings::service::LibraryManagementService;
    use std::sync::Arc;

    let naming = |id: &str, name: &str, source: &str| -> NamingScript {
        serde_json::from_value(serde_json::json!({"id": id, "name": name, "source": source}))
            .expect("script")
    };
    let mut management = initial_settings("");
    management.naming_scripts.push(naming(
        "66666666-6666-4666-8666-666666666666",
        "Shouting",
        "{albumartist}/{shout(title)}",
    ));
    let dir = common::ScratchDir::new("management-script-compile");
    let path = dir.join("config.json");
    std::fs::write(
        &path,
        serde_json::json!({ "library_management": management }).to_string(),
    )
    .expect("config writes");
    let store = Arc::new(
        ConfigStore::scratch(&dir, Crypto::from_key_bytes(&[7u8; 32]).expect("key"))
            .expect("store opens"),
    );
    let service = LibraryManagementService::new(store);

    let read = service
        .get_settings()
        .expect("a stored bad script still reads");
    let mut proposed = read.settings;
    proposed.tagging_scripts.push(
        serde_json::from_value(serde_json::json!({
            "id": "77777777-7777-4777-8777-777777777777",
            "name": "Branches",
            "source": "if is_empty(genre):\n  set genre = \"Unknown\"\nelse:\n  set genre = upper(genre)\nend"
        }))
        .expect("script"),
    );
    let saved = service
        .save_settings(proposed, &read.settings_revision)
        .expect("else: saves beside an untouched refused script");

    let refused = |settings: LibraryManagement| {
        let error = service
            .save_settings(settings, &saved.settings_revision)
            .expect_err("the script is refused");
        let SettingsError::InvalidInput { message } = error else {
            panic!("expected a caller fault, got {error:?}");
        };
        message
    };
    let mut edited = saved.settings.clone();
    if let Some(script) = edited
        .naming_scripts
        .iter_mut()
        .find(|script| script.name == "Shouting")
    {
        script.source = "{albumartist}/{album}/{shout(title)}".to_owned();
    }
    assert_eq!(
        refused(edited),
        "Shouting line 1, column 24: Unknown function: shout."
    );
    let mut wide = saved.settings.clone();
    wide.naming_scripts.push(naming(
        "88888888-8888-4888-8888-888888888888",
        "Wide",
        "{track:070000d}",
    ));
    assert_eq!(
        refused(wide),
        "Wide line 1, column 1: Numeric format 070000d is not valid."
    );

    service
        .save_settings(saved.settings.clone(), &saved.settings_revision)
        .expect("a later save still works");
}

/// One admin creates a profile, exports it, previews the share code, and
/// imports it back under a new name. Imports never overwrite: the copy
/// lands beside the original, and a stale revision is refused.
#[tokio::test]
async fn profile_create_export_preview_import_round_trip() {
    use crate::common::auth::{AuthRig, test_principal, with_test_principal};
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use droppedneedle::settings::wiring::SettingsSetup;
    use droppedneedle_auth::users::roles::{Role, SessionKind};
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tower::ServiceExt as _;

    let rig = AuthRig::new("settings-profiles").await;
    let admin = rig.seed_user("brenda", Role::Admin).await;
    let settings = SettingsSetup::for_tests(Arc::new(FixedIdGenerator::new(common::FIXED_ID)))
        .expect("settings bundle builds");
    let app = with_test_principal(
        droppedneedle::settings::wiring::routes()
            .with_state(settings)
            .into_router(),
        test_principal(&admin.id, Role::Admin, "sess-1", SessionKind::Standard),
    );
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
    let base = "/api/v3/settings/library-management";

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

// ---------------------------------------------------------------------------
// Profile fields: one name each, completed from the stored shape
// ---------------------------------------------------------------------------

mod fields {
    use std::collections::BTreeMap;

    use droppedneedle_kernel::runtime_config::sections::RelationshipType;
    use droppedneedle_kernel::runtime_config::sections::{
        FieldMode, LibraryManagement, LibraryManagementRootAssignment, ManagedField, TaggingScript,
    };
    use droppedneedle_library::management::enrich::{AlbumExtras, LyricsAnswer};
    use droppedneedle_library::management::planner::Overrides;
    use droppedneedle_library::management::profile::{self, CompiledScripts, FileInput};
    use droppedneedle_library::management::settings::{self as lm, PICARD_ORGANIZER_PROFILE_ID};
    use droppedneedle_library::matching::model::{
        RELATIONS_VERSION, RecordingRelations, ReleaseMedium, ReleaseRelations, RoleCredit,
    };
    use droppedneedle_library::matching::{CreditedArtist, Release, ReleaseTrack};
    use droppedneedle_library::publish::TagDocument;
    use droppedneedle_library::tags::{TagField, picard};

    /// The Picard preset's fields as stored at version 4: alias spellings
    /// beside their fields, `genre` and `publisher` entries, and no entry
    /// of their own for artist sort, the release facts or original date.
    const STORED_V4_FIELDS: &[&str] = &[
        "title",
        "artist",
        "artists",
        "album",
        "albumartist",
        "album_artist",
        "albumartists",
        "album_artists",
        "track_number",
        "track",
        "disc_number",
        "disc",
        "total_tracks",
        "total_discs",
        "year",
        "date",
        "genre",
        "comment",
        "composer",
        "lyricist",
        "conductor",
        "performer",
        "arranger",
        "remixer",
        "producer",
        "publisher",
        "copyright",
        "encoded_by",
        "isrc",
        "barcode",
        "catalog_number",
        "label",
        "media",
        "musicbrainz_recording_id",
        "musicbrainz_release_id",
        "musicbrainz_release_group_id",
        "musicbrainz_artist_id",
        "musicbrainz_album_artist_id",
        "musicbrainz_work_id",
        "acoustid_id",
        "acoustid_fingerprint",
    ];

    fn entry(name: &str, mode: FieldMode) -> ManagedField {
        ManagedField {
            field: name.to_owned(),
            mode,
            clear_when_canonical_missing: false,
        }
    }

    /// Fresh settings whose Picard profile has these fields, stored as
    /// the version 4 preset left them.
    fn stored(fields: Vec<ManagedField>, preserve: &[&str]) -> LibraryManagement {
        let mut settings = lm::initial_settings("");
        let picard = settings
            .profiles
            .iter_mut()
            .find(|profile| profile.id == PICARD_ORGANIZER_PROFILE_ID)
            .expect("picard preset");
        picard.metadata.fields = fields;
        picard.metadata.preserve_fields = preserve.iter().map(|name| (*name).to_owned()).collect();
        picard.preset_version = Some(4);
        picard.revision = lm::profile_revision(picard);
        settings
    }

    fn stored_v4(preserve: &[&str]) -> LibraryManagement {
        let fields = STORED_V4_FIELDS
            .iter()
            .map(|name| {
                let mode = if name.starts_with("acoustid_") {
                    FieldMode::FillMissing
                } else {
                    FieldMode::Replace
                };
                entry(name, mode)
            })
            .collect();
        stored(fields, preserve)
    }

    fn modes(settings: &LibraryManagement) -> BTreeMap<String, (FieldMode, bool)> {
        settings
            .profiles
            .iter()
            .find(|profile| profile.id == PICARD_ORGANIZER_PROFILE_ID)
            .expect("picard preset")
            .metadata
            .fields
            .iter()
            .map(|field| {
                (
                    field.field.clone(),
                    (field.mode, field.clear_when_canonical_missing),
                )
            })
            .collect()
    }

    fn artist(name: &str, sort: &str) -> CreditedArtist {
        CreditedArtist {
            id: format!("{name}-id"),
            name: name.to_owned(),
            sort_name: Some(sort.to_owned()),
            join: String::new(),
        }
    }

    /// A release with every fact the Picard tags read.
    fn release() -> Release {
        Release {
            id: "11111111-1111-4111-8111-111111111111".to_owned(),
            release_group_id: "22222222-2222-4222-8222-222222222222".to_owned(),
            title: "The Album".to_owned(),
            artists: vec![artist("The Band", "Band, The")],
            date: Some("2001-02-03".to_owned()),
            original_date: Some("1999".to_owned()),
            country: Some("GB".to_owned()),
            status: Some("Official".to_owned()),
            barcode: Some("5012345678900".to_owned()),
            asin: Some("B000000001".to_owned()),
            primary_type: Some("Album".to_owned()),
            secondary_types: vec!["Live".to_owned()],
            labels: vec!["Label One".to_owned()],
            catalog_numbers: vec!["CAT-1".to_owned()],
            media: vec![ReleaseMedium {
                position: 1,
                format: Some("CD".to_owned()),
                title: Some("The First Disc".to_owned()),
                track_count: 1,
            }],
            tracks: vec![ReleaseTrack {
                id: "33333333-3333-4333-8333-333333333333".to_owned(),
                recording_id: "44444444-4444-4444-8444-444444444444".to_owned(),
                title: "The Song".to_owned(),
                artists: vec![artist("The Singer", "Singer, The")],
                disc: 1,
                position: 1,
                absolute_position: 1,
                isrcs: vec!["GBAAA0100001".to_owned()],
                ..ReleaseTrack::default()
            }],
            ..Release::default()
        }
    }

    /// A file holding older values for some of the fields.
    fn file() -> TagDocument {
        let managed: BTreeMap<String, Vec<String>> = [
            ("title", "Old Title"),
            ("album_artist", "Old Album Artist"),
            ("artist_sort", "Old Sort"),
            ("original_date", "1980"),
            ("copyright", "(c) Someone"),
        ]
        .iter()
        .map(|(name, value)| ((*name).to_owned(), vec![(*value).to_owned()]))
        .collect();
        serde_json::from_value(serde_json::json!({"managed": managed, "custom": {}}))
            .expect("document")
    }

    /// The tags planning gives the file under the Picard profile.
    fn plan(settings: &LibraryManagement) -> BTreeMap<String, Vec<String>> {
        plan_with(settings, &Overrides::default())
    }

    /// [`plan`] with the file's saved tag edits.
    fn plan_with(
        settings: &LibraryManagement,
        overrides: &Overrides,
    ) -> BTreeMap<String, Vec<String>> {
        plan_release(settings, overrides, &release())
    }

    /// [`plan`] for a release whose track MusicBrainz credits an engineer
    /// and a composer.
    fn plan_engineered(settings: &LibraryManagement) -> BTreeMap<String, Vec<String>> {
        let mut release = release();
        let credit = |role: &str, name: &str| RoleCredit {
            role: role.to_owned(),
            relation_type: role.to_owned(),
            name: name.to_owned(),
            artist_id: format!("{name}-id"),
            ..RoleCredit::default()
        };
        let mut relations = ReleaseRelations {
            version: RELATIONS_VERSION,
            ..ReleaseRelations::default()
        };
        relations.tracks.insert(
            release.tracks[0].id.clone(),
            RecordingRelations {
                recording_id: release.tracks[0].recording_id.clone(),
                credits: vec![
                    credit("engineer", "Eve Engineer"),
                    credit("composer", "Cy Composer"),
                ],
                works: Vec::new(),
            },
        );
        release.graft_relations(&relations);
        plan_release(settings, &Overrides::default(), &release)
    }

    fn plan_release(
        settings: &LibraryManagement,
        overrides: &Overrides,
        release: &Release,
    ) -> BTreeMap<String, Vec<String>> {
        let pinned = profile::pin(settings, PICARD_ORGANIZER_PROFILE_ID, None).expect("pinned");
        let scripts = CompiledScripts::new(&pinned);
        let doc = file();
        let plan = profile::plan_tags(
            &pinned,
            &scripts,
            &FileInput {
                release,
                track: &release.tracks[0],
                doc: &doc,
                format: "flac",
                id3v23: false,
                writable: true,
                overrides,
                extras: &AlbumExtras::default(),
                track_id: "track-1",
                lyrics: &LyricsAnswer::Off,
            },
        )
        .unwrap_or_else(|reason| panic!("planned: {reason:?}"));
        plan.desired
    }

    /// The version 4 Picard preset reads as one entry per field, each
    /// writing what it wrote before: the folded spellings, artist sort,
    /// the release facts and original date under their parents' modes,
    /// and the names it never listed off. Planning gives the file every
    /// Picard tag over its own values; ISRC and album artists, which that
    /// profile listed but never wrote, are now written too.
    #[test]
    fn a_stored_profile_completes_and_plans_as_before() {
        let mut settings = stored_v4(&[]);
        // A clear on comment never removed anything, and still does not.
        for field in &mut settings.profiles[0].metadata.fields {
            field.clear_when_canonical_missing |= field.field == "comment";
        }
        lm::normalize(&mut settings).expect("valid");
        let mut expected = modes(&lm::initial_settings(""));
        for name in [
            "title_sort",
            "album_sort",
            "work",
            "movement",
            "movement_number",
            "movement_count",
        ] {
            expected.insert(name.to_owned(), (FieldMode::Disabled, false));
        }
        assert_eq!(modes(&settings), expected);

        let mut expected = file().managed.named();
        expected.extend(picard::release_tags(&release(), &release().tracks[0]));
        assert!(expected.contains_key("isrc") && expected.contains_key("album_artists"));
        assert_eq!(plan(&settings), expected);

        // An ISRC saved in the tag editor still wins over the profile's.
        let by_hand = vec!["XXHAND0000001".to_owned()];
        let overrides = Overrides {
            fields: vec![(TagField::Isrc, "replace".to_owned(), by_hand.clone())],
            revision: 1,
        };
        assert_eq!(plan_with(&settings, &overrides)["isrc"], by_hand);
    }

    /// Two spellings of one field: planning applied them in turn, so
    /// replace then fill missing wrote the release's value, and still does.
    #[test]
    fn folded_spellings_keep_the_strongest_mode() {
        let mut settings = stored(
            vec![
                entry("album_artist", FieldMode::Replace),
                entry("albumartist", FieldMode::FillMissing),
            ],
            &[],
        );
        lm::normalize(&mut settings).expect("valid");
        assert_eq!(
            modes(&settings).get("album_artist"),
            Some(&(FieldMode::Replace, false))
        );
        assert!(!modes(&settings).contains_key("albumartist"));
        assert_eq!(plan(&settings)["album_artist"], vec!["The Band".to_owned()]);
    }

    /// Preserving `artist` and `year` kept artist sort and original date
    /// too, and still does.
    #[test]
    fn a_preserved_parent_keeps_what_it_covered() {
        let mut settings = stored_v4(&["artist", "year"]);
        lm::normalize(&mut settings).expect("valid");
        let desired = plan(&settings);
        assert_eq!(desired["artist_sort"], vec!["Old Sort".to_owned()]);
        assert_eq!(desired["original_date"], vec!["1980".to_owned()]);
        assert_eq!(desired["album_artist"], vec!["The Band".to_owned()]);
    }

    /// A complete profile preserving `album` and `date` keeps only those:
    /// the release facts and original date have their own entries now.
    #[test]
    fn a_complete_profile_preserves_only_what_it_names() {
        let named = vec!["album".to_owned(), "date".to_owned()];
        let mut settings = lm::initial_settings("");
        for profile in &mut settings.profiles {
            profile.metadata.preserve_fields.clone_from(&named);
        }
        lm::normalize(&mut settings).expect("valid");
        for profile in &settings.profiles {
            assert_eq!(profile.metadata.preserve_fields, named, "{}", profile.name);
        }
    }

    /// v2's mergeable fields take merge.
    #[test]
    fn label_and_release_type_merge() {
        let mut settings = stored(
            vec![
                entry("label", FieldMode::Merge),
                entry("release_type", FieldMode::Merge),
            ],
            &[],
        );
        lm::normalize(&mut settings).expect("merge validates");
        assert_eq!(
            modes(&settings).get("release_type"),
            Some(&(FieldMode::Merge, false))
        );

        // Merge on a field that holds one value is refused.
        for field in &mut settings.profiles[0].metadata.fields {
            if field.field == "comment" {
                field.mode = FieldMode::Merge;
            }
        }
        assert!(lm::normalize(&mut settings).is_err());
    }

    /// A folder activated under the stored profile stays activated once
    /// the completed settings are read, and once a client saves the
    /// stored shape back.
    /// Activate the folder `music` under the stored Picard profile.
    fn activate(stored: &mut LibraryManagement) {
        let assignment = LibraryManagementRootAssignment {
            root_id: "music".to_owned(),
            profile_id: Some(PICARD_ORGANIZER_PROFILE_ID.to_owned()),
            enabled: true,
            ..LibraryManagementRootAssignment::default()
        };
        let profile = lm::pin_profile(stored, &assignment).expect("profile");
        let (standard, multi_disc) = lm::pin_naming_scripts(stored, &profile).expect("scripts");
        stored
            .root_assignments
            .push(LibraryManagementRootAssignment {
                activation_profile_revision: Some(lm::profile_revision(&profile)),
                activation_naming_policy_revision: Some(lm::naming_policy_revision(
                    &standard,
                    multi_disc.as_ref(),
                )),
                activation_policy_revision: Some("policy".to_owned()),
                activation_preview_token: Some("token".to_owned()),
                activation_preview_hash: Some("hash".to_owned()),
                activation_confirmed_at: Some(1.0),
                ..assignment
            });
    }

    /// Whether the folder [`activate`] activated is still current.
    fn current(settings: &LibraryManagement) -> bool {
        lm::activation_is_current(settings, &settings.root_assignments[0], "policy")
    }

    #[test]
    fn an_activation_survives_completion() {
        let mut stored = stored_v4(&[]);
        activate(&mut stored);
        assert!(current(&stored));

        let mut read = stored.clone();
        lm::normalize(&mut read).expect("valid");
        assert!(current(&read), "current after a read");

        let mut saved = stored.clone();
        let invalidated =
            lm::migration_carry(&read, &mut saved, PICARD_ORGANIZER_PROFILE_ID).expect("carried");
        lm::normalize(&mut saved).expect("valid");
        assert!(invalidated.is_empty(), "{invalidated:?}");
        assert!(current(&saved), "current after a save");
    }

    /// The fields "Other roles" writes.
    const OTHER_ROLE_FIELDS: [&str; 5] = ["engineer", "mixer", "dj_mixer", "writer", "director"];

    /// Fresh settings whose Picard profile was saved before the "Other
    /// roles" fields: every other field has its own entry, and Other is
    /// ticked or not.
    fn without_other_role_fields(other: bool) -> LibraryManagement {
        let mut settings = lm::initial_settings("");
        let picard = settings
            .profiles
            .iter_mut()
            .find(|profile| profile.id == PICARD_ORGANIZER_PROFILE_ID)
            .expect("picard preset");
        picard
            .metadata
            .fields
            .retain(|field| !OTHER_ROLE_FIELDS.contains(&field.field.as_str()));
        if !other {
            picard
                .metadata
                .relationships
                .types
                .retain(|kind| *kind != RelationshipType::Other);
        }
        picard.preset_version = Some(5);
        picard.revision = lm::profile_revision(picard);
        settings
    }

    /// A clear on a field with no source, set once each field had its own
    /// entry, survives the completion that adds the newer fields.
    #[test]
    fn a_clear_on_a_field_with_no_source_is_kept() {
        let mut settings = without_other_role_fields(false);
        for profile in &mut settings.profiles {
            for field in &mut profile.metadata.fields {
                if field.field == "comment" {
                    field.mode = FieldMode::Replace;
                    field.clear_when_canonical_missing = true;
                }
            }
        }
        lm::normalize(&mut settings).expect("valid");
        assert_eq!(
            modes(&settings).get("comment"),
            Some(&(FieldMode::Replace, true))
        );
    }

    /// With Other off, the new fields read as off and the profile plans
    /// what it planned before, an engineer credit included.
    #[test]
    fn without_other_the_new_fields_read_as_off() {
        let stored = without_other_role_fields(false);
        let mut read = stored.clone();
        lm::normalize(&mut read).expect("valid");
        let modes = modes(&read);
        for name in OTHER_ROLE_FIELDS {
            assert_eq!(
                modes.get(name),
                Some(&(FieldMode::Disabled, false)),
                "{name}"
            );
        }
        let planned = plan_engineered(&read);
        assert_eq!(planned, plan_engineered(&stored));
        assert!(!planned.contains_key("engineer"), "{planned:?}");
    }

    /// With Other ticked, the new fields read as replace without clear,
    /// so the profile starts writing them, and a folder activated under it
    /// stays current.
    #[test]
    fn with_other_the_new_fields_read_as_replace() {
        let mut stored = without_other_role_fields(true);
        activate(&mut stored);
        assert!(current(&stored));
        let mut read = stored.clone();
        lm::normalize(&mut read).expect("valid");
        let modes = modes(&read);
        for name in OTHER_ROLE_FIELDS {
            assert_eq!(
                modes.get(name),
                Some(&(FieldMode::Replace, false)),
                "{name}"
            );
        }
        assert!(current(&read), "current after a read");
        assert_eq!(
            plan_engineered(&read)["engineer"],
            vec!["Eve Engineer".to_owned()]
        );
    }

    /// A tagging script that sets the older spellings still validates.
    #[test]
    fn scripts_keep_their_spellings() {
        let mut settings = stored_v4(&[]);
        settings.tagging_scripts.push(
            serde_json::from_value::<TaggingScript>(serde_json::json!({
                "id": "55555555-5555-4555-8555-555555555555",
                "name": "Older names",
                "source": "set albumartist = \"Someone\"\nset publisher = \"Label\""
            }))
            .expect("script"),
        );
        lm::normalize(&mut settings).expect("the script validates");
    }
}
