//! Plugin manifest: `plugin.toml` at a plugin package's root.
//!
//! The manifest is the contract's front door. The host refuses to load any
//! code before the manifest parses, declares a supported `api_version`, and
//! names only known capabilities. The rules below port v2's manifest module
//! one by one, including its exact rejection messages, so a manifest that
//! fails here fails for the same reason it fails on v2.
//!
//! TOML parsing is a strict subset reader, not a general parser: the slice
//! has no TOML crate, and manifests only need comments, `[table]` and
//! `[[array]]` headers, and string, integer, boolean, and single-line
//! string-array values. Anything outside that subset fails the manifest
//! instead of guessing.

use std::collections::HashMap;
use std::path::Path;

/// Current plugin API version.
pub const PLUGIN_API_VERSION: i64 = 1;
/// Legacy API version, kept byte-identical.
pub const PLUGIN_API_VERSION_LEGACY: i64 = 0;
/// Every API version the host speaks.
pub const SUPPORTED_API_VERSIONS: [i64; 2] = [PLUGIN_API_VERSION_LEGACY, PLUGIN_API_VERSION];

/// Capabilities a v0 manifest may activate.
pub const V0_ACTIVE_CAPABILITIES: &[&str] = &["scrobbler", "purchase_links"];
/// Every capability id a v0 manifest may name (active plus reserved).
pub const V0_KNOWN_CAPABILITIES: &[&str] = &[
    "scrobbler",
    "purchase_links",
    "metadata_provider",
    "streaming_source",
];
/// Capabilities a v1 manifest may activate.
pub const V1_ACTIVE_CAPABILITIES: &[&str] = &[
    "scrobbler",
    "purchase_links",
    "download_client",
    "indexer",
    "subscriber",
    "publisher",
    "metadata_provider",
    "scheduler",
    "streaming_source",
];

/// Active capability ids for one API version.
pub fn active_capabilities(api_version: i64) -> &'static [&'static str] {
    if api_version == PLUGIN_API_VERSION_LEGACY {
        V0_ACTIVE_CAPABILITIES
    } else {
        V1_ACTIVE_CAPABILITIES
    }
}

/// Known capability ids for one API version.
pub fn known_capabilities(api_version: i64) -> &'static [&'static str] {
    if api_version == PLUGIN_API_VERSION_LEGACY {
        V0_KNOWN_CAPABILITIES
    } else {
        V1_ACTIVE_CAPABILITIES
    }
}

/// v1 plugin/source name rule: `^[a-z0-9][a-z0-9-]{0,31}$`.
pub fn valid_v1_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() || bytes.len() > 32 {
        return false;
    }
    let first = bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// v0 plugin name rule: non-empty, alphanumerics plus `-` and `_`.
pub fn valid_legacy_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Route path rule: `^[a-z0-9][a-z0-9/_-]{0,63}$`.
pub fn valid_route_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    let first = bytes[0];
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    bytes[1..].iter().all(|b| {
        b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'/' || *b == b'_' || *b == b'-'
    })
}

/// Indexer target rule: `usenet` or `plugin:<v1-name>`.
pub fn valid_plugin_target(target: &str) -> bool {
    if target == "usenet" {
        return true;
    }
    match target.strip_prefix("plugin:") {
        Some(name) => valid_v1_name(name),
        None => false,
    }
}

/// Plugin source key rule: `plugin:<v1-name>`.
pub fn valid_plugin_key(key: &str) -> bool {
    match key.strip_prefix("plugin:") {
        Some(name) => valid_v1_name(name),
        None => false,
    }
}

/// One admin-editable setting the plugin wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSettingField {
    /// Setting key.
    pub key: String,
    /// Display label (defaults to the key).
    pub label: String,
    /// Help text for the settings UI.
    pub help: String,
    /// Secret values are encrypted at rest and masked on read.
    pub secret: bool,
}

/// Per-capability config from one `[[capability]]` table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginCapabilityConfig {
    /// Capability id.
    pub id: String,
    /// Download-client source key.
    pub source: String,
    /// Indexer target (`usenet` or a plugin source key).
    pub target_source: String,
    /// Display name for the source.
    pub display_name: String,
}

/// Schedule for the `scheduler` capability, from `[schedule]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginScheduleConfig {
    /// Tick interval in minutes, 5 to 1440.
    pub interval_minutes: i64,
    /// Fire once right after load instead of waiting out the interval.
    pub run_on_load: bool,
}

