//! `purchase_links`: plugins add links to an album's "Where to buy" list.
//!
//! Every provider is asked at once under a 10 second budget; a plugin that
//! fails or runs late adds nothing and marks the answer incomplete. Links must be `http(s)`, are
//! deduplicated by URL, and keep the providers' name order. The album page
//! orders them by its own store rules afterwards: a plugin cannot push its
//! links to the top.

use std::collections::HashSet;
use std::time::Duration;

use droppedneedle_providers::WebAddress;
use serde_json::json;

use super::super::host::PluginHost;
use super::super::manifest::Capability;
use super::super::protocol::methods;
use super::super::runtime::PluginPurchaseLink;
use super::{Gathered, answer};

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
    ) -> Gathered<Vec<PluginPurchaseLink>> {
        let plugins = self.serving(Capability::PurchaseLinks);
        let params = json!({
            "artist": artist,
            "album": album,
            "release_group_mbid": release_group_mbid,
        });
        let answers = futures_util::future::join_all(plugins.iter().map(|plugin| {
            answer::<Vec<PluginPurchaseLink>>(
                plugin,
                methods::PURCHASE_LINKS,
                params.clone(),
                PURCHASE_TIMEOUT,
            )
        }))
        .await;
        let complete = answers.iter().all(Result::is_ok);
        let mut seen = HashSet::new();
        let mut links = Vec::new();
        for found in answers.into_iter().flatten().flatten() {
            for mut link in found.into_iter().take(LINKS_PER_PLUGIN) {
                let Some(url) = WebAddress::parse(&link.url).map(String::from) else {
                    continue;
                };
                let label = link.label.trim().to_owned();
                if label.is_empty() || !seen.insert(url.clone()) {
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
        Gathered {
            value: links,
            complete,
        }
    }
}
