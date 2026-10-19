//! The committed OpenAPI snapshot matches the document the code builds.
//! `server/openapi/check.sh` diffs the generated TypeScript, and in CI
//! takes the document from the shipping binary.

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

/// A tag left unset defaults to the module path of its handler, so moving a
/// handler between crates would rename the tag and change the document.
/// Every operation names exactly one tag, and no tag is a module path.
#[test]
fn every_operation_has_one_named_tag() {
    let doc = serde_json::to_value(ApiDoc::openapi()).unwrap();
    let mut wrong = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        let operations = item.as_object().unwrap().iter().filter(|(key, _)| {
            matches!(
                key.as_str(),
                "get" | "put" | "post" | "delete" | "options" | "head" | "patch" | "trace"
            )
        });
        for (method, operation) in operations {
            let tags: Vec<&str> = operation["tags"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|tag| tag.as_str())
                .collect();
            if !matches!(tags.as_slice(), [tag] if !tag.contains("::")) {
                wrong.push(format!("{method} {path}: tags {tags:?}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "operations without exactly one named tag: {wrong:#?}"
    );
}

/// Operation ids name the generated client's operations, so two routes
/// sharing one would collapse into a single typed entry.
#[test]
fn operation_ids_are_unique() {
    let doc = serde_json::to_value(ApiDoc::openapi()).unwrap();
    let mut seen = BTreeSet::new();
    let mut repeated = Vec::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, operation) in item.as_object().unwrap() {
            if let Some(id) = operation["operationId"].as_str()
                && !seen.insert(id.to_owned())
            {
                repeated.push(format!("{method} {path}: {id}"));
            }
        }
    }
    assert!(repeated.is_empty(), "repeated operation ids: {repeated:#?}");
}

/// The event stream's documented union lists exactly the event names the
/// hub sends.
#[test]
fn stream_event_union_names_every_event() {
    let doc = serde_json::to_value(ApiDoc::openapi()).unwrap();
    let members = doc["components"]["schemas"]["StreamEvent"]["oneOf"]
        .as_array()
        .expect("StreamEvent is a oneOf");
    let documented: BTreeSet<&str> = members
        .iter()
        .filter_map(|member| member["properties"]["event"]["enum"][0].as_str())
        .collect();
    let sent: BTreeSet<&str> = droppedneedle_kernel::events::model::names::ALL
        .into_iter()
        .collect();
    assert_eq!(documented, sent);
    assert_eq!(members.len(), sent.len());
}
