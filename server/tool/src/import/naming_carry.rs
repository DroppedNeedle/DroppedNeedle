//! What an import makes of a v2 naming template. v3 places imports with a
//! naming script, so a template is rewritten here, once, by the library's one
//! converter. Every step is a pure function of the file, so a re-import writes
//! nothing and a dry run reports the same lines.

use droppedneedle_kernel::runtime_config::secret_sections::DEFAULT_NAMING_SCRIPT;
use droppedneedle_library::import::naming;
use droppedneedle_library::management::settings::legacy_template_to_script;
use serde_json::Value;

use super::report::{DROPPED_SETTING, ReportBuilder};

const TEMPLATE: &str = "naming_template";
const SCRIPT: &str = "naming_script";

/// Turn a `library_settings` section's template into its import naming
/// script. A template that can't be carried leaves the default script and a
/// report line, and never fails the import. A section with no template reads
/// as the default script as it is.
pub(super) fn convert_template(section: &mut Value, report: &mut ReportBuilder) {
    let Some(object) = section.as_object_mut() else {
        return;
    };
    let Some(old) = object.remove(TEMPLATE) else {
        return;
    };
    let script = match old.as_str() {
        Some(template) => carried_script(template, report),
        None => default_instead(&old.to_string(), "it is not text", report),
    };
    object.insert(SCRIPT.to_owned(), Value::String(script));
}

fn carried_script(template: &str, report: &mut ReportBuilder) -> String {
    let converted = legacy_template_to_script(template);
    if let Err(error) = naming::check(&converted.script) {
        return default_instead(template, &error.to_string(), report);
    }
    if converted.script != DEFAULT_NAMING_SCRIPT {
        note(
            report,
            format!(
                "the v2 naming template {template} is now the import naming script {}",
                converted.script
            ),
        );
    }
    for warning in converted.warnings {
        report.add_note(format!("Naming script: {warning}"));
        note(report, warning);
    }
    converted.script
}

fn default_instead(template: &str, reason: &str, report: &mut ReportBuilder) -> String {
    let detail = format!(
        "the v2 naming template {template} can't be carried, so imports use the default \
         naming script: {reason}"
    );
    report.add_note(format!("Naming script: {detail}"));
    report.note(
        "settings",
        format!("library_settings.{TEMPLATE}"),
        DROPPED_SETTING,
        detail,
    );
    DEFAULT_NAMING_SCRIPT.to_owned()
}

fn note(report: &mut ReportBuilder, detail: String) {
    report.note(
        "settings",
        format!("library_settings.{SCRIPT}"),
        "imported",
        detail,
    );
}
