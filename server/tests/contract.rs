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