/// One `[[route]]` table: plugin HTTP served under `/ext/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginRouteSpec {
    /// Subpath below the plugin's `/ext/` prefix.
    pub path: String,
    /// GET, POST, or DELETE.
    pub method: String,
    /// `admin` or `user`.
    pub auth: String,
    /// Per-minute request budget for one caller on this route.
    pub rate_limit_per_minute: i64,
}

/// A validated plugin manifest.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PluginManifest {
    /// Unique id, kebab-case.
    pub name: String,
    /// Plugin version string.
    pub version: String,
    /// Contract version, 0 or 1.
    pub api_version: i64,
    /// `<module>:<ClassName>` inside the plugin package.
    pub entrypoint: String,
    /// Declared capability ids.
    pub capabilities: Vec<String>,
    /// Display name (defaults to the name).
    pub display_name: String,
    /// Short description.
    pub description: String,
    /// Author string.
    pub author: String,
    /// Homepage URL.
    pub homepage: String,
    /// Admin-editable settings fields.
    pub settings: Vec<PluginSettingField>,
    /// Per-capability configs.
    pub capability_configs: Vec<PluginCapabilityConfig>,
    /// Scheduler config, required when `scheduler` is declared.
    pub schedule: Option<PluginScheduleConfig>,
    /// Declared `/ext/` routes.
    pub routes: Vec<PluginRouteSpec>,
    /// Panel bundle path inside the plugin dir, when the plugin ships UI.
    pub ui_entry: String,
    /// Panel page ids (at most one in v1).
    pub ui_pages: Vec<String>,
    /// External panel URL, exclusive with entry/pages.
    pub ui_external_url: String,
}

impl PluginManifest {
    /// Keys of the secret-flagged settings fields.
    pub fn secret_keys(&self) -> std::collections::HashSet<String> {
        self.settings
            .iter()
            .filter(|field| field.secret)
            .map(|field| field.key.clone())
            .collect()
    }
}

/// A manifest that is missing, unparsable, or declares an invalid contract.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError(pub String);

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ManifestError {}

// ---------------------------------------------------------------------------
// Strict TOML subset reader
// ---------------------------------------------------------------------------

/// One parsed value. Manifests only need these four shapes.
#[derive(Debug, Clone, PartialEq, Eq)]
enum RawValue {
    /// Double-quoted string.
    Str(String),
    /// Plain integer.
    Int(i64),
    /// `true` or `false`.
    Bool(bool),
    /// Single-line array of strings.
    StrList(Vec<String>),
}

/// One table body. A vec (not a map) so duplicate keys fail instead of
/// silently winning.
#[derive(Debug, Clone, Default)]
struct RawTable {
    values: Vec<(String, RawValue)>,
}

impl RawTable {
    fn get(&self, key: &str) -> Option<&RawValue> {
        self.values
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    fn insert(&mut self, line_no: usize, key: String, value: RawValue) -> Result<(), String> {
        if self.values.iter().any(|(name, _)| name == &key) {
            return Err(format!("line {line_no}: duplicate key '{key}'"));
        }
        self.values.push((key, value));
        Ok(())
    }
}

/// A parsed manifest file: bare top-level keys, `[tables]`, and
/// `[[arrays]]` of tables.
#[derive(Debug, Clone, Default)]
struct RawDoc {
    top: RawTable,
    tables: HashMap<String, RawTable>,
    arrays: HashMap<String, Vec<RawTable>>,
}

/// Cut a `#` comment, ignoring hashes inside double-quoted strings.
fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        if *byte == b'"' {
            in_string = true;
        } else if *byte == b'#' {
            return &line[..index];
        }
    }
    line
}

