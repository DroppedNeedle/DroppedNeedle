//! The discover stand-in for when YouTube cannot start without its quota
//! file. It invents no rows: it answers empty or "not configured".

use std::collections::HashMap;

use crate::reads::discover::{
    models::YouTubeQuotaResponse,
    ports::{BoxFuture, ProviderFailure, YouTubeSource},
};

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

    fn is_cached<'a>(
        &'a self,
        _artist: &'a str,
        _name: &'a str,
        _track: bool,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    fn cached_tracks<'a>(
        &'a self,
        _pairs: &'a [(String, String)],
    ) -> BoxFuture<'a, HashMap<String, bool>> {
        Box::pin(async { HashMap::new() })
    }

    fn quota(&self) -> BoxFuture<'_, Option<YouTubeQuotaResponse>> {
        Box::pin(async { None })
    }
}
