//! The committed OpenAPI snapshot matches the document the code builds.
//! `server/openapi/check.sh` diffs the generated TypeScript in CI.

use std::collections::BTreeSet;

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

/// Every operation declares exactly the `{name}` segments of its path as
/// path parameters. A query struct without
/// `#[into_params(parameter_in = Query)]` lands its fields here as path
/// parameters, and a typed client then cannot build the URL.
#[test]
fn path_parameters_match_their_templates() {
    let doc = serde_json::to_value(ApiDoc::openapi()).unwrap();
    let mut wrong = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        let template: BTreeSet<&str> = path
            .split('{')
            .skip(1)
            .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
            .collect();
        let operations = item.as_object().unwrap().iter().filter(|(key, _)| {
            matches!(
                key.as_str(),
                "get" | "put" | "post" | "delete" | "options" | "head" | "patch" | "trace"
            )
        });
        for (method, operation) in operations {
            let declared: BTreeSet<&str> = operation["parameters"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|parameter| parameter["in"] == "path")
                .filter_map(|parameter| parameter["name"].as_str())
                .collect();
            if declared != template {
                wrong.push(format!("{method} {path}: declares {declared:?}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "path parameters off their template: {wrong:#?}"
    );
}