/// Split a `key = value` line on the first `=` outside a string.
fn split_assignment(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    let mut in_string = false;
    let mut escaped = false;
    for (index, byte) in bytes.iter().enumerate() {
        if in_string {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        if *byte == b'"' {
            in_string = true;
        } else if *byte == b'=' {
            return Some((line[..index].trim(), line[index + 1..].trim()));
        }
    }
    None
}

fn valid_key_chars(text: &str) -> bool {
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// Parse one double-quoted string starting at the first byte of `text`.
/// Returns the value plus the unconsumed remainder.
fn parse_quoted(text: &str, line_no: usize) -> Result<(String, &str), String> {
    let bytes = text.as_bytes();
    if bytes.first() != Some(&b'"') {
        return Err(format!("line {line_no}: expected a quoted string"));
    }
    let mut out: Vec<u8> = Vec::new();
    let mut index = 1;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if escaped {
            match byte {
                b'"' => out.push(b'"'),
                b'\\' => out.push(b'\\'),
                b'n' => out.push(b'\n'),
                b't' => out.push(b'\t'),
                b'r' => out.push(b'\r'),
                _ => {
                    return Err(format!("line {line_no}: unsupported escape in string"));
                }
            }
            escaped = false;
            index += 1;
            continue;
        }
        if byte == b'\\' {
            escaped = true;
            index += 1;
            continue;
        }
        if byte == b'"' {
            return match String::from_utf8(out) {
                Ok(value) => Ok((value, &text[index + 1..])),
                Err(_) => Err(format!("line {line_no}: string is not valid UTF-8")),
            };
        }
        if byte < 0x20 {
            return Err(format!("line {line_no}: unterminated string"));
        }
        out.push(byte);
        index += 1;
    }
    Err(format!("line {line_no}: unterminated string"))
}

/// Parse one value: string, integer, boolean, or string array.
fn parse_value(text: &str, line_no: usize) -> Result<RawValue, String> {
    if text.starts_with('"') {
        let (value, rest) = parse_quoted(text, line_no)?;
        if !rest.trim().is_empty() {
            return Err(format!("line {line_no}: unexpected text after value"));
        }
        return Ok(RawValue::Str(value));
    }
    if text == "true" {
        return Ok(RawValue::Bool(true));
    }
    if text == "false" {
        return Ok(RawValue::Bool(false));
    }
    if text.starts_with('[') {
        if !text.ends_with(']') {
            return Err(format!(
                "line {line_no}: multi-line arrays are not supported"
            ));
        }
        let inner = text[1..text.len() - 1].trim();
        if inner.is_empty() {
            return Ok(RawValue::StrList(Vec::new()));
        }
        let mut items = Vec::new();
        let mut rest = inner;
        loop {
            rest = rest.trim_start();
            if rest.is_empty() {
                break;
            }
            if !rest.starts_with('"') {
                return Err(format!("line {line_no}: arrays hold strings only"));
            }
            let (value, after) = parse_quoted(rest, line_no)?;
            items.push(value);
            let after = after.trim_start();
            if after.is_empty() {
                break;
            }
            if !after.starts_with(',') {
                return Err(format!("line {line_no}: expected a comma in array"));
            }
            rest = &after[1..];
        }
        return Ok(RawValue::StrList(items));
    }
    let digits = text.strip_prefix('-').unwrap_or(text);
    if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
        return match text.parse::<i64>() {
            Ok(number) => Ok(RawValue::Int(number)),
            Err(_) => Err(format!("line {line_no}: integer out of range")),
        };
    }
    Err(format!("line {line_no}: unsupported value"))
}

