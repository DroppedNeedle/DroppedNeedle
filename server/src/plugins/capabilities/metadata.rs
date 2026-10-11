//! `metadata_provider`: plugins fill gaps in artist and album pages.
//!
//! Plugin data sits below every first-party source. A field counts as a
//! gap when it is missing or empty, and only gaps are filled:
//!
//! | Field       | First-party present | Gap                                  |
//! |-------------|---------------------|--------------------------------------|
//! | `biography` | kept                | first non-empty plugin biography     |
//! | `links`     | kept, then extended | first-party order, then plugin-only  |
//! | `tags`      | kept, then extended | same rule as links                   |
//! | images      | kept                | the first plugin image URL           |
//!
//! Plugins are asked in name order under a 15 second budget each; one
//! that fails or answers `null` ("not mine") adds nothing.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::host::PluginHost;
use super::super::protocol::methods;
use super::{call, decode};
use crate::reads::catalog::models::PluginDetails;
use crate::reads::catalog::ports::{BoxFuture, DetailsSource};

/// Budget per plugin.
const METADATA_TIMEOUT: Duration = Duration::from_secs(15);

/// What a plugin may add to an artist or album. Every field is optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginEnrichment {
    /// Biography or album notes.
    pub biography: Option<String>,
    /// External links.
    pub links: Vec<String>,
    /// Tags or genres.
    pub tags: Vec<String>,
    /// Image URLs, best first.
    pub image_urls: Vec<String>,
}

/// The first-party fields a page already has, filled in place.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnrichmentFields {
    /// Biography from first-party sources.
    pub biography: Option<String>,
    /// Links from first-party sources.
    pub links: Vec<String>,
    /// Tags from first-party sources.
    pub tags: Vec<String>,
    /// Image from first-party sources.
    pub image_url: Option<String>,
}

impl EnrichmentFields {
    /// Fill gaps from one plugin answer, never overwriting first-party data.
    pub fn fill_gaps(&mut self, plugin: &PluginEnrichment) {
        let empty =
            |text: &Option<String>| text.as_deref().is_none_or(|text| text.trim().is_empty());
        if empty(&self.biography)
            && let Some(biography) = plugin
                .biography
                .as_ref()
                .filter(|text| !text.trim().is_empty())
        {
            self.biography = Some(biography.clone());
        }
        for link in &plugin.links {
            if !link.trim().is_empty() && !self.links.contains(link) {
                self.links.push(link.clone());
            }
        }
        for tag in &plugin.tags {
            if !tag.trim().is_empty() && !self.tags.contains(tag) {
                self.tags.push(tag.clone());
            }
        }
        if empty(&self.image_url)
            && let Some(image) = plugin
                .image_urls
                .iter()
                .find(|url| url.starts_with("https://") || url.starts_with("http://"))
        {
            self.image_url = Some(image.clone());
        }
    }
}

impl PluginHost {
    /// Artist enrichment from every `metadata_provider` plugin, in name
    /// order. Empty when no plugin claims the artist.
    pub async fn enrich_artist(
        &self,
        artist_name: &str,
        mbid: Option<&str>,
    ) -> Vec<PluginEnrichment> {
        let params = json!({ "artist_name": artist_name, "mbid": mbid });
        self.enrich(methods::ENRICH_ARTIST, params).await
    }

    /// Album enrichment from every `metadata_provider` plugin, in name order.
    pub async fn enrich_album(
        &self,
        artist_name: &str,
        album_title: &str,
        mbid: Option<&str>,
    ) -> Vec<PluginEnrichment> {
        let params = json!({
            "artist_name": artist_name,
            "album_title": album_title,
            "mbid": mbid,
        });
        self.enrich(methods::ENRICH_ALBUM, params).await
    }

    async fn enrich(&self, method: &str, params: serde_json::Value) -> Vec<PluginEnrichment> {
        let plugins = self.serving("metadata_provider");
        let answers = futures_util::future::join_all(plugins.iter().map(|plugin| {
            let params = params.clone();
            async move {
                let value = call(plugin, method, params, METADATA_TIMEOUT).await.ok()?;
                decode::<Option<PluginEnrichment>>(plugin, method, value)
                    .ok()
                    .flatten()
            }
        }))
        .await;
        answers.into_iter().flatten().collect()
    }
}

