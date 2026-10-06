//! Seams the catalog reads through without owning the data behind them.

use std::future::Future;
use std::pin::Pin;

/// Boxed future for dyn-compatible port methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One user's follow state for one artist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowState {
    /// Whether the user follows the artist.
    pub followed: bool,
    /// Whether the user wants new releases downloaded on their own.
    pub auto_download: bool,
    /// Approval state of that ask: none, pending, approved, rejected or
    /// revoked.
    pub auto_download_state: String,
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

/// Where edition pins are written. Pins belong to one library copy of an
/// album; the collections pin store owns them.
pub trait EditionPins: Send + Sync {
    /// Pin `release_mbid` for one library album.
    fn set<'a>(
        &'a self,
        album_id: &'a str,
        release_group_mbid: &'a str,
        release_mbid: &'a str,
        user_id: &'a str,
    ) -> BoxFuture<'a, Result<(), String>>;

    /// Clear one library album's pin.
    fn clear<'a>(&'a self, album_id: &'a str) -> BoxFuture<'a, Result<(), String>>;
}

/// No pin store wired: every write fails.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEditionPins;

impl EditionPins for NoEditionPins {
    fn set<'a>(
        &'a self,
        _album_id: &'a str,
        _release_group_mbid: &'a str,
        _release_mbid: &'a str,
        _user_id: &'a str,
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("edition pins are not wired".to_owned()) })
    }

    fn clear<'a>(&'a self, _album_id: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async { Err("edition pins are not wired".to_owned()) })
    }
}
