//! Discover stand-ins for when a live part cannot start: YouTube without
//! its quota file, and the presence feed on this router (live presence is
//! served by playback).
//!
//! None of them invent rows: they answer empty or "not available".

use std::collections::HashMap;

use crate::reads::discover::{
    models::{NowPlayingEntry, YouTubeQuotaResponse},
    ports::{BoxFuture, NowPlayingStore, ProviderFailure, YouTubeSource},
};

/// Presence lives in playback; this router's feed is empty.
pub struct NoPresence;

impl NowPlayingStore for NoPresence {
    fn snapshot(&self) -> Vec<NowPlayingEntry> {
        Vec::new()
    }
}

/// YouTube when its quota file cannot be read: searches are off.
pub struct UnavailableYouTube;

impl YouTubeSource for UnavailableYouTube {
    fn is_configured(&self) -> bool {
        false
    }

    fn search_video<'a>(
        &'a self,
        _artist: &'a str,
        _album: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>> {
        Box::pin(async {
            Err(ProviderFailure::not_configured(
                "YouTube search is off: its quota file could not be read.",
            ))
        })
    }

    fn search_track<'a>(
        &'a self,
        _artist: &'a str,
        _track: &'a str,
    ) -> BoxFuture<'a, Result<Option<String>, ProviderFailure>> {
        Box::pin(async {
            Err(ProviderFailure::not_configured(
                "YouTube search is off: its quota file could not be read.",
            ))
        })
    }

    fn is_cached(&self, _artist: &str, _name: &str, _track: bool) -> bool {
        false
    }

    fn cached_tracks(&self, _pairs: &[(String, String)]) -> HashMap<String, bool> {
        HashMap::new()
    }

    fn quota(&self) -> BoxFuture<'_, Option<YouTubeQuotaResponse>> {
        Box::pin(async { None })
    }
}
