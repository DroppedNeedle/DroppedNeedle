//! The one seam every Usenet search goes through.
//!
//! [`Indexer`] is implemented by the native Newznab list, by Prowlarr and
//! by plugin indexers that feed Usenet. The Usenet source holds the
//! indexers the settings choose (the native list or Prowlarr, then any
//! plugin indexers) and searches them all the same way; only the wiring
//! reads the backend setting.

use std::sync::Arc;

use futures_util::future::BoxFuture;

use super::newznab::{IndexerResult, UsenetRelease};
use crate::downloads::source_tag::SourceTag;
use crate::target::SearchTarget;
use droppedneedle_plugins::host::PluginHost;

/// What one Usenet search asks for.
pub enum IndexerQuery<'a> {
    /// The target's album.
    Album(&'a SearchTarget),
    /// A single track on its own (the last resort).
    Track {
        /// Credited artist.
        artist: &'a str,
        /// Track title.
        title: &'a str,
    },
}

/// One place Usenet releases are found. A search never fails: an indexer
/// that refuses, limits or answers garbage contributes no releases, so
/// one bad indexer never sinks the others.
pub trait Indexer: Send + Sync {
    /// Releases for `query`, best first.
    fn search<'a>(&'a self, query: &'a IndexerQuery<'a>) -> BoxFuture<'a, Vec<IndexerResult>>;
}

/// Every enabled plugin indexer whose target is Usenet.
pub struct PluginIndexers {
    host: Arc<PluginHost>,
}

impl PluginIndexers {
    /// The Usenet-feeding indexers among `host`'s plugins.
    pub fn new(host: Arc<PluginHost>) -> Self {
        Self { host }
    }
}

impl Indexer for PluginIndexers {
    /// Results without an http(s) NZB URL cannot go to SABnzbd and are
    /// skipped.
    fn search<'a>(&'a self, query: &'a IndexerQuery<'a>) -> BoxFuture<'a, Vec<IndexerResult>> {
        Box::pin(async move {
            // Plugin indexers name the source they feed by its stored text.
            let usenet = SourceTag::Usenet.to_string();
            let found = match query {
                IndexerQuery::Track { artist, title } => {
                    self.host.search_track(&usenet, artist, title, None).await
                }
                IndexerQuery::Album(target) => {
                    self.host
                        .search_album(
                            &usenet,
                            &target.artist,
                            &target.album_title,
                            target.year.map(i64::from),
                            i64::try_from(target.tracklist.len())
                                .ok()
                                .filter(|count| *count > 0),
                        )
                        .await
                }
            };
            found
                .into_iter()
                .filter(|hit| {
                    hit.nzb_url.starts_with("https://") || hit.nzb_url.starts_with("http://")
                })
                .map(|hit| IndexerResult {
                    source: SourceTag::Usenet,
                    usenet: UsenetRelease {
                        indexer_id: "plugin".to_owned(),
                        indexer_name: "plugin".to_owned(),
                        guid: hit.payload,
                        title: hit.title,
                        nzb_url: hit.nzb_url,
                        size_bytes: hit.size_bytes.max(0) as u64,
                        category_ids: Vec::new(),
                        grabs: None,
                        files: None,
                        usenet_date: hit.usenet_date,
                        password: 0,
                    },
                })
                .collect()
        })
    }
}