/// Plain text from plugin text: tags dropped (`<br>` and paragraph ends
/// become line breaks), common entities decoded, and any angle bracket
/// left over removed, so the text is safe wherever a page puts it.
pub fn plain_text(text: &str) -> String {
    let mut plain = String::with_capacity(text.len());
    let mut tag = String::new();
    let mut in_tag = false;
    for character in text.chars() {
        match character {
            '<' => {
                in_tag = true;
                tag.clear();
            }
            '>' if in_tag => {
                in_tag = false;
                let name = tag
                    .trim_start_matches('/')
                    .split(|c: char| c.is_whitespace() || c == '/')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                if matches!(name.as_str(), "br" | "p" | "div" | "li") {
                    plain.push('\n');
                }
            }
            _ if in_tag => tag.push(character),
            _ => plain.push(character),
        }
    }
    let decoded = plain
        .replace("&nbsp;", " ")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&lt;", "")
        .replace("&gt;", "")
        .replace("&amp;", "&");
    let cleaned: String = decoded
        .chars()
        .filter(|c| *c != '<' && *c != '>')
        .filter(|c| *c == '\n' || !c.is_control())
        .collect();
    let lines: Vec<&str> = cleaned.lines().map(str::trim).collect();
    lines.join("\n").trim().to_owned()
}

/// Every plugin's answer merged by the gap rule, as the catalog shows it.
/// Text is reduced to plain text, links to `http(s)` and the image to
/// `https` only.
fn merged(answers: &[PluginEnrichment]) -> PluginDetails {
    let mut fields = EnrichmentFields::default();
    for answer in answers {
        let mut clean = answer.clone();
        clean.biography = clean
            .biography
            .as_deref()
            .map(plain_text)
            .filter(|text| !text.is_empty());
        clean.tags = clean
            .tags
            .iter()
            .map(String::as_str)
            .map(plain_text)
            .filter(|tag| !tag.is_empty() && !tag.contains('\n'))
            .collect();
        clean.links.retain(|link| {
            (link.starts_with("https://") || link.starts_with("http://"))
                && !link
                    .chars()
                    .any(|c| c.is_whitespace() || c == '"' || c == '<')
        });
        clean.image_urls.retain(|url| {
            url.starts_with("https://") && !url.chars().any(|c| c.is_whitespace() || c == '"')
        });
        fields.fill_gaps(&clean);
    }
    PluginDetails {
        biography: fields.biography,
        links: fields.links,
        tags: fields.tags,
        image_url: fields.image_url,
    }
}

/// The artist and album pages' plugin details.
pub struct PluginDetailsSource(pub Arc<PluginHost>);

impl DetailsSource for PluginDetailsSource {
    fn token(&self) -> String {
        self.0
            .serving("metadata_provider")
            .iter()
            .map(|plugin| plugin.manifest.name.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    fn artist<'a>(&'a self, name: &'a str, mbid: &'a str) -> BoxFuture<'a, PluginDetails> {
        Box::pin(async move { merged(&self.0.enrich_artist(name, Some(mbid)).await) })
    }

    fn album<'a>(
        &'a self,
        artist: &'a str,
        title: &'a str,
        mbid: &'a str,
    ) -> BoxFuture<'a, PluginDetails> {
        Box::pin(async move { merged(&self.0.enrich_album(artist, title, Some(mbid)).await) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_text_is_plain() {
        assert_eq!(
            plain_text("<img src=x onerror=alert(1)>Hi<br>there &lt;script&gt; &amp; <b>you</b>"),
            "Hi\nthere script & you"
        );
    }

    #[test]
    fn plugins_only_fill_gaps() {
        let mut page = EnrichmentFields {
            biography: Some("First-party bio".to_owned()),
            links: vec!["https://a.test".to_owned()],
            tags: Vec::new(),
            image_url: None,
        };
        page.fill_gaps(&PluginEnrichment {
            biography: Some("Plugin bio".to_owned()),
            links: vec!["https://b.test".to_owned(), "https://a.test".to_owned()],
            tags: vec!["rock".to_owned()],
            image_urls: vec!["https://img.test/1.jpg".to_owned()],
        });
        assert_eq!(page.biography.as_deref(), Some("First-party bio"));
        assert_eq!(page.links, ["https://a.test", "https://b.test"]);
        assert_eq!(page.tags, ["rock"]);
        assert_eq!(page.image_url.as_deref(), Some("https://img.test/1.jpg"));
    }
}
