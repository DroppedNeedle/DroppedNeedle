//! 30-second track and album previews via Deezer (primary) and iTunes (fallback),
//! Deezer artist pictures by Deezer artist id, and Deezer album ids by barcode.
//!
//! A Rust port of v2's preview repository. Both APIs are keyless. Deezer
//! matches better (field-scoped query syntax, ordered album track lists);
//! iTunes results are verified against the requested artist because its top
//! hit can be a cover. The Deezer wire shapes, and the dates they were
//! verified on, are in `preview/models.rs`; the iTunes ones are in
//! `itunes/models.rs`.
//!
//! Expiry warning, carried over from the v2 module docs: Deezer preview URLs
//! carry an `hdnea` expiry token. Callers must resolve them just-in-time and
//! never cache the URL long-term; only the enrichment layer decides
//! what, if anything, is cached, and it must treat these URLs as short-lived.
//!
//! Deezer requests go through the executor at the `deezer` row, iTunes
//! requests through the [`ITunesClient`] at the `itunes` row. A 404 on
//! either leg is absence. The composed lookups below are infallible
//! exactly like v2 (a dead preview source is absence, not failure); the
//! per-leg functions stay public and return typed failures.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::RequestPriority;
use crate::client::{decode, parse_base, unusable_base};
use crate::error::{MAX_RETRY_AFTER, ProviderError};
use crate::executor::{
    Answer, Backoff, Executor, Redirects, Reply, Request, Spec, Url, standard_classify,
};
use crate::itunes::ITunesClient;
use crate::itunes::models::SongSearch;
use crate::web_address::{self, WebAddress};

mod models;

pub use self::models::SongLookup;
use self::models::{
    AlbumByUpc, AlbumSearch, AlbumTracks, DeezerArtist, DeezerFault, Track, TrackSearch,
};

/// Live Deezer API origin.
pub const DEEZER_API_BASE: &str = "https://api.deezer.com";
/// Live Deezer short-link origin.
pub const DEEZER_LINKS_BASE: &str = "https://link.deezer.com";
/// The provider name Deezer errors carry, and its rate row.
pub const DEEZER: &str = "deezer";
/// Preview length v2 stamps on every result.
pub const PREVIEW_DURATION_S: u32 = 30;
/// Largest Deezer page read.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// Deezer on the wire: an error object inside a 200 is read by its code (see
/// `classify`), and a 429 that names no wait rests five seconds (v2's
/// default).
pub static DEEZER_SPEC: Spec = Spec {
    source: DEEZER,
    health_key: DEEZER,
    timeout: Duration::from_secs(15),
    max_body: MAX_BODY_BYTES,
    retry_after_default: Backoff::Fixed(Duration::from_secs(5)),
    retry_after_cap: MAX_RETRY_AFTER,
    classify,
};

/// The Deezer hook over the standard rules. Deezer answers an error with a
/// 200 and an error object, so that body is read by its code first:
///
/// - 800, no data: [`ProviderError::NotFound`], which the lookups read as
///   absence. Seen for an unknown album id on 2026-10-20.
/// - 4, quota: [`ProviderError::RateLimited`], which the executor paces and
///   retries like a 429, never absence. Deezer's quota code, not yet seen on
///   the wire.
/// - any other code: [`ProviderError::BadRequest`] with the status, never
///   retried. Only 800 has been seen on the wire, so the other codes are
///   refused rather than mapped to a guessed meaning.
///
/// A 429 or a 5xx is standard, whatever its body says.
fn classify(answer: &Answer<'_>) -> Result<(), ProviderError> {
    let status = answer.reply.status();
    if status == 429 || status >= 500 {
        return standard_classify(answer);
    }
    let Ok(envelope) = answer.reply.json::<DeezerFault>() else {
        return standard_classify(answer);
    };
    let Some(error) = envelope.error else {
        return standard_classify(answer);
    };
    Err(match error.code {
        800 => ProviderError::NotFound { provider: DEEZER },
        4 => ProviderError::RateLimited {
            provider: DEEZER,
            retry_after: answer.retry_after(),
        },
        _ => ProviderError::BadRequest {
            provider: DEEZER,
            status,
        },
    })
}

// ---------------------------------------------------------------------------
// Normalized model
// ---------------------------------------------------------------------------

