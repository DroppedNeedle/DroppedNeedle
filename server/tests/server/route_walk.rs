//! Every documented path answers 405 to each method the document does not
//! list for it. A `routes!` call naming handlers at two paths mounts every
//! one of them at both, which the document cannot show; this walk does.

use axum::http::StatusCode;
use droppedneedle::docs::ApiDoc;
use utoipa::OpenApi as _;

use crate::common::TestApp;

/// Stands in for every path parameter. It matches no literal segment, so
/// `/plugins/{name}` cannot land on `/plugins/sources`.
const PARAMETER: &str = "route-walk-x";

#[tokio::test]
async fn undocumented_methods_answer_405() {
    let app = TestApp::start("route-walk").await;
    let doc = ApiDoc::openapi();
    let mut wrong = Vec::new();
    for (path, item) in &doc.paths.paths {
        let uri = fill_parameters(path);
        for (method, listed) in [
            ("GET", item.get.is_some()),
            ("POST", item.post.is_some()),
            ("PUT", item.put.is_some()),
            ("PATCH", item.patch.is_some()),
            ("DELETE", item.delete.is_some()),
        ] {
            if listed {
                continue;
            }
            let (status, _, _) = app.raw(method, &uri, &[], None).await;
            if status != StatusCode::METHOD_NOT_ALLOWED {
                wrong.push(format!("{method} {path}: {status}"));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "methods answering that the document does not list: {wrong:#?}"
    );
}

fn fill_parameters(template: &str) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        out.push_str(PARAMETER);
        let end = rest[start..].find('}').expect("balanced template braces");
        rest = &rest[start + end + 1..];
    }
    out.push_str(rest);
    out
}
