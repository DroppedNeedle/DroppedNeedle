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

use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::super::host::PluginHost;
use super::super::protocol::methods;
use super::{call, decode};

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

#[cfg(test)]
mod tests {
    use super::*;

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