/// Parse a manifest file into tables. Anything outside the supported subset
/// is an error naming its line.
fn parse_manifest_toml(text: &str) -> Result<RawDoc, String> {
    let mut doc = RawDoc::default();
    // Currently open section: table name, or array name plus element index.
    let mut current_table: Option<String> = None;
    let mut current_array: Option<(String, usize)> = None;
    for (index, raw_line) in text.lines().enumerate() {
        let line_no = index + 1;
        let line = strip_comment(raw_line).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            current_table = None;
            current_array = None;
            if line.starts_with("[[") {
                if !line.ends_with("]]") || line.len() <= 4 {
                    return Err(format!("line {line_no}: bad array header"));
                }
                let name = line[2..line.len() - 2].trim().to_owned();
                if !valid_key_chars(&name) {
                    return Err(format!("line {line_no}: bad array header"));
                }
                let elements = doc.arrays.entry(name.clone()).or_default();
                elements.push(RawTable::default());
                current_array = Some((name, elements.len() - 1));
                continue;
            }
            if !line.ends_with(']') || line.len() <= 2 {
                return Err(format!("line {line_no}: bad table header"));
            }
            let name = line[1..line.len() - 1].trim().to_owned();
            if !valid_key_chars(&name) {
                return Err(format!("line {line_no}: bad table header"));
            }
            if doc.tables.contains_key(&name) {
                return Err(format!("line {line_no}: duplicate table '{name}'"));
            }
            doc.tables.insert(name.clone(), RawTable::default());
            current_table = Some(name);
            continue;
        }
        let Some((key, raw_value)) = split_assignment(line) else {
            return Err(format!("line {line_no}: expected key = value"));
        };
        if !valid_key_chars(key) {
            return Err(format!("line {line_no}: bad key '{key}'"));
        }
        let value = parse_value(raw_value, line_no)?;
        if let Some((name, element)) = current_array.clone() {
            let elements = doc.arrays.get_mut(&name);
            match elements.and_then(|items| items.get_mut(element)) {
                Some(table) => table.insert(line_no, key.to_owned(), value)?,
                None => return Err(format!("line {line_no}: keys must live under a table")),
            }
        } else if let Some(name) = current_table.clone() {
            match doc.tables.get_mut(&name) {
                Some(table) => table.insert(line_no, key.to_owned(), value)?,
                None => return Err(format!("line {line_no}: keys must live under a table")),
            }
        } else {
            doc.top.insert(line_no, key.to_owned(), value)?;
        }
    }
    Ok(doc)
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Read one string field, defaulting to empty when absent. Non-string values
/// read as empty the way v2's `str(value or "")` does for optional fields.
fn raw_str(table: &RawTable, key: &str) -> String {
    match table.get(key) {
        Some(RawValue::Str(value)) => value.clone(),
        Some(RawValue::Int(number)) => number.to_string(),
        Some(RawValue::Bool(true)) => "true".to_owned(),
        Some(RawValue::Bool(false)) => "false".to_owned(),
        _ => String::new(),
    }
}

fn raw_bool(table: &RawTable, key: &str) -> bool {
    matches!(table.get(key), Some(RawValue::Bool(true)))
}

/// Format a string list the way Python renders `['a', 'b']` in messages.
fn py_list(items: &[String]) -> String {
    let mut out = String::from('[');
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            out.push_str(", ");
        }
        out.push('\'');
        out.push_str(item);
        out.push('\'');
    }
    out.push(']');
    out
}

/// Reproduce v2's localhost check for `http://` panel URLs exactly: split
/// off the path, strip userinfo, take the pre-colon host, strip brackets.
fn http_loopback_host(url: &str) -> String {
    let after_scheme = url
        .strip_prefix("http://")
        .unwrap_or("")
        .split('/')
        .next()
        .unwrap_or("");
    let no_userinfo = after_scheme.rsplit('@').next().unwrap_or("");
    let host = no_userinfo.split(':').next().unwrap_or("");
    host.trim_matches(['[', ']']).to_owned()
}

/// Load and validate the manifest in one plugin directory.
pub fn load_manifest(plugin_dir: &Path) -> Result<PluginManifest, ManifestError> {
    let dir_name = plugin_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("plugin")
        .to_owned();
    let path = plugin_dir.join("plugin.toml");
    let text = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(_) => return Err(ManifestError(format!("{dir_name}: no plugin.toml"))),
    };
    let text = match String::from_utf8(text) {
        Ok(text) => text,
        Err(error) => {
            return Err(ManifestError(format!(
                "{dir_name}: unreadable plugin.toml ({error})"
            )));
        }
    };
    let doc = match parse_manifest_toml(&text) {
        Ok(doc) => doc,
        Err(reason) => {
            return Err(ManifestError(format!(
                "{dir_name}: unreadable plugin.toml ({reason})"
            )));
        }
    };
    validate_manifest(&dir_name, &doc)
}