/// One provider-neutral 30s preview.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PreviewTrack {
    /// Track title.
    pub title: String,
    /// Track artist.
    pub artist_name: String,
    /// Short-lived preview URL: resolve just-in-time, never cache long-term.
    pub preview_url: String,
    /// Preview length in seconds (always 30 from these legs).
    pub duration_s: Option<u32>,
    /// Position within the album, when known.
    pub position: Option<i64>,
}

// ---------------------------------------------------------------------------
// Pure matching (ports of the v2 module-level helpers)
// ---------------------------------------------------------------------------

/// Loose comparison form: lowercase, ASCII alphanumerics only (casefold,
/// then strip everything outside `a-z0-9`).
pub fn norm(value: &str) -> String {
    value
        .chars()
        .flat_map(|c| c.to_lowercase())
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Loose name match: both names must be non-empty, and
/// either normalized form must contain the other. Note the faithful edge:
/// when the wanted name normalizes to something non-empty but the got name
/// normalizes to nothing, the empty string is "contained" and the match
/// succeeds, exactly as the v2 substring check behaves.
pub fn names_match(wanted: &str, got: &str) -> bool {
    if wanted.is_empty() || got.is_empty() {
        return false;
    }
    let a = norm(wanted);
    let b = norm(got);
    !a.is_empty() && (a.contains(&b) || b.contains(&a))
}

// ---------------------------------------------------------------------------
// Barcodes
// ---------------------------------------------------------------------------

/// A barcode as a 14-digit GTIN, padded with leading zeros, or `None` when it
/// is not one: digits only, a GTIN length (8, 12, 13 or 14), at least eight
/// significant digits and a valid check digit. Surrounding whitespace is
/// ignored.
fn gtin14(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if !matches!(raw.len(), 8 | 12 | 13 | 14) || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let gtin = format!("{raw:0>14}");
    if gtin.trim_start_matches('0').len() < 8 {
        return None;
    }
    // The check digit is the last one. Counting from the left, the digits
    // before it weigh 3, 1, 3, and so on; with the check digit added, the
    // weighted sum is a multiple of 10.
    let digits: Vec<u32> = gtin.bytes().map(|b| u32::from(b - b'0')).collect();
    let (check, body) = digits.split_last()?;
    let sum: u32 = body
        .iter()
        .enumerate()
        .map(|(at, digit)| {
            let weight = if at % 2 == 0 { 3 } else { 1 };
            digit * weight
        })
        .sum();
    ((sum + check) % 10 == 0).then_some(gtin)
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Preview client across the Deezer and iTunes legs. Built once with the
/// providers; cheap to clone.
#[derive(Debug, Clone)]
pub struct PreviewClient {
    deezer: Executor,
    deezer_base: Option<Url>,
    deezer_links: Option<Url>,
    itunes: ITunesClient,
}

/// False for a picture URL with an empty image hash
/// (`/images/artist//1000x1000-...`), the form Deezer's default silhouette is
/// reported to take. Not verified: no artist without a picture was found to
/// record.
fn has_picture(url: &str) -> bool {
    Url::parse(url).is_ok_and(|url| {
        url.path_segments().is_some_and(|mut segments| {
            segments.next() == Some("images")
                && segments.next() == Some("artist")
                && segments.next().is_some_and(|hash| !hash.is_empty())
        })
    })
}

/// The title a Deezer track shows: the short title when it has one.
fn deezer_title(hit: &Track) -> String {
    if hit.title_short.is_empty() {
        hit.title.clone()
    } else {
        hit.title_short.clone()
    }
}

/// The preview URL of a Deezer track, when Deezer has one.
fn deezer_preview(hit: &Track) -> Option<&str> {
    hit.preview.as_ref().map(WebAddress::as_str)
}

impl PreviewClient {
    /// The client sending Deezer requests through `deezer` to the origin
    /// at `deezer_base` and the short links at `deezer_links`, and iTunes
    /// requests through `itunes`.
    #[must_use]
    pub fn new(
        deezer: Executor,
        deezer_base: &str,
        deezer_links: &str,
        itunes: ITunesClient,
    ) -> Self {
        Self {
            deezer,
            deezer_base: parse_base(deezer_base, DEEZER),
            deezer_links: parse_base(deezer_links, DEEZER),
            itunes,
        }
    }

    /// One Deezer GET of `segments` under the origin, or `None` on a 404.
    async fn deezer_get(
        &self,
        segments: &[&str],
        query: &[(&str, &str)],
        priority: RequestPriority,
    ) -> Result<Option<Reply>, ProviderError> {
        let base = self
            .deezer_base
            .as_ref()
            .ok_or_else(|| unusable_base(DEEZER))?;
        let request = Request::get(base, segments, query).map_err(|_| unusable_base(DEEZER))?;
        match self.deezer.send(request, priority).await {
            Ok(reply) => Ok(Some(reply)),
            Err(ProviderError::NotFound { .. }) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// The picture URL of Deezer artist `artist_id`: its 1000 px picture,
    /// `None` when Deezer has no such artist, answers another id, or has no
    /// picture for it.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn deezer_artist_picture(
        &self,
        artist_id: u64,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        let id = artist_id.to_string();
        let Some(reply) = self
            .deezer_get(&["artist", id.as_str()], &[], priority)
            .await?
        else {
            return Ok(None);
        };
        let artist: DeezerArtist = decode(&reply)?;
        if u64::try_from(artist.id).ok() != Some(artist_id) {
            return Ok(None);
        }
        Ok(artist.picture_xl.filter(|url| has_picture(url)))
    }

    /// Deezer track `track_id`, `None` when Deezer does not hold it or
    /// answers another id.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn deezer_track(
        &self,
        track_id: u64,
        priority: RequestPriority,
    ) -> Result<Option<SongLookup>, ProviderError> {
        let id = track_id.to_string();
        let Some(reply) = self
            .deezer_get(&["track", id.as_str()], &[], priority)
            .await?
        else {
            return Ok(None);
        };
        let track: SongLookup = decode(&reply)?;
        Ok((u64::try_from(track.id).ok() == Some(track_id)).then_some(track))
    }

    /// The Deezer id of the album whose barcode is `barcode`. `None` when no
    /// answer names an album whose barcode equals it once both are padded to
    /// 14 digits, or when `barcode` is not a GTIN. When the GTIN-13 starts with
    /// 0, the 12-digit form is asked first and the GTIN-13 form second; otherwise
    /// only the GTIN-13 form is asked. The 14-digit form is never asked: Deezer
    /// answers it with code 800. A GTIN-14 whose first (indicator) digit is not
    /// 0 names a pack of items, never an album, so nothing is asked for it.
    ///
    /// # Errors
    ///
    /// The typed failure of a call or of its answer, quota included.
    pub async fn deezer_album_by_upc(
        &self,
        barcode: &str,
        priority: RequestPriority,
    ) -> Result<Option<u64>, ProviderError> {
        let Some(gtin) = gtin14(barcode).filter(|gtin| gtin.starts_with('0')) else {
            return Ok(None);
        };
        let gtin13 = &gtin[1..];
        // Deezer answers the leading-zero GTIN-13 of a UPC-A with code 800
        // (probed), so the 12-digit form is asked first.
        let forms = if gtin13.starts_with('0') {
            vec![&gtin[2..], gtin13]
        } else {
            vec![gtin13]
        };
        for form in forms {
            let segment = format!("upc:{form}");
            let Some(reply) = self
                .deezer_get(&["album", segment.as_str()], &[], priority)
                .await?
            else {
                continue;
            };
            let album: AlbumByUpc = decode(&reply)?;
            if format!("{:0>14}", album.upc) == gtin {
                return Ok(Some(album.id));
            }
        }
        Ok(None)
    }

    /// Where Deezer short link `code` (`link.deezer.com/s/{code}`) leads:
    /// the address in its redirect's `dest`, else the redirect itself;
    /// `None` when it does not redirect. The redirect is read, never
    /// followed.
    ///
    /// Checked on 2026-11-20: a short link answers a 301 to
    /// `link.deezer.com/?dest=<the deezer.com address>&...`.
    ///
    /// # Errors
    ///
    /// The typed failure of the call.
    pub async fn deezer_short_link(
        &self,
        code: &str,
        priority: RequestPriority,
    ) -> Result<Option<String>, ProviderError> {
        let base = self
            .deezer_links
            .as_ref()
            .ok_or_else(|| unusable_base(DEEZER))?;
        let request = Request::get(base, &["s", code], &[])
            .map_err(|_| unusable_base(DEEZER))?
            .redirects(Redirects::Return);
        let reply = match self.deezer.send(request, priority).await {
            Ok(reply) => reply,
            Err(ProviderError::NotFound { .. }) => return Ok(None),
            Err(error) => return Err(error),
        };
        if !(300..400).contains(&reply.status()) {
            return Ok(None);
        }
        let Some(location) = reply.header("location").and_then(|at| base.join(at).ok()) else {
            return Ok(None);
        };
        let dest = location
            .query_pairs()
            .find(|(key, _)| key == "dest")
            .map(|(_, value)| value.into_owned());
        Ok(Some(dest.unwrap_or_else(|| location.to_string())))
    }

    /// One track preview via Deezer's field-scoped search.
    /// The first hit carrying a preview URL wins.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn deezer_track_preview(
        &self,
        artist: &str,
        track: &str,
        priority: RequestPriority,
    ) -> Result<Option<PreviewTrack>, ProviderError> {
        let query = format!(r#"artist:"{artist}" track:"{track}""#);
        let Some(reply) = self
            .deezer_get(
                &["search"],
                &[("q", query.as_str()), ("limit", "3")],
                priority,
            )
            .await?
        else {
            return Ok(None);
        };
        let page: TrackSearch = decode(&reply)?;
        Ok(page.data.iter().find_map(|hit| {
            deezer_preview(hit).map(|preview_url| PreviewTrack {
                title: deezer_title(hit),
                artist_name: hit
                    .artist
                    .as_ref()
                    .map(|artist| artist.name.clone())
                    .unwrap_or_else(|| artist.to_owned()),
                preview_url: preview_url.to_owned(),
                duration_s: Some(PREVIEW_DURATION_S),
                position: hit.track_position,
            })
        }))
    }

    /// Ordered album previews via Deezer. The
    /// album whose artist loosely matches wins; when no artist matches but
    /// hits exist, the top hit is used (the artist field is sometimes
    /// missing or odd). Tracks without previews are skipped, positions fall
    /// back to list order, and the tracks page over-fetches
    /// (`max(limit, 4)`) so skipping preview-less rows still fills `limit`.
    ///
    /// # Errors
    ///
    /// The typed failure of either call or of its answer.
    pub async fn deezer_album_tracks(
        &self,
        artist: &str,
        album: &str,
        limit: usize,
        priority: RequestPriority,
    ) -> Result<Vec<PreviewTrack>, ProviderError> {
        let query = format!(r#"artist:"{artist}" album:"{album}""#);
        let Some(reply) = self
            .deezer_get(
                &["search", "album"],
                &[("q", query.as_str()), ("limit", "3")],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let albums: AlbumSearch = decode(&reply)?;
        let album_id = albums
            .data
            .iter()
            .find(|hit| {
                let hit_artist = hit.artist.as_ref().map(|a| a.name.as_str()).unwrap_or("");
                names_match(artist, hit_artist)
            })
            .or_else(|| albums.data.first())
            .map(|hit| hit.id);
        let Some(album_id) = album_id else {
            return Ok(Vec::new());
        };
        let album_id = album_id.to_string();
        let rows = limit.max(4).to_string();
        let Some(reply) = self
            .deezer_get(
                &["album", album_id.as_str(), "tracks"],
                &[("limit", rows.as_str())],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let tracks: AlbumTracks = decode(&reply)?;
        let mut results = Vec::new();
        for hit in &tracks.data {
            let Some(preview_url) = deezer_preview(hit) else {
                continue;
            };
            let position = hit.track_position.or(Some(results.len() as i64 + 1));
            results.push(PreviewTrack {
                title: deezer_title(hit),
                artist_name: hit
                    .artist
                    .as_ref()
                    .map(|artist| artist.name.clone())
                    .unwrap_or_else(|| artist.to_owned()),
                preview_url: preview_url.to_owned(),
                duration_s: Some(PREVIEW_DURATION_S),
                position,
            });
            if results.len() >= limit {
                break;
            }
        }
        Ok(results)
    }

    /// One iTunes song search, or `None` on a 404.
    async fn itunes_songs(
        &self,
        term: &str,
        limit: &str,
        priority: RequestPriority,
    ) -> Result<Option<SongSearch>, ProviderError> {
        let Some(reply) = self
            .itunes
            .search(
                &[("term", term), ("entity", "song"), ("limit", limit)],
                priority,
            )
            .await?
        else {
            return Ok(None);
        };
        decode(&reply).map(Some)
    }

    /// One track preview via iTunes. iTunes'
    /// top hit can be a cover, so the artist is verified with the loose
    /// name match before a hit is trusted.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn itunes_track_preview(
        &self,
        artist: &str,
        track: &str,
        priority: RequestPriority,
    ) -> Result<Option<PreviewTrack>, ProviderError> {
        let term = format!("{artist} {track}");
        let Some(page) = self.itunes_songs(&term, "5", priority).await? else {
            return Ok(None);
        };
        Ok(page
            .results
            .iter()
            .find_map(|hit| {
                let preview_url = hit.preview_url.as_ref()?;
                names_match(artist, &hit.artist_name).then_some((hit, preview_url))
            })
            .map(|(hit, preview_url)| PreviewTrack {
                title: hit.track_name.clone(),
                artist_name: hit.artist_name.clone(),
                preview_url: preview_url.to_string(),
                duration_s: Some(PREVIEW_DURATION_S),
                position: None,
            }))
    }

    /// Ordered album previews via iTunes. Both
    /// the artist and the collection must loosely match, positions fall
    /// back to list order, and the results sort by position.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn itunes_album_tracks(
        &self,
        artist: &str,
        album: &str,
        limit: usize,
        priority: RequestPriority,
    ) -> Result<Vec<PreviewTrack>, ProviderError> {
        let term = format!("{artist} {album}");
        let Some(page) = self.itunes_songs(&term, "25", priority).await? else {
            return Ok(Vec::new());
        };
        let mut results = Vec::new();
        for hit in &page.results {
            let Some(preview_url) = hit.preview_url.as_ref() else {
                continue;
            };
            if !names_match(artist, &hit.artist_name) {
                continue;
            }
            if !names_match(album, &hit.collection_name) {
                continue;
            }
            let position = hit.track_number.or(Some(results.len() as i64 + 1));
            results.push(PreviewTrack {
                title: hit.track_name.clone(),
                artist_name: hit.artist_name.clone(),
                preview_url: preview_url.to_string(),
                duration_s: Some(PREVIEW_DURATION_S),
                position,
            });
            if results.len() >= limit {
                break;
            }
        }
        results.sort_by_key(|track| track.position.unwrap_or(0));
        Ok(results)
    }

    /// An artist's popular tracks via Deezer search. The search
    /// over-fetches (`max(limit * 2, 10)`), non-matching artists are
    /// filtered out, and titles dedupe case-insensitively. A failed fetch is
    /// an error, so a caller can tell it from an artist with no previews.
    ///
    /// # Errors
    ///
    /// The typed failure of the call or of its answer.
    pub async fn artist_top_tracks_leg(
        &self,
        artist: &str,
        limit: usize,
        priority: RequestPriority,
    ) -> Result<Vec<PreviewTrack>, ProviderError> {
        let query = format!(r#"artist:"{artist}""#);
        let rows = (limit * 2).max(10).to_string();
        let Some(reply) = self
            .deezer_get(
                &["search"],
                &[("q", query.as_str()), ("limit", rows.as_str())],
                priority,
            )
            .await?
        else {
            return Ok(Vec::new());
        };
        let page: TrackSearch = decode(&reply)?;
        let mut results = Vec::new();
        let mut seen: Vec<String> = Vec::new();
        for hit in &page.data {
            let hit_artist = hit.artist.as_ref().map(|a| a.name.as_str()).unwrap_or("");
            if !names_match(artist, hit_artist) {
                continue;
            }
            let title = deezer_title(hit);
            let key = title.to_lowercase();
            if seen.contains(&key) {
                continue;
            }
            seen.push(key);
            results.push(PreviewTrack {
                title,
                artist_name: if hit_artist.is_empty() {
                    artist.to_owned()
                } else {
                    hit_artist.to_owned()
                },
                preview_url: web_address::or_empty(hit.preview.clone()),
                duration_s: Some(PREVIEW_DURATION_S),
                position: None,
            });
            if results.len() >= limit {
                break;
            }
        }
        Ok(results)
    }

    /// A single 30s preview with Deezer-to-iTunes fallback.
    /// Any failure on a leg falls through to the next;
    /// when both legs fail the lookup is absence. Each leg's failure is
    /// recorded by the call that met it.
    pub async fn get_track_preview(
        &self,
        artist: &str,
        track: &str,
        priority: RequestPriority,
    ) -> (Option<PreviewTrack>, Option<&'static str>) {
        if let Ok(Some(found)) = self.deezer_track_preview(artist, track, priority).await {
            return (Some(found), Some("deezer"));
        }
        match self.itunes_track_preview(artist, track, priority).await {
            Ok(Some(found)) => (Some(found), Some("itunes")),
            _ => (None, None),
        }
    }

    /// Ordered 30s samples of an album's first tracks with Deezer-to-iTunes
    /// fallback.
    pub async fn get_album_preview_tracks(
        &self,
        artist: &str,
        album: &str,
        limit: usize,
        priority: RequestPriority,
    ) -> (Vec<PreviewTrack>, Option<&'static str>) {
        match self
            .deezer_album_tracks(artist, album, limit, priority)
            .await
        {
            Ok(tracks) if !tracks.is_empty() => return (tracks, Some("deezer")),
            _ => {}
        }
        match self
            .itunes_album_tracks(artist, album, limit, priority)
            .await
        {
            Ok(tracks) if !tracks.is_empty() => (tracks, Some("itunes")),
            _ => (Vec::new(), None),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::gtin14;
    use crate::test_support::ScriptedWire;
    use crate::{ProviderEndpoints, Providers, RequestPriority};

    const BASE: &str = "https://api.deezer.com";
    /// In Rainbows, UPC 634904032463.
    const IN_RAINBOWS: &str = include_str!("../../tests/fixtures/deezer/album_upc_12.json");
    /// Mellon Collie (Remastered), EAN 5099997855354.
    const MELLON_COLLIE: &str = include_str!("../../tests/fixtures/deezer/album_upc_13.json");
    const UNKNOWN_14: &str = include_str!("../../tests/fixtures/deezer/album_upc_14.json");
    /// In Rainbows' GTIN-13 form with a leading zero: code 800.
    const LEADING_ZERO_800: &str =
        include_str!("../../tests/fixtures/deezer/album_upc_13_leading_zero.json");
    const UNKNOWN: &str = include_str!("../../tests/fixtures/deezer/album_upc_unknown.json");
    /// Synthetic: no quota answer has been recorded, so this body is written by
    /// hand. Only its code is read; Deezer's quota code is 4.
    const QUOTA: &str = r#"{"error":{"code":4}}"#;

    /// Providers with Deezer at [`BASE`], answering from `wire`.
    fn providers(wire: &Arc<ScriptedWire>) -> Providers {
        Providers::unpaced_at(
            wire.clone(),
            ProviderEndpoints {
                deezer: BASE.to_owned(),
                ..ProviderEndpoints::closed()
            },
        )
    }

    /// The paths the wire was asked for, in order.
    fn asked(wire: &ScriptedWire) -> Vec<String> {
        wire.sent()
            .iter()
            .map(|sent| sent.url.path().to_owned())
            .collect()
    }

    #[test]
    fn barcode_normaliser_table() {
        let cases = [
            ("634904032463", Some("00634904032463")),
            ("0634904032463", Some("00634904032463")),
            ("00634904032463", Some("00634904032463")),
            ("5099997855354", Some("05099997855354")),
            ("05099997855354", Some("05099997855354")),
            (" 634904032463 ", Some("00634904032463")),
            ("634904032464", None),
            ("5099997855355", None),
            ("000000000017", None),
            ("00000017", None),
            ("63490403246X", None),
            ("6349 0403 2463", None),
            ("123456789", None),
            ("123456789012345", None),
            ("", None),
        ];
        for (barcode, expected) in cases {
            assert_eq!(gtin14(barcode).as_deref(), expected, "barcode {barcode:?}");
        }
    }

    /// A 14-digit barcode asks only its GTIN-13 form. A 12-digit barcode asks its
    /// 12-digit form first, and the hit ends the lookup. The leading-zero GTIN-13
    /// form is scripted with its recorded 800 and must not be asked, so a lookup
    /// that asked it would show in the request list.
    #[tokio::test]
    async fn barcode_lookup_takes_the_album_whose_barcode_matches() {
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{BASE}/album/upc:5099997855354"),
            200,
            &[],
            MELLON_COLLIE,
        );
        let found = providers(&wire)
            .previews
            .deezer_album_by_upc("05099997855354", RequestPriority::UserInitiated)
            .await;
        assert_eq!(found, Ok(Some(6158273)));
        assert_eq!(asked(&wire), ["/album/upc:5099997855354"]);

        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{BASE}/album/upc:0634904032463"),
            200,
            &[],
            LEADING_ZERO_800,
        );
        wire.answer(
            &format!("{BASE}/album/upc:634904032463"),
            200,
            &[],
            IN_RAINBOWS,
        );
        let found = providers(&wire)
            .previews
            .deezer_album_by_upc("634904032463", RequestPriority::UserInitiated)
            .await;
        assert_eq!(found, Ok(Some(14880659)));
        assert_eq!(asked(&wire), ["/album/upc:634904032463"]);
    }

    /// Code 800 and an answer naming another barcode are both absence. The
    /// 14-digit form is scripted too, so a lookup that asked it would show. In
    /// the second case the 12-digit answer is a scripted mismatch (another
    /// album's recorded answer, given at the path asked); the leading-zero
    /// answer is recorded.
    #[tokio::test]
    async fn barcode_lookup_is_absence_for_code_800_and_for_a_miss() {
        let wire = Arc::new(ScriptedWire::new());
        wire.answer(
            &format!("{BASE}/album/upc:05099997855354"),
            200,
            &[],
            UNKNOWN_14,
        );
        wire.answer(
            &format!("{BASE}/album/upc:5099997855354"),
            200,
            &[],
            UNKNOWN,
        );
        let found = providers(&wire)
            .previews
            .deezer_album_by_upc("05099997855354", RequestPriority::UserInitiated)
            .await;
        assert_eq!(found, Ok(None));
        assert_eq!(asked(&wire), ["/album/upc:5099997855354"]);

        let wire = Arc::new(ScriptedWire::new());
        // The mismatch is scripted: Mellon Collie's recorded answer, given at
        // the 12-digit path that In Rainbows' barcode asks first.
        wire.answer(
            &format!("{BASE}/album/upc:634904032463"),
            200,
            &[],
            MELLON_COLLIE,
        );
        wire.answer(
            &format!("{BASE}/album/upc:0634904032463"),
            200,
            &[],
            LEADING_ZERO_800,
        );
        let found = providers(&wire)
            .previews
            .deezer_album_by_upc("634904032463", RequestPriority::UserInitiated)
            .await;
        assert_eq!(found, Ok(None));
        assert_eq!(
            asked(&wire),
            ["/album/upc:634904032463", "/album/upc:0634904032463"]
        );

        // A GTIN-14 with indicator digit 1 is a pack of In Rainbows, not the
        // album: nothing is asked.
        let wire = Arc::new(ScriptedWire::new());
        let found = providers(&wire)
            .previews
            .deezer_album_by_upc("10634904032460", RequestPriority::UserInitiated)
            .await;
        assert_eq!(found, Ok(None));
        assert!(asked(&wire).is_empty());
    }

    /// A quota answer is a failure the executor paces and retries, never
    /// absence: the retry's album is the answer.
    #[tokio::test(start_paused = true)]
    async fn barcode_lookup_paces_a_quota_answer_and_retries_it() {
        let wire = Arc::new(ScriptedWire::new());
        let url = format!("{BASE}/album/upc:5099997855354");
        wire.answer(&url, 200, &[], QUOTA);
        wire.answer(&url, 200, &[], MELLON_COLLIE);
        let found = providers(&wire)
            .previews
            .deezer_album_by_upc("5099997855354", RequestPriority::BackgroundSync)
            .await;
        assert_eq!(found, Ok(Some(6158273)));
        assert_eq!(
            asked(&wire),
            ["/album/upc:5099997855354", "/album/upc:5099997855354"]
        );
    }
}
