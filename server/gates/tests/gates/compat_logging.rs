//! Compat request addresses stay out of the logs.

use crate::common::sources::workspace_sources;

/// Compat clients put app passwords in the query string, so source code may
/// only read a request URI's path, query or host, and never log a URI.
#[test]
fn access_logging_records_path_only() {
    let mut violations = Vec::new();
    for source in workspace_sources() {
        for (number, line) in source.text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            let full_target =
                line.contains("uri().path_and_query") || line.contains("uri().to_string()");
            // Match `uri` as an identifier so words like "during" pass.
            let names_uri = line
                .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
                .any(|word| word == "uri" || word.ends_with("_uri") || word.starts_with("uri_"));
            let logged = line.contains("tracing::") && names_uri;
            if full_target || logged {
                violations.push(format!("{}:{}: {line}", source.path, number + 1));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "full-target URI reads:\n{}",
        violations.join("\n")
    );
}