fn validate_manifest(dir_name: &str, doc: &RawDoc) -> Result<PluginManifest, ManifestError> {
    let fail = |message: String| ManifestError(message);
    let Some(plugin) = doc.tables.get("plugin") else {
        return Err(fail(format!("{dir_name}: missing [plugin] table")));
    };

    let raw_name = raw_str(plugin, "name").trim().to_owned();
    // v2 coerces with `int(...)`, which accepts bools (True == 1); the
    // strict integer check below applies to every other numeric field.
    let api_version = match plugin.get("api_version") {
        Some(RawValue::Int(number)) => *number,
        Some(RawValue::Bool(true)) => 1,
        Some(RawValue::Bool(false)) => 0,
        _ => {
            return Err(fail(format!(
                "{}: api_version must be an integer",
                display_name(&raw_name, dir_name)
            )));
        }
    };
    if !SUPPORTED_API_VERSIONS.contains(&api_version) {
        return Err(fail(format!(
            "{}: api_version {api_version} unsupported (host speaks (0, 1))",
            display_name(&raw_name, dir_name)
        )));
    }
    let legacy = api_version == PLUGIN_API_VERSION_LEGACY;
    let name_ok = if legacy {
        valid_legacy_name(&raw_name)
    } else {
        valid_v1_name(&raw_name)
    };
    if !name_ok {
        return Err(fail(format!(
            "{dir_name}: invalid plugin name '{raw_name}'"
        )));
    }
    let name = raw_name;

    for key in doc
        .top
        .values
        .iter()
        .map(|(name, _)| name)
        .chain(doc.tables.keys())
        .chain(doc.arrays.keys())
    {
        if !matches!(
            key.as_str(),
            "plugin" | "settings" | "capability" | "schedule" | "route" | "plugin_ui"
        ) {
            return Err(fail(format!("{name}: unknown key '{key}'")));
        }
    }
    for (key, _) in &plugin.values {
        if !matches!(
            key.as_str(),
            "name"
                | "version"
                | "api_version"
                | "entrypoint"
                | "capabilities"
                | "display_name"
                | "description"
                | "author"
                | "homepage"
        ) {
            return Err(fail(format!("{name}: unknown [plugin] key '{key}'")));
        }
    }

    let entrypoint = raw_str(plugin, "entrypoint").trim().to_owned();
    if !entrypoint.contains(':') {
        return Err(fail(format!(
            "{name}: entrypoint must be '<module>:<ClassName>'"
        )));
    }

    let capabilities = match plugin.get("capabilities") {
        Some(RawValue::StrList(items)) if !items.is_empty() => items.clone(),
        _ => {
            return Err(fail(format!("{name}: at least one capability is required")));
        }
    };
    let known = known_capabilities(api_version);
    let unknown: Vec<String> = capabilities
        .iter()
        .filter(|item| !known.contains(&item.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(fail(format!(
            "{name}: unknown capabilities {}",
            py_list(&unknown)
        )));
    }

    let settings = validate_settings(&name, doc)?;
    let capability_configs = validate_capability_configs(&name, doc, &capabilities)?;
    let schedule = validate_schedule(&name, doc, legacy, &capabilities)?;
    let routes = validate_routes(&name, doc, legacy, &capabilities)?;
    let (ui_entry, ui_pages, ui_external_url) = validate_ui(&name, doc, legacy)?;

    Ok(PluginManifest {
        display_name: {
            let display = raw_str(plugin, "display_name");
            if display.is_empty() {
                name.clone()
            } else {
                display
            }
        },
        version: {
            let version = raw_str(plugin, "version");
            if version.is_empty() {
                "0.0.0".to_owned()
            } else {
                version
            }
        },
        name,
        api_version,
        entrypoint,
        capabilities,
        description: raw_str(plugin, "description"),
        author: raw_str(plugin, "author"),
        homepage: raw_str(plugin, "homepage"),
        settings,
        capability_configs,
        schedule,
        routes,
        ui_entry,
        ui_pages,
        ui_external_url,
    })
}

fn display_name<'a>(raw_name: &'a str, dir_name: &'a str) -> &'a str {
    if raw_name.is_empty() {
        dir_name
    } else {
        raw_name
    }
}

fn validate_settings(name: &str, doc: &RawDoc) -> Result<Vec<PluginSettingField>, ManifestError> {
    if doc.tables.contains_key("settings") {
        return Err(ManifestError(format!(
            "{name}: [[settings]] must be a list"
        )));
    }
    let mut fields = Vec::new();
    for entry in doc.arrays.get("settings").map(Vec::as_slice).unwrap_or(&[]) {
        let key = raw_str(entry, "key");
        if key.is_empty() {
            return Err(ManifestError(format!(
                "{name}: each [[settings]] entry needs a key"
            )));
        }
        for (entry_key, _) in &entry.values {
            if !matches!(entry_key.as_str(), "key" | "label" | "help" | "secret") {
                return Err(ManifestError(format!(
                    "{name}: unknown [[settings]] key '{entry_key}'"
                )));
            }
        }
        let label = raw_str(entry, "label");
        fields.push(PluginSettingField {
            key: key.clone(),
            label: if label.is_empty() { key } else { label },
            help: raw_str(entry, "help"),
            secret: raw_bool(entry, "secret"),
        });
    }
    Ok(fields)
}

fn validate_capability_configs(
    name: &str,
    doc: &RawDoc,
    declared: &[String],
) -> Result<Vec<PluginCapabilityConfig>, ManifestError> {
    if doc.tables.contains_key("capability") {
        return Err(ManifestError(format!(
            "{name}: [[capability]] must be a list"
        )));
    }
    let mut configs = Vec::new();
    for entry in doc
        .arrays
        .get("capability")
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let id = raw_str(entry, "id");
        if id.is_empty() {
            return Err(ManifestError(format!(
                "{name}: each [[capability]] entry needs an id"
            )));
        }
        for (entry_key, _) in &entry.values {
            if !matches!(
                entry_key.as_str(),
                "id" | "source" | "target_source" | "display_name"
            ) {
                return Err(ManifestError(format!(
                    "{name}: unknown [[capability]] key '{entry_key}'"
                )));
            }
        }
        let source = raw_str(entry, "source");
        if !source.is_empty() && !valid_v1_name(&source) {
            return Err(ManifestError(format!(
                "{name}: [[capability]] source '{source}' must match ^[a-z0-9][a-z0-9-]{{0,31}}$"
            )));
        }
        let target_source = raw_str(entry, "target_source");
        if !target_source.is_empty() && !valid_plugin_target(&target_source) {
            return Err(ManifestError(format!(
                "{name}: [[capability]] target_source '{target_source}' must be 'usenet' or a plugin source key"
            )));
        }
        configs.push(PluginCapabilityConfig {
            id,
            source,
            target_source,
            display_name: raw_str(entry, "display_name"),
        });
    }
    let outside: Vec<String> = configs
        .iter()
        .filter(|config| !declared.contains(&config.id))
        .map(|config| config.id.clone())
        .collect();
    if !outside.is_empty() {
        return Err(ManifestError(format!(
            "{name}: [[capability]] ids {} must also appear in capabilities",
            py_list(&outside)
        )));
    }
    if declared.contains(&"indexer".to_owned()) && !declared.contains(&"download_client".to_owned())
    {
        let targeted = configs
            .iter()
            .any(|config| config.id == "indexer" && !config.target_source.is_empty());
        if !targeted {
            return Err(ManifestError(format!(
                "{name}: indexer capability requires target_source ('usenet' or a plugin source key)"
            )));
        }
    }
    Ok(configs)
}

