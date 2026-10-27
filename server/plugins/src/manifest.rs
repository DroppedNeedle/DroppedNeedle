//! Plugin manifest: `plugin.toml` at a plugin package's root.
//!
//! The manifest is the contract's front door. The host refuses to load any
//! code before the manifest parses, declares the current `api_version`, and
//! names only known capabilities. The file is read with the `toml` crate
//! into raw structs that refuse unknown keys, then checked into typed
//! values.

use std::path::Path;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The plugin API version this host reads. Any other version is refused.
pub const PLUGIN_API_VERSION: i64 = 2;

/// What a plugin may declare it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = PluginCapability)]
pub enum Capability {
    /// Receives accepted plays.
    Scrobbler,
    /// Adds buy links to album pages.
    PurchaseLinks,
    /// Downloads releases for requests.
    DownloadClient,
    /// Searches for releases.
    Indexer,
    /// Receives server events.
    Subscriber,
    /// Publishes notes and hints back, and serves `/ext/` routes.
    Publisher,
    /// Adds biographies, tags, links and images.
    MetadataProvider,
    /// Runs on a timer.
    Scheduler,
    /// Serves audio for tracks missing from the library.
    StreamingSource,
    /// Adds new files to the library.
    LibraryWriter,
}

impl Capability {
    /// Every capability, in the order PLUGINS.md lists them.
    pub const ALL: [Capability; 10] = [
        Capability::Scrobbler,
        Capability::PurchaseLinks,
        Capability::DownloadClient,
        Capability::Indexer,
        Capability::Subscriber,
        Capability::Publisher,
        Capability::MetadataProvider,
        Capability::Scheduler,
        Capability::StreamingSource,
        Capability::LibraryWriter,
    ];

    /// The id a manifest and the protocol use.
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Scrobbler => "scrobbler",
            Capability::PurchaseLinks => "purchase_links",
            Capability::DownloadClient => "download_client",
            Capability::Indexer => "indexer",
            Capability::Subscriber => "subscriber",
            Capability::Publisher => "publisher",
            Capability::MetadataProvider => "metadata_provider",
            Capability::Scheduler => "scheduler",
            Capability::StreamingSource => "streaming_source",
            Capability::LibraryWriter => "library_writer",
        }
    }

    /// The capability with this id, if there is one.
    pub fn from_id(id: &str) -> Option<Capability> {
        Capability::ALL
            .into_iter()
            .find(|capability| capability.as_str() == id)
    }
}

impl std::fmt::Display for Capability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// HTTP method of a plugin route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteMethod {
    /// GET.
    Get,
    /// POST.
    Post,
    /// DELETE.
    Delete,
}

impl RouteMethod {
    /// The method as HTTP writes it.
    pub fn as_str(self) -> &'static str {
        match self {
            RouteMethod::Get => "GET",
            RouteMethod::Post => "POST",
            RouteMethod::Delete => "DELETE",
        }
    }
}

/// Who may call a plugin route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteAuth {
    /// Admins only.
    Admin,
    /// Any signed-in user.
    User,
}

