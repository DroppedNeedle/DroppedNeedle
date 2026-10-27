//! The plugin sides of the catalog's details and purchase-link ports:
//! artist and album details and extra "Where to buy" links, answered by the
//! enabled plugins.

use std::sync::Arc;

use super::models::{PluginDetails, PurchaseKind};
use super::ports::{
    BoxFuture, DetailsSource, ExtraPurchaseLink, PurchaseLinkSource, PurchaseQuery,
};
use droppedneedle_plugins::capabilities::metadata::{
    EnrichmentFields, PluginEnrichment, plain_text,
};
use droppedneedle_plugins::host::PluginHost;
use droppedneedle_plugins::manifest::Capability;

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
            .serving(Capability::MetadataProvider)
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

/// The album page's extra purchase links, answered by the plugins.
pub struct PluginPurchaseLinks(pub Arc<PluginHost>);

impl PurchaseLinkSource for PluginPurchaseLinks {
    fn token(&self) -> String {
        self.0
            .serving(Capability::PurchaseLinks)
            .iter()
            .map(|plugin| plugin.manifest.name.as_str())
            .collect::<Vec<_>>()
            .join(",")
    }

    fn links<'a>(&'a self, query: &'a PurchaseQuery) -> BoxFuture<'a, Vec<ExtraPurchaseLink>> {
        Box::pin(async move {
            self.0
                .gather_purchase_links(&query.artist, &query.title, &query.release_group_mbid)
                .await
                .into_iter()
                .map(|link| ExtraPurchaseLink {
                    kind: Some(match link.kind.as_str() {
                        "physical" => PurchaseKind::Physical,
                        "free" => PurchaseKind::Free,
                        _ => PurchaseKind::Digital,
                    }),
                    url: link.url,
                    label: Some(link.label),
                })
                .collect()
        })
    }
}
