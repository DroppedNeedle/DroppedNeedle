//! `purchase_links`: plugins add links to an album's "Where to buy" list.
//!
//! Every provider is asked at once under a 10 second budget; a plugin that
//! fails or runs late adds nothing. Links must be `http(s)`, are
//! deduplicated by URL, and keep the providers' name order. The album page
//! orders them by its own store rules afterwards: a plugin cannot push its
//! links to the top.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::super::host::PluginHost;
use super::super::protocol::methods;
use super::super::runtime::PluginPurchaseLink;
use super::{call, decode};
use crate::reads::catalog::models::PurchaseKind;
use crate::reads::catalog::ports::{
    BoxFuture, ExtraPurchaseLink, PurchaseLinkSource, PurchaseQuery,
};

/// Budget per plugin.
const PURCHASE_TIMEOUT: Duration = Duration::from_secs(10);
/// Links kept per plugin.
const LINKS_PER_PLUGIN: usize = 20;

impl PluginHost {
    /// Links from every `purchase_links` plugin for one album.
    pub async fn gather_purchase_links(
        &self,
        artist: &str,
        album: &str,
        release_group_mbid: &str,
    ) -> Vec<PluginPurchaseLink> {
        let plugins = self.serving("purchase_links");
        let params = json!({
            "artist": artist,
            "album": album,
            "release_group_mbid": release_group_mbid,
        });
        let answers = futures_util::future::join_all(plugins.iter().map(|plugin| {
            let params = params.clone();
            async move {
                let value = call(plugin, methods::PURCHASE_LINKS, params, PURCHASE_TIMEOUT)
                    .await
                    .ok()?;
                decode::<Vec<PluginPurchaseLink>>(plugin, methods::PURCHASE_LINKS, value).ok()
            }
        }))
        .await;
        let mut seen = HashSet::new();
        let mut links = Vec::new();
        for found in answers.into_iter().flatten() {
            for mut link in found.into_iter().take(LINKS_PER_PLUGIN) {
                let url = link.url.trim().to_owned();
                let label = link.label.trim().to_owned();
                if label.is_empty()
                    || !(url.starts_with("https://") || url.starts_with("http://"))
                    || !seen.insert(url.clone())
                {
                    continue;
                }
                if !matches!(link.kind.as_str(), "digital" | "physical" | "free") {
                    link.kind = "digital".to_owned();
                }
                link.url = url;
                link.label = label;
                links.push(link);
            }
        }
        links
    }
}

/// The album page's extra purchase links, answered by the plugins.
pub struct PluginPurchaseLinks(pub Arc<PluginHost>);

impl PurchaseLinkSource for PluginPurchaseLinks {
    fn token(&self) -> String {
        self.0
            .serving("purchase_links")
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