/// Plugin and source name rule: `^[a-z0-9][a-z0-9-]{0,31}$`.
pub fn valid_plugin_name(name: &str) -> bool {
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

/// Indexer target rule: `usenet` or `plugin:<name>`.
pub fn valid_plugin_target(target: &str) -> bool {
    target == "usenet" || valid_plugin_key(target)
}

/// Plugin source key rule: `plugin:<name>`.
pub fn valid_plugin_key(key: &str) -> bool {
    match key.strip_prefix("plugin:") {
        Some(name) => valid_plugin_name(name),
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
    /// The capability it configures.
    pub id: Capability,
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
    /// The method it answers.
    pub method: RouteMethod,
    /// Who may call it.
    pub auth: RouteAuth,
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
    /// Contract version, always [`PLUGIN_API_VERSION`] once loaded.
    pub api_version: i64,
    /// `<module>:<ClassName>` inside the plugin package, run under the
    /// Python helper. Empty when `command` is set.
    pub entrypoint: String,
    /// Program plus arguments that start the plugin, for plugins not
    /// written in Python. A leading `./` points inside the plugin folder.
    pub command: Vec<String>,
    /// Declared capabilities.
    pub capabilities: Vec<Capability>,
    /// Display name (defaults to the name).
    pub display_name: String,
    /// Short description.
    pub description: String,
    /// Author string.
    pub author: String,
    /// Homepage, an http or https address, or empty.
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
    /// Panel page ids (at most one).
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

    /// Whether the manifest declares `capability`.
    pub fn declares(&self, capability: Capability) -> bool {
        self.capabilities.contains(&capability)
    }

    /// What enabling this plugin lets it do, in plain words, for the
    /// install preview. Derived from the manifest alone: it describes what
    /// the plugin asks for, not what its code will actually do.
    pub fn permissions(&self) -> Vec<String> {
        let mut lines = vec![
            "Runs as its own program on your server, with the same file and network access as the server's user account".to_owned(),
        ];
        if self.command.is_empty() {
            lines.push(format!("Starts with Python: {}", self.entrypoint));
        } else {
            lines.push(format!("Starts the program: {}", self.command.join(" ")));
        }
        for capability in &self.capabilities {
            let line = match capability {
                Capability::Scrobbler => "Sees every play that gets scrobbled: artist, track, album and time".to_owned(),
                Capability::PurchaseLinks => "Adds buy links to album pages".to_owned(),
                Capability::DownloadClient => "Downloads files for requests and hands them to the importer".to_owned(),
                Capability::Indexer => "Searches for releases when music is requested".to_owned(),
                Capability::Subscriber => "Sees download, request, import and playback events, including user ids".to_owned(),
                Capability::Publisher => "Can post notes and search hints back to the server".to_owned(),
                Capability::MetadataProvider => "Adds biographies, tags, links and images to artist and album pages".to_owned(),
                Capability::Scheduler => {
                    let minutes = self
                        .schedule
                        .as_ref()
                        .map(|schedule| schedule.interval_minutes)
                        .unwrap_or(60);
                    format!("Runs on its own every {minutes} minutes")
                }
                Capability::LibraryWriter => "Can add new files to your music library (never replaces one), up to 100 MiB a day".to_owned(),
                Capability::StreamingSource => "Can serve audio for tracks missing from your library, from a file or a web address".to_owned(),
            };
            lines.push(line);
        }
        for route in &self.routes {
            let who = match route.auth {
                RouteAuth::Admin => "admins",
                RouteAuth::User => "any signed-in user",
            };
            lines.push(format!(
                "Answers {} /api/v3/plugins/ext/{}/{} for {who}",
                route.method.as_str(),
                self.name,
                route.path
            ));
        }
        if !self.ui_entry.is_empty() {
            lines.push("Shows its own panel in Settings (in a locked-down frame)".to_owned());
        }
        if !self.ui_external_url.is_empty() {
            lines.push(format!(
                "Links to an outside page: {}",
                self.ui_external_url
            ));
        }
        let secrets = self.settings.iter().filter(|field| field.secret).count();
        if secrets > 0 {
            lines.push(format!(
                "Receives {secrets} secret setting(s) you enter, such as API keys"
            ));
        }
        lines
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
// The file as written
// ---------------------------------------------------------------------------

/// The `[plugin]` name and version, read leniently before anything else so
/// a manifest for another API version is refused with the porting pointer
/// rather than with whatever key its version spelled differently.
#[derive(Deserialize)]
struct VersionProbe {
    plugin: Option<ProbePlugin>,
}

#[derive(Deserialize)]
struct ProbePlugin {
    name: Option<toml::Value>,
    api_version: Option<toml::Value>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawManifest {
    plugin: Option<RawPlugin>,
    #[serde(default)]
    settings: Vec<RawSetting>,
    #[serde(default)]
    capability: Vec<RawCapability>,
    schedule: Option<RawSchedule>,
    #[serde(default)]
    route: Vec<RawRoute>,
    plugin_ui: Option<RawUi>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPlugin {
    #[serde(default)]
    name: String,
    version: Option<String>,
    api_version: i64,
    entrypoint: Option<String>,
    command: Option<Vec<String>>,
    #[serde(default)]
    capabilities: Vec<String>,
    display_name: Option<String>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    author: String,
    #[serde(default)]
    homepage: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSetting {
    #[serde(default)]
    key: String,
    #[serde(default)]
    label: String,
    #[serde(default)]
    help: String,
    #[serde(default)]
    secret: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCapability {
    #[serde(default)]
    id: String,
    #[serde(default)]
    source: String,
    #[serde(default)]
    target_source: String,
    #[serde(default)]
    display_name: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSchedule {
    interval_minutes: Option<i64>,
    #[serde(default)]
    run_on_load: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRoute {
    #[serde(default)]
    path: String,
    method: Option<String>,
    auth: Option<String>,
    rate_limit_per_minute: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawUi {
    #[serde(default)]
    entry: String,
    #[serde(default)]
    pages: Vec<String>,
    #[serde(default)]
    external_url: String,
}

/// Describe a TOML or shape error by its line and message.
fn describe(text: &str, error: &toml::de::Error) -> String {
    let message = error.message().trim_end();
    match error.span() {
        Some(span) => {
            let line = text[..span.start.min(text.len())].matches('\n').count() + 1;
            format!("line {line}: {message}")
        }
        None => message.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

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
    parse_manifest(&dir_name, &text)
}

/// Validate manifest bytes. `dir_name` names the plugin in errors when the
/// manifest has no usable name of its own.
pub fn parse_manifest(dir_name: &str, text: &[u8]) -> Result<PluginManifest, ManifestError> {
    let unreadable =
        |reason: String| ManifestError(format!("{dir_name}: unreadable plugin.toml ({reason})"));
    let text = std::str::from_utf8(text).map_err(|error| unreadable(error.to_string()))?;
    let probe: VersionProbe =
        toml::from_str(text).map_err(|error| unreadable(describe(text, &error)))?;
    let Some(plugin) = probe.plugin else {
        return Err(ManifestError(format!("{dir_name}: missing [plugin] table")));
    };
    let who = match &plugin.name {
        Some(toml::Value::String(name)) if !name.trim().is_empty() => name.trim().to_owned(),
        _ => dir_name.to_owned(),
    };
    let api_version = match plugin.api_version {
        Some(toml::Value::Integer(number)) => number,
        _ => {
            return Err(ManifestError(format!(
                "{who}: api_version must be a whole number; this DroppedNeedle reads \
                 api_version {PLUGIN_API_VERSION}; see Porting a v2 plugin in PLUGINS.md"
            )));
        }
    };
    if api_version != PLUGIN_API_VERSION {
        return Err(ManifestError(format!(
            "{who}: written for api_version {api_version}; this DroppedNeedle reads \
             api_version {PLUGIN_API_VERSION}; see Porting a v2 plugin in PLUGINS.md"
        )));
    }
    let raw: RawManifest = toml::from_str(text)
        .map_err(|error| ManifestError(format!("{who}: {}", describe(text, &error))))?;
    validate_manifest(dir_name, raw)
}

fn validate_manifest(dir_name: &str, raw: RawManifest) -> Result<PluginManifest, ManifestError> {
    let fail = |message: String| ManifestError(message);
    let Some(plugin) = raw.plugin else {
        return Err(fail(format!("{dir_name}: missing [plugin] table")));
    };
    let name = plugin.name.trim().to_owned();
    if !valid_plugin_name(&name) {
        return Err(fail(format!("{dir_name}: invalid plugin name '{name}'")));
    }

    let command = match plugin.command {
        None => Vec::new(),
        Some(items) if items.first().is_some_and(|first| !first.trim().is_empty()) => items,
        Some(_) => {
            return Err(fail(format!(
                "{name}: command must be a non-empty list of strings"
            )));
        }
    };
    let entrypoint = plugin.entrypoint.unwrap_or_default().trim().to_owned();
    if command.is_empty() && !entrypoint.contains(':') {
        return Err(fail(format!(
            "{name}: entrypoint must be '<module>:<ClassName>'"
        )));
    }

    if plugin.capabilities.is_empty() {
        return Err(fail(format!("{name}: at least one capability is required")));
    }
    let mut capabilities = Vec::new();
    for id in &plugin.capabilities {
        let Some(capability) = Capability::from_id(id) else {
            return Err(fail(format!("{name}: unknown capability '{id}'")));
        };
        if !capabilities.contains(&capability) {
            capabilities.push(capability);
        }
    }

    let homepage = plugin.homepage.trim().to_owned();
    if !homepage.is_empty() && droppedneedle_auth::federated::web_address(&homepage).is_none() {
        return Err(fail(format!(
            "{name}: homepage must be an http or https address"
        )));
    }

    let settings = validate_settings(&name, raw.settings)?;
    let capability_configs = validate_capability_configs(&name, raw.capability, &capabilities)?;
    let schedule = validate_schedule(&name, raw.schedule, &capabilities)?;
    let routes = validate_routes(&name, raw.route, &capabilities)?;
    let (ui_entry, ui_pages, ui_external_url) = match raw.plugin_ui {
        Some(ui) => validate_ui(&name, ui)?,
        None => (String::new(), Vec::new(), String::new()),
    };

    Ok(PluginManifest {
        display_name: plugin
            .display_name
            .filter(|display| !display.is_empty())
            .unwrap_or_else(|| name.clone()),
        version: plugin
            .version
            .filter(|version| !version.is_empty())
            .unwrap_or_else(|| "0.0.0".to_owned()),
        name,
        api_version: plugin.api_version,
        entrypoint,
        command,
        capabilities,
        description: plugin.description,
        author: plugin.author,
        homepage,
        settings,
        capability_configs,
        schedule,
        routes,
        ui_entry,
        ui_pages,
        ui_external_url,
    })
}

fn validate_settings(
    name: &str,
    entries: Vec<RawSetting>,
) -> Result<Vec<PluginSettingField>, ManifestError> {
    let mut fields = Vec::new();
    for entry in entries {
        if entry.key.is_empty() {
            return Err(ManifestError(format!(
                "{name}: each [[settings]] entry needs a key"
            )));
        }
        fields.push(PluginSettingField {
            label: if entry.label.is_empty() {
                entry.key.clone()
            } else {
                entry.label
            },
            key: entry.key,
            help: entry.help,
            secret: entry.secret,
        });
    }
    Ok(fields)
}

fn validate_capability_configs(
    name: &str,
    entries: Vec<RawCapability>,
    declared: &[Capability],
) -> Result<Vec<PluginCapabilityConfig>, ManifestError> {
    let mut configs = Vec::new();
    for entry in entries {
        if entry.id.is_empty() {
            return Err(ManifestError(format!(
                "{name}: each [[capability]] entry needs an id"
            )));
        }
        let Some(id) = Capability::from_id(&entry.id) else {
            return Err(ManifestError(format!(
                "{name}: unknown capability '{}' in [[capability]]",
                entry.id
            )));
        };
        if !declared.contains(&id) {
            return Err(ManifestError(format!(
                "{name}: [[capability]] id '{id}' must also appear in capabilities"
            )));
        }
        if !entry.source.is_empty() && !valid_plugin_name(&entry.source) {
            return Err(ManifestError(format!(
                "{name}: [[capability]] source '{}' must match ^[a-z0-9][a-z0-9-]{{0,31}}$",
                entry.source
            )));
        }
        if !entry.target_source.is_empty() && !valid_plugin_target(&entry.target_source) {
            return Err(ManifestError(format!(
                "{name}: [[capability]] target_source '{}' must be 'usenet' or a plugin source key",
                entry.target_source
            )));
        }
        configs.push(PluginCapabilityConfig {
            id,
            source: entry.source,
            target_source: entry.target_source,
            display_name: entry.display_name,
        });
    }
    if declared.contains(&Capability::Indexer) && !declared.contains(&Capability::DownloadClient) {
        let targeted = configs
            .iter()
            .any(|config| config.id == Capability::Indexer && !config.target_source.is_empty());
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
    table: Option<RawSchedule>,
    declared: &[Capability],
) -> Result<Option<PluginScheduleConfig>, ManifestError> {
    let Some(table) = table else {
        if declared.contains(&Capability::Scheduler) {
            return Err(ManifestError(format!(
                "{name}: scheduler capability requires a [schedule] table with interval_minutes in [5, 1440]"
            )));
        }
        return Ok(None);
    };
    let Some(interval) = table.interval_minutes else {
        return Err(ManifestError(format!(
            "{name}: [schedule] interval_minutes must be an integer in [5, 1440]"
        )));
    };
    if !(5..=1440).contains(&interval) {
        return Err(ManifestError(format!(
            "{name}: [schedule] interval_minutes {interval} must be in [5, 1440]"
        )));
    }
    Ok(Some(PluginScheduleConfig {
        interval_minutes: interval,
        run_on_load: table.run_on_load,
    }))
}

fn validate_routes(
    name: &str,
    entries: Vec<RawRoute>,
    declared: &[Capability],
) -> Result<Vec<PluginRouteSpec>, ManifestError> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    if !declared.contains(&Capability::Publisher) {
        return Err(ManifestError(format!(
            "{name}: [[route]] requires the publisher capability"
        )));
    }
    let mut routes = Vec::new();
    for entry in entries {
        if !valid_route_path(&entry.path) {
            return Err(ManifestError(format!(
                "{name}: [[route]] path '{}' must match ^[a-z0-9][a-z0-9/_-]{{0,63}}$",
                entry.path
            )));
        }
        let method = match entry.method.as_deref().unwrap_or("GET") {
            "GET" => RouteMethod::Get,
            "POST" => RouteMethod::Post,
            "DELETE" => RouteMethod::Delete,
            other => {
                return Err(ManifestError(format!(
                    "{name}: [[route]] method '{other}' must be one of GET, POST, DELETE"
                )));
            }
        };
        let auth = match entry.auth.as_deref().unwrap_or("admin") {
            "admin" => RouteAuth::Admin,
            "user" => RouteAuth::User,
            other => {
                return Err(ManifestError(format!(
                    "{name}: [[route]] auth '{other}' must be admin or user"
                )));
            }
        };
        let rate = entry.rate_limit_per_minute.unwrap_or(60);
        if !(1..=600).contains(&rate) {
            return Err(ManifestError(format!(
                "{name}: [[route]] rate_limit_per_minute {rate} must be in [1, 600]"
            )));
        }
        routes.push(PluginRouteSpec {
            path: entry.path,
            method,
            auth,
            rate_limit_per_minute: rate,
        });
    }
    Ok(routes)
}

fn validate_ui(name: &str, table: RawUi) -> Result<(String, Vec<String>, String), ManifestError> {
    let entry = table.entry;
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
    let pages = table.pages;
    if pages.iter().any(String::is_empty) {
        return Err(ManifestError(format!(
            "{name}: [plugin_ui] pages must be a list of page ids"
        )));
    }
    if pages.len() > 1 {
        return Err(ManifestError(format!(
            "{name}: [plugin_ui] pages allows a single page id"
        )));
    }
    let external_url = table.external_url;
    if !external_url.is_empty() {
        if external_url.starts_with("https://") {
            // Allowed anywhere.
        } else if external_url.starts_with("http://") {
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
        parse_manifest("toy", text.as_bytes())
    }

    #[test]
    fn minimal_subscriber_manifest_validates() {
        let manifest = parse(
            "[plugin]\n\
             name = \"toy\"\n\
             version = \"1.0.0\"\n\
             api_version = 2\n\
             entrypoint = \"plugin:Toy\"\n\
             capabilities = [\"subscriber\"]\n",
        )
        .unwrap();
        assert_eq!(manifest.name, "toy");
        assert_eq!(manifest.display_name, "toy");
        assert_eq!(manifest.capabilities, [Capability::Subscriber]);
        assert!(manifest.schedule.is_none());
    }

    /// The protocol and the API send a capability as its manifest id.
    #[test]
    fn capabilities_serialize_as_their_manifest_ids() {
        for capability in Capability::ALL {
            assert_eq!(
                serde_json::to_value(capability).unwrap(),
                serde_json::Value::String(capability.as_str().to_owned())
            );
        }
    }

    #[test]
    fn unknown_capability_is_named() {
        let error = parse(
            "[plugin]\n\
             name = \"toy\"\n\
             api_version = 2\n\
             entrypoint = \"plugin:Toy\"\n\
             capabilities = [\"subscriber\", \"mind-reading\"]\n",
        )
        .unwrap_err();
        assert_eq!(error.0, "toy: unknown capability 'mind-reading'");
    }

    #[test]
    fn a_misspelt_key_is_named() {
        let error = parse(
            "[plugin]\n\
             name = \"toy\"\n\
             api_version = 2\n\
             entrypoint = \"plugin:Toy\"\n\
             capabilites = [\"subscriber\"]\n",
        )
        .unwrap_err();
        assert!(error.0.contains("line 5"), "{error}");
        assert!(error.0.contains("capabilites"), "{error}");
    }

    #[test]
    fn scheduler_without_schedule_is_rejected() {
        let error = parse(
            "[plugin]\n\
             name = \"toy\"\n\
             api_version = 2\n\
             entrypoint = \"plugin:Toy\"\n\
             capabilities = [\"scheduler\"]\n",
        )
        .unwrap_err();
        assert!(error.0.contains("requires a [schedule] table"));
    }

    #[test]
    fn routes_need_the_publisher_capability() {
        let error = parse(
            "[plugin]\n\
             name = \"toy\"\n\
             api_version = 2\n\
             entrypoint = \"plugin:Toy\"\n\
             capabilities = [\"subscriber\"]\n\
             [[route]]\n\
             path = \"status\"\n",
        )
        .unwrap_err();
        assert!(error.0.contains("requires the publisher capability"));
    }

    #[test]
    fn homepage_must_be_a_web_address() {
        let manifest = |homepage: &str| {
            parse(&format!(
                "[plugin]\nname = \"toy\"\napi_version = 2\nentrypoint = \"plugin:Toy\"\n\
                 capabilities = [\"subscriber\"]\nhomepage = \"{homepage}\"\n"
            ))
        };
        assert!(manifest("https://toy.test/about").is_ok());
        for bad in [
            "javascript:alert(1)",
            "ftp://toy.test",
            "https://user:pw@toy.test",
        ] {
            let error = manifest(bad).unwrap_err();
            assert!(error.0.contains("homepage"), "{bad}: {error}");
        }
    }

    #[test]
    fn indexer_without_a_target_is_rejected() {
        let error = parse(
            "[plugin]\n\
             name = \"toy\"\n\
             api_version = 2\n\
             entrypoint = \"plugin:Toy\"\n\
             capabilities = [\"indexer\"]\n\
             [[capability]]\n\
             id = \"indexer\"\n",
        )
        .unwrap_err();
        assert!(error.0.contains("requires target_source"));
    }
}
