//! Contract-drift brief (TS-diff gate, Rust half): the committed OpenAPI
//! snapshot matches the document the code builds. The TypeScript half lives
//! in `server/openapi/check.sh` and runs as a CI gate.

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
/// about, so the stage-10 routes mounted outside their slices' handlers pin
/// their documented paths here by name.
#[test]
fn openapi_covers_the_stage10_trigger_routes() {
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