fn validate_schedule(
    name: &str,
    doc: &RawDoc,
    legacy: bool,
    declared: &[String],
) -> Result<Option<PluginScheduleConfig>, ManifestError> {
    if doc.arrays.contains_key("schedule") {
        return Err(ManifestError(format!("{name}: [schedule] must be a table")));
    }
    let Some(table) = doc.tables.get("schedule") else {
        if declared.contains(&"scheduler".to_owned()) {
            return Err(ManifestError(format!(
                "{name}: scheduler capability requires a [schedule] table with interval_minutes in [5, 1440]"
            )));
        }
        return Ok(None);
    };
    if legacy {
        return Err(ManifestError(format!(
            "{name}: [schedule] requires api_version 1"
        )));
    }
    for (key, _) in &table.values {
        if !matches!(key.as_str(), "interval_minutes" | "run_on_load") {
            return Err(ManifestError(format!(
                "{name}: unknown [schedule] key '{key}'"
            )));
        }
    }
    let interval = match table.get("interval_minutes") {
        Some(RawValue::Int(number)) => *number,
        _ => {
            return Err(ManifestError(format!(
                "{name}: [schedule] interval_minutes must be an integer in [5, 1440]"
            )));
        }
    };
    if !(5..=1440).contains(&interval) {
        return Err(ManifestError(format!(
            "{name}: [schedule] interval_minutes {interval} must be in [5, 1440]"
        )));
    }
    match table.get("run_on_load") {
        None => {}
        Some(RawValue::Bool(_)) => {}
        Some(_) => {
            return Err(ManifestError(format!(
                "{name}: [schedule] run_on_load must be a boolean"
            )));
        }
    }
    Ok(Some(PluginScheduleConfig {
        interval_minutes: interval,
        run_on_load: raw_bool(table, "run_on_load"),
    }))
}

