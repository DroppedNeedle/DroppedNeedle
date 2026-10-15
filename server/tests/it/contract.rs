//! The committed OpenAPI snapshot matches the document the code builds.
//! `server/openapi/check.sh` diffs the generated TypeScript, and in CI
//! takes the document from the shipping binary.

use std::collections::BTreeSet;

use droppedneedle::docs::ApiDoc;
use droppedneedle_auth::session::allowlist::{PUBLIC_PATHS, PUBLIC_PREFIXES};
use utoipa::OpenApi as _;

#[test]
fn committed_openapi_snapshot_matches_code() {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/openapi/openapi.json");
    let committed = std::fs::read_to_string(path).unwrap();
    let committed_json: serde_json::Value = serde_json::from_str(&committed).unwrap();
    let fresh_json = serde_json::to_value(ApiDoc::openapi()).unwrap();
    assert_eq!(
        committed_json, fresh_json,
        "openapi.json drifted; run make contract-write, then review the diff"
    );
}

/// Where `write_openapi_document` puts the document.
const OPENAPI_OUT_ENV: &str = "DN_OPENAPI_OUT";

/// Writes the document, byte for byte what `--print-openapi` prints, so
/// `server/openapi/check.sh --from-tests` can reuse this binary instead of
/// building the server a second time.
#[test]
#[ignore = "writes the OpenAPI document for server/openapi/check.sh --from-tests"]
fn write_openapi_document() {
    let out = std::env::var_os(OPENAPI_OUT_ENV)
        .unwrap_or_else(|| panic!("{OPENAPI_OUT_ENV} names the file to write"));
    let mut document = serde_json::to_string_pretty(&ApiDoc::openapi()).unwrap();
    document.push('\n');
    std::fs::write(out, document).unwrap();
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

#[test]
fn every_public_path_is_a_mounted_route() {
    let doc = ApiDoc::openapi();
    for path in PUBLIC_PATHS {
        assert!(doc.paths.paths.contains_key(*path), "{path} has no route");
    }
    for prefix in PUBLIC_PREFIXES {
        assert!(
            doc.paths.paths.keys().any(|path| path.starts_with(prefix)),
            "{prefix} has no route"
        );
    }
}
