//! The committed OpenAPI snapshot matches the document the code builds.
//! `server/openapi/check.sh` diffs the generated TypeScript in CI.

use droppedneedle::docs::ApiDoc;
use utoipa::OpenApi as _;

#[test]
fn committed_openapi_snapshot_matches_code() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/openapi/openapi.json");
    let committed = std::fs::read_to_string(path).unwrap();
    let committed_json: serde_json::Value = serde_json::from_str(&committed).unwrap();
    let fresh_json = serde_json::to_value(ApiDoc::openapi()).unwrap();
    assert_eq!(
        committed_json, fresh_json,
        "openapi.json drifted; run server/openapi/check.sh --write, then review the diff"
    );
}

/// The equality check above cannot catch a mounted route nobody told utoipa
/// about, so the trigger routes mounted outside the modules that document
/// them pin their paths here by name.
#[test]
fn openapi_covers_routes_mounted_outside_their_slices() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/openapi/openapi.json");
    let committed = std::fs::read_to_string(path).unwrap();
    let committed_json: serde_json::Value = serde_json::from_str(&committed).unwrap();
    for route in [
        "/api/v3/settings/navidrome/playlist-sync",
        "/api/v3/admin/precache/run",
    ] {
        assert!(
            committed_json["paths"][route]["post"].is_object(),
            "{route} is mounted but missing from the snapshot; register its utoipa path"
        );
    }
}