fn validate_routes(
    name: &str,
    doc: &RawDoc,
    legacy: bool,
    declared: &[String],
) -> Result<Vec<PluginRouteSpec>, ManifestError> {
    if doc.tables.contains_key("route") {
        return Err(ManifestError(format!("{name}: [[route]] must be a list")));
    }
    let entries = doc.arrays.get("route").map(Vec::as_slice).unwrap_or(&[]);
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    if legacy {
        return Err(ManifestError(format!(
            "{name}: [[route]] requires api_version 1"
        )));
    }
    if !declared.contains(&"publisher".to_owned()) {
        return Err(ManifestError(format!(
            "{name}: [[route]] requires the publisher capability"
        )));
    }
    let mut routes = Vec::new();
    for entry in entries {
        for (key, _) in &entry.values {
            if !matches!(
                key.as_str(),
                "path" | "method" | "auth" | "rate_limit_per_minute"
            ) {
                return Err(ManifestError(format!(
                    "{name}: unknown [[route]] key '{key}'"
                )));
            }
        }
        let path = raw_str(entry, "path");
        if !valid_route_path(&path) {
            return Err(ManifestError(format!(
                "{name}: [[route]] path '{path}' must match ^[a-z0-9][a-z0-9/_-]{{0,63}}$"
            )));
        }
        let method = {
            let method = raw_str(entry, "method");
            if method.is_empty() {
                "GET".to_owned()
            } else {
                method
            }
        };
        if !matches!(method.as_str(), "GET" | "POST" | "DELETE") {
            return Err(ManifestError(format!(
                "{name}: [[route]] method '{method}' must be one of GET, POST, DELETE"
            )));
        }
        let auth = {
            let auth = raw_str(entry, "auth");
            if auth.is_empty() {
                "admin".to_owned()
            } else {
                auth
            }
        };
        if !matches!(auth.as_str(), "admin" | "user") {
            return Err(ManifestError(format!(
                "{name}: [[route]] auth '{auth}' must be admin or user"
            )));
        }
        let rate = match entry.get("rate_limit_per_minute") {
            None => 60,
            Some(RawValue::Int(number)) => *number,
            Some(_) => {
                return Err(ManifestError(format!(
                    "{name}: [[route]] rate_limit_per_minute must be an integer"
                )));
            }
        };
        if !(1..=600).contains(&rate) {
            return Err(ManifestError(format!(
                "{name}: [[route]] rate_limit_per_minute {rate} must be in [1, 600]"
            )));
        }
        routes.push(PluginRouteSpec {
            path,
            method,
            auth,
            rate_limit_per_minute: rate,
        });
    }
    Ok(routes)
}

