//! Seams the catalog reads through without owning the data behind them.

use std::future::Future;
use std::pin::Pin;

use crate::reads::collections::state::AutoDownloadState;

/// Boxed future for dyn-compatible port methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One user's follow state for one artist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowState {
    /// Whether the user follows the artist.
    pub followed: bool,
    /// Whether the user wants new releases downloaded on their own.
    pub auto_download: bool,
    /// Approval state of that ask.
    pub auto_download_state: AutoDownloadState,
}

/// Where the artist header reads the caller's follow state. The follow
/// store lives with the collections; the header only reads it.
pub trait FollowLookup: Send + Sync {
    /// The user's follow state for one artist, or `None` when it cannot be
    /// read (the header then shows "not followed").
    fn status<'a>(
        &'a self,
        user_id: &'a str,
        artist_mbid: &'a str,
    ) -> BoxFuture<'a, Option<FollowState>>;
}

/// No follow store wired: every artist reads as not followed.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoFollows;

impl FollowLookup for NoFollows {
    fn status<'a>(
        &'a self,
        _user_id: &'a str,
        _artist_mbid: &'a str,
    ) -> BoxFuture<'a, Option<FollowState>> {
        Box::pin(async { None })
    }
}

/// What a purchase-link provider is asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurchaseQuery {
    /// The album's artist credit as printed.
    pub artist: String,
    /// The album title.
    pub title: String,
    /// The release-group MBID.
    pub release_group_mbid: String,
}

/// One extra "where to buy" link from a provider outside MusicBrainz.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtraPurchaseLink {
    /// Store page; only http(s) URLs are shown.
    pub url: String,
    /// Display name, when the provider gave one.
    pub label: Option<String>,
    /// Digital, physical or free; digital when the provider did not say.
    pub kind: Option<super::models::PurchaseKind>,
}

/// Extra purchase links, the plugins' `purchase_links` capability in v2.
/// The plugin host plugs in here; until then the source is empty.
pub trait PurchaseLinkSource: Send + Sync {
    /// Identifies the providers answering right now (their names, sorted).
    /// It keys the purchase cache, so enabling or disabling a provider
    /// misses to a fresh answer instead of serving a week-old one.
    fn token(&self) -> String;

    /// Links for one album. Failures are the provider's to log; they read
    /// as no links.
    fn links<'a>(&'a self, query: &'a PurchaseQuery) -> BoxFuture<'a, Vec<ExtraPurchaseLink>>;
}

/// No purchase-link providers.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoPurchaseLinks;

impl PurchaseLinkSource for NoPurchaseLinks {
    fn token(&self) -> String {
        String::new()
    }

    fn links<'a>(&'a self, _query: &'a PurchaseQuery) -> BoxFuture<'a, Vec<ExtraPurchaseLink>> {
        Box::pin(async { Vec::new() })
    }
}

/// Artist and album details from providers below the first-party sources
/// (the plugins' `metadata_provider` capability). Failures are the
/// provider's to log; they read as empty details.
pub trait DetailsSource: Send + Sync {
    /// Identifies the providers answering right now (their names); it
    /// keys the details cache, so enabling or disabling one is seen at
    /// once. Empty means none, and nothing is asked.
    fn token(&self) -> String;

    /// Details about one artist.
    fn artist<'a>(
        &'a self,
        name: &'a str,
        mbid: &'a str,
    ) -> BoxFuture<'a, super::models::PluginDetails>;

    /// Details about one album.
    fn album<'a>(
        &'a self,
        artist: &'a str,
        title: &'a str,
        mbid: &'a str,
    ) -> BoxFuture<'a, super::models::PluginDetails>;
}

/// No details providers.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoDetails;

impl DetailsSource for NoDetails {
    fn token(&self) -> String {
        String::new()
    }

    fn artist<'a>(
        &'a self,
        _name: &'a str,
        _mbid: &'a str,
    ) -> BoxFuture<'a, super::models::PluginDetails> {
        Box::pin(async { super::models::PluginDetails::default() })
    }

    fn album<'a>(
        &'a self,
        _artist: &'a str,
        _title: &'a str,
        _mbid: &'a str,
    ) -> BoxFuture<'a, super::models::PluginDetails> {
        Box::pin(async { super::models::PluginDetails::default() })
    }
}