fn validate_ui(
    name: &str,
    doc: &RawDoc,
    legacy: bool,
) -> Result<(String, Vec<String>, String), ManifestError> {
    if doc.arrays.contains_key("plugin_ui") {
        return Err(ManifestError(format!(
            "{name}: [plugin_ui] must be a table"
        )));
    }
    let Some(table) = doc.tables.get("plugin_ui") else {
        return Ok((String::new(), Vec::new(), String::new()));
    };
    if legacy {
        return Err(ManifestError(format!(
            "{name}: [plugin_ui] requires api_version 1"
        )));
    }
    for (key, _) in &table.values {
        if !matches!(key.as_str(), "entry" | "pages" | "external_url") {
            return Err(ManifestError(format!(
                "{name}: unknown [plugin_ui] key '{key}'"
            )));
        }
    }
    let entry = raw_str(table, "entry");
    if !entry.is_empty() {
        let normalized = entry.replace('\\', "/");
        if normalized.starts_with('/')
            || normalized.starts_with("http://")
            || normalized.starts_with("https://")
            || normalized.split('/').any(|part| part == "..")
        {
            return Err(ManifestError(format!(
                "{name}: [plugin_ui] entry must stay inside the plugin dir"
            )));
        }
    }
    let pages = match table.get("pages") {
        None => Vec::new(),
        Some(RawValue::StrList(items)) => {
            if items.iter().any(|item| item.is_empty()) {
                return Err(ManifestError(format!(
                    "{name}: [plugin_ui] pages must be a list of page ids"
                )));
            }
            items.clone()
        }
        Some(_) => {
            return Err(ManifestError(format!(
                "{name}: [plugin_ui] pages must be a list of page ids"
            )));
        }
    };
    if pages.len() > 1 {
        return Err(ManifestError(format!(
            "{name}: [plugin_ui] pages allows a single page id in v1"
        )));
    }
    let external_url = raw_str(table, "external_url");
    if !external_url.is_empty() {
        if external_url.starts_with("https://") {
            // Allowed anywhere.
        } else if let Some(_rest) = external_url.strip_prefix("http://") {
            let host = http_loopback_host(&external_url);
            if host != "localhost" && !host.starts_with("127.") && host != "::1" {
                return Err(ManifestError(format!(
                    "{name}: [plugin_ui] external_url http is only for localhost/loopback"
                )));
            }
        } else {
            return Err(ManifestError(format!(
                "{name}: [plugin_ui] external_url must be https:// (http only for localhost/loopback)"
            )));
        }
    }
    if !external_url.is_empty() && (!entry.is_empty() || !pages.is_empty()) {
        return Err(ManifestError(format!(
            "{name}: [plugin_ui] entry/pages and external_url are mutually exclusive"
        )));
    }
    Ok((entry, pages, external_url))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Result<PluginManifest, ManifestError> {
        let doc = parse_manifest_toml(text).map_err(ManifestError)?;
        validate_manifest("toy", &doc)
    }

    #[test]
    fn minimal_subscriber_manifest_validates() {
        let manifest = parse(
            "[plugin]\n\
             name = \"toy\"\n\
             version = \"1.0.0\"\n\
             api_version = 1\n\
             entrypoint = \"plugin:Toy\"\n\
             capabilities = [\"subscriber\"]\n",
        )
        .unwrap();
        assert_eq!(manifest.name, "toy");
        assert_eq!(manifest.display_name, "toy");
        assert!(manifest.schedule.is_none());
    }

    #[test]
    fn unknown_capability_lists_the_offenders() {
        let error = parse(
            "[plugin]\n\
             name = \"toy\"\n\
             api_version = 1\n\
             entrypoint = \"plugin:Toy\"\n\
             capabilities = [\"subscriber\", \"mind-reading\"]\n",
        )
        .unwrap_err();
        assert_eq!(error.0, "toy: unknown capabilities ['mind-reading']");
    }

    #[test]
    fn scheduler_without_schedule_is_rejected() {
        let error = parse(
            "[plugin]\n\
             name = \"toy\"\n\
             api_version = 1\n\
             entrypoint = \"plugin:Toy\"\n\
             capabilities = [\"scheduler\"]\n",
        )
        .unwrap_err();
        assert!(error.0.contains("requires a [schedule] table"));
    }

    #[test]
    fn legacy_manifest_keeps_its_narrow_name_rule() {
        assert!(
            parse(
                "[plugin]\n\
             name = \"Old_Name-1\"\n\
             api_version = 0\n\
             entrypoint = \"plugin:Toy\"\n\
             capabilities = [\"scrobbler\"]\n",
            )
            .is_ok()
        );
        let error = parse(
            "[plugin]\n\
             name = \"Old_Name-1\"\n\
             api_version = 1\n\
             entrypoint = \"plugin:Toy\"\n\
             capabilities = [\"scrobbler\"]\n",
        )
        .unwrap_err();
        assert!(error.0.contains("invalid plugin name"));
    }

    #[test]
    fn routes_need_the_publisher_capability() {
        let error = parse(
            "[plugin]\n\
             name = \"toy\"\n\
             api_version = 1\n\
             entrypoint = \"plugin:Toy\"\n\
             capabilities = [\"subscriber\"]\n\
             [[route]]\n\
             path = \"status\"\n",
        )
        .unwrap_err();
        assert!(error.0.contains("requires the publisher capability"));
    }

    #[test]
    fn subset_parser_rejects_multi_line_arrays() {
        let error = parse_manifest_toml("[plugin]\ncapabilities = [\n\"a\",\n]\n").unwrap_err();
        assert!(error.contains("multi-line arrays"));
    }

    #[test]
    fn subset_parser_keeps_hashes_inside_strings() {
        let doc = parse_manifest_toml("[plugin]\ndescription = \"a # b\"\n").unwrap();
        assert_eq!(
            doc.tables["plugin"].get("description"),
            Some(&RawValue::Str("a # b".to_owned()))
        );
    }

    #[test]
    fn indexer_without_a_target_is_rejected() {
        let error = parse(
            "[plugin]\n\
             name = \"toy\"\n\
             api_version = 1\n\
             entrypoint = \"plugin:Toy\"\n\
             capabilities = [\"indexer\"]\n\
             [[capability]]\n\
             id = \"indexer\"\n",
        )
        .unwrap_err();
        assert!(error.0.contains("requires target_source"));
    }
}
